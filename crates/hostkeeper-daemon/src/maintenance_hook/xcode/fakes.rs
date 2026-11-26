//! The fakes of the `xcode` kind's stage and of the `probe` kind's archive check, over a scratch
//! directory standing for the Mac's files: a store that serves one archive by its SHA-256 and can
//! break a read part way, a runner of fake programs that records what it ran, a fake `xip` that
//! makes an app from a fixture version list, and a fake free space. They are shared by the stage's
//! tests ([`super::stage`]) and the archive check's ([`super::check`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_link::{GetError, Got, Limit};
use hostkeeper_package::gh::{Ran, Runner, Spawn};
use hostkeeper_proto::hostkeeper::agent::v1::XcodeEntry;
use hostkeeper_steps::{Clock, Stamp};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

use super::signature::tests::APPLE_REPORT;
use super::stage::{
    ARCHIVE, CODESIGN, CODESIGN_SECS, EXPAND, PKGUTIL, STAGING, Stage, XIP, XIP_SECS,
};
use super::store::{Store, StoreError};
use crate::maintenance_collect::{APPLICATIONS, HostRoot};
use crate::maintenance_hook::KindError;
use crate::maintenance_mac::FreeSpace;
use crate::maintenance_plist::HostPlists;
use crate::testing::FakeClock;

pub(super) const V27: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-27.2.plist");
pub(super) const V26: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-26.3.plist");
pub(super) const NO_BUILD: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-no-build.plist");
pub(super) const INTEGER: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-integer.plist");

/// The archive the store serves: an `xar` header and some bytes.
pub(super) const ARCHIVE_BYTES: &[u8] = b"xar!\x00\x1c\x00\x01 the bytes of an Xcode archive";

pub(super) fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The order's entry of `bytes`: Xcode 27.2 (27B52).
pub(super) fn entry_of(bytes: &[u8]) -> XcodeEntry {
    XcodeEntry {
        version: "27.2".to_owned(),
        build: "27B52".to_owned(),
        size: bytes.len() as u64,
        sha256: sha256(bytes),
    }
}

/// What one read of the fake store does after it gives the bytes it gives.
#[derive(Clone, Debug)]
pub(super) enum Play {
    /// Gives everything from `first` on.
    Whole,
    /// Gives `count` bytes from `first`, then fails with the error.
    Breaks(u64, GetError),
    /// Gives nothing and fails.
    Fails(StoreError),
}

/// A store that holds `bytes` at `/v1/xcode/<sha256 of held>` and plays `reads` in order (then
/// [`Play::Whole`]), recording each `(path, first)` it is asked.
#[derive(Clone)]
pub(super) struct FakeStore {
    pub(super) path: String,
    pub(super) bytes: Vec<u8>,
    pub(super) reads: Arc<Mutex<Vec<Play>>>,
    pub(super) asked: Arc<Mutex<Vec<(String, u64)>>>,
}

impl FakeStore {
    pub(super) fn holding(held: &[u8], bytes: &[u8]) -> FakeStore {
        FakeStore {
            path: format!("/v1/xcode/{}", sha256(held)),
            bytes: bytes.to_vec(),
            reads: Arc::default(),
            asked: Arc::default(),
        }
    }

    pub(super) fn playing(self, reads: Vec<Play>) -> FakeStore {
        *lock(&self.reads) = reads;
        self
    }

    pub(super) fn asked(&self) -> Vec<(String, u64)> {
        lock(&self.asked).clone()
    }
}

impl Store for FakeStore {
    fn get_from(
        &self,
        path: &str,
        first: u64,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, StoreError> {
        lock(&self.asked).push((path.to_owned(), first));
        if path != self.path {
            return Ok(Got::NotFound);
        }
        let read = {
            let mut reads = lock(&self.reads);
            match reads.is_empty() {
                true => Play::Whole,
                false => reads.remove(0),
            }
        };
        let rest = &self.bytes[usize::try_from(first).expect("small")..];
        let length = rest.len() as u64;
        if length > limit.max_bytes {
            return Err(StoreError::Get(GetError::TooLong {
                length,
                max: limit.max_bytes,
            }));
        }
        match read {
            Play::Whole => {
                sink(rest).map_err(|e| StoreError::Get(GetError::Sink(e.kind())))?;
                Ok(Got::Body(length))
            }
            Play::Breaks(count, error) => {
                let count = usize::try_from(count).expect("small");
                sink(&rest[..count]).map_err(|e| StoreError::Get(GetError::Sink(e.kind())))?;
                Err(StoreError::Get(error))
            }
            Play::Fails(error) => Err(error),
        }
    }
}

/// What a fake program does when it is run.
pub(super) type Program = Box<dyn Fn(&Spawn) -> io::Result<Ran> + Send>;

/// A runner of fake programs by path, recording each run and, for `xip`, the archive's bytes
/// when it ran.
#[derive(Clone, Default)]
pub(super) struct FakeRunner {
    pub(super) programs: Arc<Mutex<BTreeMap<PathBuf, Program>>>,
    pub(super) ran: Arc<Mutex<Vec<Spawn>>>,
    pub(super) archives: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeRunner {
    pub(super) fn answers(&self, program: &str, answer: Program) {
        lock(&self.programs).insert(PathBuf::from(program), answer);
    }

    pub(super) fn ran(&self) -> Vec<Spawn> {
        lock(&self.ran).clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        lock(&self.ran).push(spawn.clone());
        if spawn.program == Path::new(XIP) {
            let archive = fs::read(&spawn.args[1]).unwrap_or_default();
            lock(&self.archives).push(archive);
        }
        let programs = lock(&self.programs);
        let program = programs
            .get(&spawn.program)
            .expect("a program the test answers");
        program(spawn)
    }
}

/// A `pkgutil` that exits 0 saying `report` on its standard output.
pub(super) fn reports(report: &str) -> Program {
    let stdout = report.as_bytes().to_vec();
    Box::new(move |_| {
        Ok(Ran {
            code: Some(0),
            stdout: stdout.clone(),
            ..Ran::default()
        })
    })
}

/// A program that exits `code` saying `stderr`.
pub(super) fn exits(code: i32, stderr: &str) -> Program {
    let stderr = stderr.as_bytes().to_vec();
    Box::new(move |_| {
        Ok(Ran {
            code: Some(code),
            timed_out: false,
            stderr: stderr.clone(),
            ..Ran::default()
        })
    })
}

/// An `xip` that makes `<cwd>/<name>` with `plist` as its version list, and exits 0.
pub(super) fn xip_making(name: &'static str, plist: &'static [u8]) -> Program {
    Box::new(move |spawn| {
        let contents = spawn.cwd.join(name).join("Contents");
        fs::create_dir_all(&contents)?;
        fs::write(contents.join("version.plist"), plist)?;
        Ok(Ran {
            code: Some(0),
            ..Ran::default()
        })
    })
}

/// A free space of `bytes`, or one that cannot be read; counts its reads.
#[derive(Clone)]
pub(super) struct FakeSpace {
    pub(super) free: Result<u64, String>,
    pub(super) reads: Arc<Mutex<u32>>,
    /// A directory made, mode 0755, when the space is read: one put in the staging directory's
    /// place between the stage's first look at it and the fetch.
    pub(super) swap_in: Option<PathBuf>,
}

impl FreeSpace for FakeSpace {
    fn free_bytes(&self) -> Result<u64, String> {
        *lock(&self.reads) += 1;
        if let Some(dir) = &self.swap_in {
            fs::create_dir(dir).expect("the swapped-in directory");
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        self.free.clone()
    }
}

/// A Mac with its files under a scratch directory.
pub(super) struct Mac {
    pub(super) root: Scratch,
    pub(super) store: FakeStore,
    pub(super) runner: FakeRunner,
    pub(super) space: FakeSpace,
    pub(super) clock: Arc<dyn Clock>,
}

/// The free space the archive [`ARCHIVE_BYTES`] needs to be staged: itself and its expansion.
pub(super) fn needed() -> u64 {
    ARCHIVE_BYTES.len() as u64 * 4
}

impl Mac {
    /// A Mac whose store holds [`ARCHIVE_BYTES`], whose `xip` makes Xcode 27.2 (27B52), whose
    /// `codesign` accepts it, with exactly the space staging needs free.
    pub(super) fn new(name: &str) -> Mac {
        let runner = FakeRunner::default();
        runner.answers(PKGUTIL, reports(APPLE_REPORT));
        runner.answers(XIP, xip_making("Xcode.app", V27));
        runner.answers(CODESIGN, exits(0, ""));
        let clock: Arc<dyn Clock> = FakeClock::new();
        let root = scratch(name);
        fs::create_dir(root.path().join("Applications")).expect("the Applications directory");
        Mac {
            root,
            store: FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES),
            runner,
            space: FakeSpace {
                free: Ok(needed()),
                reads: Arc::default(),
                swap_in: None,
            },
            clock,
        }
    }

    pub(super) fn stage(&self) -> Stage {
        Stage {
            store: Box::new(self.store.clone()),
            runner: Box::new(self.runner.clone()),
            space: Box::new(self.space.clone()),
            plists: Box::new(HostPlists::new(HostRoot::at(self.root.path()))),
            root: HostRoot::at(self.root.path()),
            clock: Arc::clone(&self.clock),
        }
    }

    pub(super) fn run(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        self.stage().run(entries)
    }

    /// The host's `path` under the scratch root.
    pub(super) fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path.trim_start_matches('/'))
    }

    /// The names in the host's directory `path`, sorted; none when it is not there.
    pub(super) fn names(&self, path: &str) -> Vec<String> {
        let Ok(listed) = fs::read_dir(self.path(path)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = listed
            .map(|item| {
                item.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The staging directory as the stage makes it: the test's user's, mode 0700.
    pub(super) fn trusted_staging(&self) {
        DirBuilder::new()
            .mode(0o700)
            .create(self.path(STAGING))
            .expect("the staging directory");
    }

    /// The host's file `path`, with `bytes`.
    pub(super) fn file(&self, path: &str, bytes: &[u8]) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        fs::write(path, bytes).expect("the file");
    }

    /// The deadline of a program run now for `secs`.
    pub(super) fn deadline(&self, secs: u64) -> Stamp {
        let now = Stamp::now(&*self.clock);
        Stamp {
            boot_id: now.boot_id,
            mono_secs: now.mono_secs + secs,
        }
    }

    /// The run of `xip` on the archive of `entry`.
    pub(super) fn xip_run(&self, entry: &XcodeEntry) -> Spawn {
        let dir = self.path(&format!("{STAGING}/{}", entry.sha256));
        Spawn {
            program: PathBuf::from(XIP),
            args: vec![
                OsString::from("--expand"),
                dir.join(ARCHIVE).into_os_string(),
            ],
            env: vec![(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )],
            cwd: dir.join(EXPAND),
            deadline: self.deadline(XIP_SECS),
        }
    }

    /// The run of `codesign` on the app at the host's `app`.
    pub(super) fn codesign_run(&self, app: &str) -> Spawn {
        Spawn {
            program: PathBuf::from(CODESIGN),
            args: vec![
                OsString::from("--verify"),
                OsString::from("--deep"),
                OsString::from("--strict"),
                self.path(app).into_os_string(),
            ],
            env: vec![(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )],
            cwd: self.path(STAGING),
            deadline: self.deadline(CODESIGN_SECS),
        }
    }

    /// Nothing of a stage is left: no staging directory, nothing in `/Applications`.
    pub(super) fn assert_clean(&self) {
        assert!(
            !self.path(STAGING).exists(),
            "the staging directory is removed"
        );
        assert_eq!(self.names(APPLICATIONS), Vec::<String>::new());
    }
}

pub(super) fn deferral(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Deferred {
        reason,
        detail: detail.to_owned(),
    })
}
