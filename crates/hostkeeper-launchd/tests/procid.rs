//! The launchd process probe against fake pid lookups and real files in a scratch directory.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_launchd::{
    FileIdentity, LaunchdProcessProbe, PidInfo, PidInfoError, StdFileIdentity,
};
use std::time::Duration;

use hostkeeper_observe::fake::{FakeExpectedExe, FakeHealthClient, FakeServiceManager};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, ExeStat, ObserveError, ObserveOptions, Observer,
    Ports as ObservePorts, ProcessIdentity, ProcessProbe, ServiceInput, ServiceState,
    UnobservedReason, VersionView,
};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// A pid lookup with a fixed path, a start time the test can change, and the (device, inode)
/// of the image the process runs.
#[derive(Clone)]
struct FakePids {
    path: Result<PathBuf, String>,
    start: Result<Arc<AtomicU64>, String>,
    image: Result<(u64, u64), String>,
}

impl FakePids {
    /// A process running the file at `path` as it is now: its image is the file's real
    /// (device, inode), or (1, 2) when there is no file there.
    fn at(path: &Path) -> FakePids {
        let image = fs::metadata(path).map_or((1, 2), |meta| (meta.dev(), meta.ino()));
        FakePids {
            path: Ok(path.to_path_buf()),
            start: Ok(Arc::new(AtomicU64::new(1))),
            image: Ok(image),
        }
    }

    /// The same process, running the image `image`.
    fn running(self, image: Result<(u64, u64), String>) -> FakePids {
        FakePids { image, ..self }
    }
}

impl PidInfo for FakePids {
    fn path(&self, pid: u32) -> Result<PathBuf, PidInfoError> {
        self.path.clone().map_err(|why| PidInfoError { pid, why })
    }

    fn start_time(&self, pid: u32) -> Result<u64, PidInfoError> {
        match &self.start {
            Ok(start) => Ok(start.load(Ordering::SeqCst)),
            Err(why) => Err(PidInfoError {
                pid,
                why: why.clone(),
            }),
        }
    }

    fn image(&self, pid: u32) -> Result<(u64, u64), PidInfoError> {
        self.image.clone().map_err(|why| PidInfoError { pid, why })
    }
}

/// A file identity that answers fixed pairs; `None` answers with an error.
struct FakeFiles {
    by_path: Option<(u64, u64)>,
    by_file: Option<(u64, u64)>,
}

fn answer(pair: Option<(u64, u64)>, side: &str) -> io::Result<(u64, u64)> {
    pair.ok_or_else(|| io::Error::other(format!("no identity by {side}")))
}

impl FileIdentity for FakeFiles {
    fn device_inode(&self, _path: &Path) -> io::Result<(u64, u64)> {
        answer(self.by_path, "path")
    }

    fn device_inode_of(&self, _file: &File) -> io::Result<(u64, u64)> {
        answer(self.by_file, "descriptor")
    }
}

fn same_files() -> FakeFiles {
    FakeFiles {
        by_path: Some((1, 2)),
        by_file: Some((1, 2)),
    }
}

fn process(pid: u32, why: &str) -> ObserveError {
    ObserveError::Process {
        pid,
        why: why.to_string(),
    }
}

/// Catches a digest taken of anything but the bytes of the file the pid's path names.
#[test]
fn the_exe_digest_hashes_the_file_proc_pidpath_names() {
    let dir = scratch("procid-digest");
    let exe = dir.path().join("exe");
    let bytes = vec![7u8; 150_000];
    fs::write(&exe, &bytes).expect("write exe");
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), StdFileIdentity);
    let expected: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(probe.exe_digest(10), Ok(expected));
}

/// Catches a digest accepted when the path now names another file than the one that was read.
#[test]
fn a_path_replaced_between_path_lookup_and_open_is_an_error() {
    let dir = scratch("procid-replaced");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"x").expect("write exe");
    let files = FakeFiles {
        by_path: Some((1, 2)),
        by_file: Some((1, 3)),
    };
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe).running(Ok((1, 2))), files);
    let why = format!("{} was replaced while it was read", exe.display());
    assert_eq!(probe.exe_digest(10), Err(process(10, &why)));
}

/// Catches a missing executable being reported as a digest instead of an error.
#[test]
fn a_path_that_no_longer_exists_is_an_error() {
    let dir = scratch("procid-gone");
    let exe = dir.path().join("gone");
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), same_files());
    let io_error = File::open(&exe).expect_err("missing file");
    let why = format!("{}: {io_error}", exe.display());
    assert_eq!(probe.exe_digest(10), Err(process(10, &why)));
    let real = LaunchdProcessProbe::new(FakePids::at(&exe), StdFileIdentity);
    let io_error = fs::metadata(&exe).expect_err("missing file");
    let why = format!("{}: {io_error}", exe.display());
    assert_eq!(real.exe_digest(10), Err(process(10, &why)));
}

/// Catches a read failure of the open file being swallowed into a digest.
#[test]
fn a_file_that_cannot_be_read_is_an_error() {
    let dir = scratch("procid-unreadable");
    let pids = FakePids::at(dir.path()).running(Ok((1, 2)));
    let probe = LaunchdProcessProbe::new(pids, same_files());
    let io_error = File::open(dir.path())
        .and_then(|f| {
            let mut buf = [0u8; 1];
            std::os::unix::fs::FileExt::read_at(&f, &mut buf, 0)
        })
        .expect_err("a directory cannot be read");
    let why = format!("{}: {io_error}", dir.path().display());
    assert_eq!(probe.exe_digest(10), Err(process(10, &why)));
}

/// Catches a pid and start time not forming the identity, so a reused pid looks the same.
#[test]
fn identity_is_pid_and_start_time() {
    let pids = FakePids::at(Path::new("/unused"));
    let start = pids.start.clone().expect("start");
    let probe = LaunchdProcessProbe::new(pids, StdFileIdentity);
    let first = probe.identity(5).expect("first read");
    assert_eq!(
        first,
        ProcessIdentity {
            pid: 5,
            start_time: 1
        }
    );
    start.store(9, Ordering::SeqCst);
    let second = probe.identity(5).expect("second read");
    assert_eq!(
        second,
        ProcessIdentity {
            pid: 5,
            start_time: 9
        }
    );
    assert_ne!(first, second);
}

/// Catches a failed start-time lookup being reported as a zero start time.
#[test]
fn a_start_time_error_is_an_error() {
    let pids = FakePids {
        path: Ok(PathBuf::new()),
        start: Err("no such process".to_string()),
        image: Err(String::new()),
    };
    let probe = LaunchdProcessProbe::new(pids, StdFileIdentity);
    assert_eq!(probe.identity(5), Err(process(5, "no such process")));
}

/// Catches a failed path lookup being reported as a stat or a digest of something else.
#[test]
fn a_path_lookup_error_is_an_error_for_stat_and_digest() {
    let pids = FakePids {
        path: Err("denied".to_string()),
        start: Err(String::new()),
        image: Err(String::new()),
    };
    let probe = LaunchdProcessProbe::new(pids, StdFileIdentity);
    assert_eq!(probe.exe_stat(6), Err(process(6, "denied")));
    assert_eq!(probe.exe_digest(6), Err(process(6, "denied")));
}

/// Catches an exe stat that reports another size or modification time than the file's.
#[test]
fn exe_stat_reports_size_and_mtime_of_the_file() {
    let dir = scratch("procid-stat");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"twelve bytes").expect("write exe");
    let meta = fs::metadata(&exe).expect("metadata");
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), StdFileIdentity);
    let want = ExeStat {
        size: 12,
        mtime_secs: meta.mtime(),
        mtime_nanos: meta.mtime_nsec(),
    };
    assert_eq!(probe.exe_stat(3), Ok(want));
}

/// Catches an exe stat of a missing file succeeding.
#[test]
fn exe_stat_of_a_missing_file_is_an_error() {
    let dir = scratch("procid-stat-gone");
    let exe = dir.path().join("gone");
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), StdFileIdentity);
    let io_error = fs::metadata(&exe).expect_err("missing file");
    let why = format!("{}: {io_error}", exe.display());
    assert_eq!(probe.exe_stat(3), Err(process(3, &why)));
}

/// Catches an identity failure on the path side being ignored.
#[test]
fn a_file_identity_error_on_the_path_side_is_an_error() {
    let dir = scratch("procid-path-side");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"x").expect("write exe");
    let files = FakeFiles {
        by_path: None,
        by_file: Some((1, 2)),
    };
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), files);
    let why = format!("{}: no identity by path", exe.display());
    assert_eq!(probe.exe_digest(4), Err(process(4, &why)));
}

/// Catches an identity failure on the descriptor side being ignored.
#[test]
fn a_file_identity_error_on_the_descriptor_side_is_an_error() {
    let dir = scratch("procid-file-side");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"x").expect("write exe");
    let files = FakeFiles {
        by_path: Some((1, 2)),
        by_file: None,
    };
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe).running(Ok((1, 2))), files);
    let why = format!("{}: no identity by descriptor", exe.display());
    assert_eq!(probe.exe_digest(4), Err(process(4, &why)));
}

/// Catches the std identity reading different values by path and by descriptor for one file.
#[test]
fn std_file_identity_agrees_between_path_and_open_file() {
    let dir = scratch("procid-std");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"x").expect("write exe");
    let meta = fs::metadata(&exe).expect("metadata");
    let by_path = StdFileIdentity.device_inode(&exe).expect("by path");
    let file = File::open(&exe).expect("open");
    let by_file = StdFileIdentity.device_inode_of(&file).expect("by file");
    assert_eq!(by_path, (meta.dev(), meta.ino()));
    assert_eq!(by_path, by_file);
    assert!(
        StdFileIdentity
            .device_inode(&dir.path().join("gone"))
            .is_err()
    );
}

/// Catches the pid lookup error text losing its pid and reason.
#[test]
fn a_pid_info_error_names_the_pid_and_reason() {
    let error = PidInfoError {
        pid: 8,
        why: "gone".to_string(),
    };
    assert_eq!(error.to_string(), "process 8: gone");
}

/// The error text of a path whose file is not the image the process runs.
fn not_the_image(path: &Path, at_path: (u64, u64), image: (u64, u64)) -> String {
    format!(
        "{} is not the file the process runs: the path is device {} inode {}, the process runs device {} inode {}",
        path.display(),
        at_path.0,
        at_path.1,
        image.0,
        image.1
    )
}

/// Catches a stat or a digest taken of a file renamed over the path while the process runs: the
/// path's (device, inode) differs from the image's, so both are errors naming the two, and the
/// new file is never read.
#[test]
fn a_path_renamed_over_while_the_process_runs_is_an_error_for_stat_and_digest() {
    let dir = scratch("procid-renamed-over");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"old").expect("write exe");
    let pids = FakePids::at(&exe);
    let old = pids.image.clone().expect("old image");
    let new = dir.path().join("new");
    fs::write(&new, b"new").expect("write new");
    fs::rename(&new, &exe).expect("rename over");
    let meta = fs::metadata(&exe).expect("metadata");
    let at_path = (meta.dev(), meta.ino());
    assert_ne!(at_path, old);
    let probe = LaunchdProcessProbe::new(pids, StdFileIdentity);
    let why = not_the_image(&exe, at_path, old);
    assert_eq!(probe.exe_stat(12), Err(process(12, &why)));
    assert_eq!(probe.exe_digest(12), Err(process(12, &why)));
}

/// Catches an image lookup failure being ignored: neither a stat nor a digest is given without
/// knowing what the process runs.
#[test]
fn an_image_lookup_error_is_an_error_for_stat_and_digest() {
    let dir = scratch("procid-image-error");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"x").expect("write exe");
    let pids = FakePids::at(&exe).running(Err("no region".to_string()));
    let probe = LaunchdProcessProbe::new(pids, StdFileIdentity);
    assert_eq!(probe.exe_stat(13), Err(process(13, "no region")));
    assert_eq!(probe.exe_digest(13), Err(process(13, "no region")));
}

/// Catches the inode comparison being dropped: through a real observer round, a running unit
/// whose path now names another inode than the one the process runs is `unobserved` with
/// `proc_error`, never a version `Recorded` and confirmed (nor stopped, nor absent), even though
/// the new file's digest is the sealed one.
#[test]
fn a_path_replaced_while_running_is_unobserved_when_the_inode_differs() {
    let dir = scratch("procid-observer-replaced");
    let exe = dir.path().join("exe");
    fs::write(&exe, b"the new release").expect("write exe");
    let meta = fs::metadata(&exe).expect("metadata");
    let at_path = (meta.dev(), meta.ino());
    let old = (at_path.0, at_path.1 + 1);
    let sealed: [u8; 32] = Sha256::digest(b"the new release").into();

    let manager = FakeServiceManager::default();
    manager.set("web-main", ActiveState::Active, Some(77));
    let expected = FakeExpectedExe::default();
    expected.seal("web", "2.0.0", sealed);
    let observer = |image: (u64, u64)| {
        let pids = FakePids::at(&exe).running(Ok(image));
        Observer::new(
            ObservePorts {
                manager: Box::new(manager.clone()),
                process: Box::new(LaunchdProcessProbe::new(pids, StdFileIdentity)),
                health: Box::new(FakeHealthClient::default()),
                expected: Box::new(expected.clone()),
            },
            ObserveOptions::default(),
        )
    };
    let web = ServiceInput {
        service: "web".to_owned(),
        current: Some(Current {
            run: vec!["web-main".to_owned()],
            health_url: None,
        }),
        recorded_version: Some("2.0.0".to_owned()),
    };
    let deadline = Deadline {
        connect: Duration::from_secs(1),
        read: Duration::from_secs(1),
    };

    let replaced = observer(old).observe(&web, deadline);
    assert_eq!(
        replaced.state,
        ServiceState::Unobserved {
            reason: UnobservedReason::ProcError,
            detail: process(77, &not_the_image(&exe, at_path, old)).to_string(),
        }
    );
    assert_eq!(replaced.state.token(), "unobserved");
    assert_eq!(replaced.version, VersionView::NotLive);

    let same = observer(at_path).observe(&web, deadline);
    assert_eq!(same.state, ServiceState::Live);
    assert_eq!(
        same.version,
        VersionView::Recorded {
            version: "2.0.0".to_owned(),
            confirmed: true
        }
    );
}
