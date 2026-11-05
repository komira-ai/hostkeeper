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
use hostkeeper_observe::{ExeStat, ObserveError, ProcessIdentity, ProcessProbe};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// A pid lookup with a fixed path and a start time the test can change.
#[derive(Clone)]
struct FakePids {
    path: Result<PathBuf, String>,
    start: Result<Arc<AtomicU64>, String>,
}

impl FakePids {
    fn at(path: &Path) -> FakePids {
        FakePids {
            path: Ok(path.to_path_buf()),
            start: Ok(Arc::new(AtomicU64::new(1))),
        }
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
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), files);
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
    let probe = LaunchdProcessProbe::new(FakePids::at(dir.path()), same_files());
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
    let probe = LaunchdProcessProbe::new(FakePids::at(&exe), files);
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
