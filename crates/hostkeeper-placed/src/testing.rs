//! The fakes of this crate, for its own tests and for the tests of its callers:
//!
//! - [`FaultFs`]: the real filesystem under a root, recording every write in a [`Log`] and
//!   failing the n-th write and every one after it, as a crash there would leave the disk;
//! - [`FakeUnitControl`]: a [`UnitControl`] that records each call in the same [`Log`], so that
//!   a test reads the order of writes and calls in one list (a test puts the calls of its own
//!   fakes among them with [`Event::Mark`]);
//! - [`FakeLauncher`]: a [`CommandLauncher`] that records each argument vector.
//!
//! None of them runs a program, opens a socket or reads a clock.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::fs::{FileKind, Fs, RealFs};
use crate::unit_control::{CommandLauncher, UnitControl, UnitControlError};

/// One write of a [`FaultFs`], with the paths as the host names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsWrite {
    Write(PathBuf),
    SyncFile(PathBuf),
    SyncDir(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
    Symlink { target: PathBuf, link: PathBuf },
    Remove(PathBuf),
    RemoveDir(PathBuf),
    CreateDirs(PathBuf),
}

/// One call of a [`FakeUnitControl`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitCall {
    DaemonReload,
    Stop(String),
}

/// One event of a [`Log`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Fs(FsWrite),
    Unit(UnitCall),
    /// What a test wrote to the log itself: the call of another fake, put in its place among
    /// these.
    Mark(String),
}

/// The events of the fakes that share it, in the order they happened.
#[derive(Clone, Debug, Default)]
pub struct Log(Arc<Mutex<Vec<Event>>>);

impl Log {
    pub fn new() -> Log {
        Log::default()
    }

    pub fn push(&self, event: Event) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }

    /// Every event so far.
    pub fn events(&self) -> Vec<Event> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forgets every event so far.
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// [`RealFs`] under `root`, recording each write that happened in its [`Log`]. Built with
/// [`FaultFs::failing_at`], the n-th write (counted from 1) and every later one fail with an
/// error and change nothing, as if the process had died before it. With
/// [`FaultFs::failing_read`], a read fails once or from then on.
pub struct FaultFs {
    inner: RealFs,
    log: Log,
    fail_at: Option<u64>,
    writes: Mutex<u64>,
    /// The read that fails, and whether every later read fails too.
    read_fault: Option<(u64, bool)>,
    reads: Mutex<u64>,
}

impl FaultFs {
    /// A filesystem under `root` that never fails.
    pub fn new(root: &Path, log: Log) -> FaultFs {
        FaultFs {
            inner: RealFs::new(root),
            log,
            fail_at: None,
            writes: Mutex::new(0),
            read_fault: None,
            reads: Mutex::new(0),
        }
    }

    /// The same filesystem, whose `n`-th read (`read`, `kind`, `read_link` or `list`, counted
    /// from 1) fails; with `persistent`, every later read fails too, else only that one.
    pub fn failing_read(self, n: u64, persistent: bool) -> FaultFs {
        FaultFs {
            read_fault: Some((n, persistent)),
            ..self
        }
    }

    /// How many reads were attempted, failed ones included.
    pub fn reads(&self) -> u64 {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a read, and runs it unless it is a failing one.
    fn read_op<T>(&self, run: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let n = {
            let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
            *reads += 1;
            *reads
        };
        let fails = self
            .read_fault
            .is_some_and(|(at, persistent)| (n == at) | (persistent & (n > at)));
        if fails {
            return Err(io::Error::other(format!("injected failure of read {n}")));
        }
        run()
    }

    /// A filesystem under `root` whose `n`-th write and every later one fail.
    pub fn failing_at(root: &Path, n: u64, log: Log) -> FaultFs {
        FaultFs {
            fail_at: Some(n),
            ..FaultFs::new(root, log)
        }
    }

    /// How many writes were attempted, failed ones included.
    pub fn writes(&self) -> u64 {
        *self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a write, and runs it unless it is the failing one or later.
    fn write_op(&self, event: FsWrite, run: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let n = {
            let mut writes = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
            *writes += 1;
            *writes
        };
        if self.fail_at.is_some_and(|at| n >= at) {
            return Err(io::Error::other(format!("injected failure of write {n}")));
        }
        run()?;
        self.log.push(Event::Fs(event));
        Ok(())
    }
}

impl Fs for FaultFs {
    fn host_path(&self, path: &Path) -> PathBuf {
        self.inner.host_path(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_op(|| self.inner.read(path))
    }

    fn kind(&self, path: &Path) -> io::Result<Option<FileKind>> {
        self.read_op(|| self.inner.kind(path))
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.read_op(|| self.inner.read_link(path))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.read_op(|| self.inner.list(dir))
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write_op(FsWrite::Write(path.to_path_buf()), || {
            self.inner.write(path, bytes)
        })
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.write_op(FsWrite::SyncFile(path.to_path_buf()), || {
            self.inner.sync_file(path)
        })
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.write_op(FsWrite::SyncDir(dir.to_path_buf()), || {
            self.inner.sync_dir(dir)
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let event = FsWrite::Rename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        };
        self.write_op(event, || self.inner.rename(from, to))
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let event = FsWrite::Symlink {
            target: target.to_path_buf(),
            link: link.to_path_buf(),
        };
        self.write_op(event, || self.inner.symlink(target, link))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.write_op(FsWrite::Remove(path.to_path_buf()), || {
            self.inner.remove_file(path)
        })
    }

    fn remove_dir(&self, dir: &Path) -> io::Result<()> {
        self.write_op(FsWrite::RemoveDir(dir.to_path_buf()), || {
            self.inner.remove_dir(dir)
        })
    }

    fn create_dirs(&self, dir: &Path) -> io::Result<()> {
        self.write_op(FsWrite::CreateDirs(dir.to_path_buf()), || {
            self.inner.create_dirs(dir)
        })
    }
}

/// A [`UnitControl`] that records each call in its [`Log`]; built with
/// [`FakeUnitControl::failing`], every call is recorded and then fails; built with
/// [`FakeUnitControl::refusing_to_stop`], the stop of one unit is recorded and then fails, as a
/// unit still active after its stop bound would.
pub struct FakeUnitControl {
    log: Log,
    failing: bool,
    stuck: Option<String>,
}

impl FakeUnitControl {
    pub fn new(log: Log) -> FakeUnitControl {
        FakeUnitControl {
            log,
            failing: false,
            stuck: None,
        }
    }

    pub fn failing(log: Log) -> FakeUnitControl {
        FakeUnitControl {
            failing: true,
            ..FakeUnitControl::new(log)
        }
    }

    /// Every call goes through but the stop of `unit`, which fails.
    pub fn refusing_to_stop(log: Log, unit: &str) -> FakeUnitControl {
        FakeUnitControl {
            stuck: Some(unit.to_owned()),
            ..FakeUnitControl::new(log)
        }
    }

    fn call(&self, call: UnitCall) -> Result<(), UnitControlError> {
        let action = format!("{call:?}");
        let stuck = matches!(&call, UnitCall::Stop(unit) if self.stuck.as_ref() == Some(unit));
        self.log.push(Event::Unit(call));
        match self.failing | stuck {
            true => Err(UnitControlError {
                action,
                why: "the fake fails every call".to_owned(),
            }),
            false => Ok(()),
        }
    }
}

impl UnitControl for FakeUnitControl {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        self.call(UnitCall::DaemonReload)
    }

    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.call(UnitCall::Stop(unit.to_owned()))
    }
}

/// A [`CommandLauncher`] that records each argument vector; built with
/// [`FakeLauncher::failing`], every run is recorded and then fails.
#[derive(Debug, Default)]
pub struct FakeLauncher {
    runs: Mutex<Vec<Vec<String>>>,
    failing: bool,
}

impl FakeLauncher {
    pub fn new() -> FakeLauncher {
        FakeLauncher::default()
    }

    pub fn failing() -> FakeLauncher {
        FakeLauncher {
            failing: true,
            ..FakeLauncher::default()
        }
    }

    /// Every argument vector run so far.
    pub fn runs(&self) -> Vec<Vec<String>> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl CommandLauncher for FakeLauncher {
    fn run(&self, argv: &[&str]) -> Result<(), String> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(argv.iter().map(|arg| (*arg).to_owned()).collect());
        match self.failing {
            true => Err("exit status 1".to_owned()),
            false => Ok(()),
        }
    }
}
