//! The fakes of this crate, for its own tests and for the tests of its callers:
//!
//! - [`FaultFs`]: the real filesystem under a root, recording every write in a [`Log`] and
//!   failing the n-th write and every one after it, as a crash there would leave the disk;
//! - [`FakeUnitControl`]: a [`UnitControl`] that records each call in the same [`Log`], so that
//!   a test reads the order of writes and calls in one list, and keeps which units are active
//!   (a test puts the calls of its own fakes among them with [`Event::Mark`]);
//! - [`FakeLauncher`]: a [`CommandLauncher`] that records each argument vector.
//!
//! None of them runs a program, opens a socket or reads a clock.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::fs::{FileKind, Fs, RealFs};
use crate::unit_control::{CommandLauncher, LaunchError, UnitControl, UnitControlError};

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
    Start(String),
    IsActive(String),
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

/// What a [`FakeUnitControl`] holds besides its log: the units active now, and the faults a
/// test asked for.
#[derive(Debug, Default)]
struct Units {
    active: BTreeSet<String>,
    /// For a unit: how many more times [`UnitControl::is_active`] answers `false` once it is
    /// started.
    late: BTreeMap<String, u64>,
    fail_reload: bool,
    fail_start: BTreeSet<String>,
    fail_stop: BTreeSet<String>,
}

/// A [`UnitControl`] that records each call in its [`Log`] and keeps which units are active: a
/// start makes a unit active, a stop makes it inactive. Built with [`FakeUnitControl::failing`],
/// every call is recorded and then fails; [`FakeUnitControl::fail_reload`],
/// [`FakeUnitControl::fail_start`], [`FakeUnitControl::fail_stop`] and
/// [`FakeUnitControl::active_after`] fail or delay one kind of call.
pub struct FakeUnitControl {
    log: Log,
    failing: bool,
    units: Mutex<Units>,
}

impl FakeUnitControl {
    pub fn new(log: Log) -> FakeUnitControl {
        FakeUnitControl {
            log,
            failing: false,
            units: Mutex::default(),
        }
    }

    pub fn failing(log: Log) -> FakeUnitControl {
        FakeUnitControl {
            failing: true,
            ..FakeUnitControl::new(log)
        }
    }

    fn units(&self) -> MutexGuard<'_, Units> {
        self.units.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `daemon-reload` fails from now on; every other call works.
    pub fn fail_reload(&self) {
        self.units().fail_reload = true;
    }

    /// A start of `unit` fails from now on, and leaves it inactive.
    pub fn fail_start(&self, unit: &str) {
        self.units().fail_start.insert(unit.to_owned());
    }

    /// A stop of `unit` fails from now on, and leaves it as it was: a unit still active after its
    /// stop bound.
    pub fn fail_stop(&self, unit: &str) {
        self.units().fail_stop.insert(unit.to_owned());
    }

    /// Once `unit` is started, [`UnitControl::is_active`] answers `false` for it `asks` times
    /// before it answers `true`.
    pub fn active_after(&self, unit: &str, asks: u64) {
        self.units().late.insert(unit.to_owned(), asks);
    }

    /// `unit` is active, as if it had been started before the test.
    pub fn set_active(&self, unit: &str) {
        self.units().active.insert(unit.to_owned());
    }

    /// The units active now.
    pub fn active(&self) -> BTreeSet<String> {
        self.units().active.clone()
    }

    /// Records `call`, and fails it when the fake fails every call or `fault` says so.
    fn call(&self, call: UnitCall, fault: bool) -> Result<(), UnitControlError> {
        let action = format!("{call:?}");
        self.log.push(Event::Unit(call));
        let why = match (self.failing, fault) {
            (true, _) => "the fake fails every call",
            (false, true) => "the fake fails this call",
            (false, false) => return Ok(()),
        };
        Err(UnitControlError {
            action,
            why: why.to_owned(),
        })
    }
}

impl UnitControl for FakeUnitControl {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        let fault = self.units().fail_reload;
        self.call(UnitCall::DaemonReload, fault)
    }

    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        let fault = self.units().fail_stop.contains(unit);
        self.call(UnitCall::Stop(unit.to_owned()), fault)?;
        self.units().active.remove(unit);
        Ok(())
    }

    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        let fault = self.units().fail_start.contains(unit);
        self.call(UnitCall::Start(unit.to_owned()), fault)?;
        self.units().active.insert(unit.to_owned());
        Ok(())
    }

    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        self.call(UnitCall::IsActive(unit.to_owned()), false)?;
        let mut units = self.units();
        if !units.active.contains(unit) {
            return Ok(false);
        }
        match units.late.get_mut(unit) {
            Some(left) if *left > 0 => {
                *left -= 1;
                Ok(false)
            }
            _ => Ok(true),
        }
    }
}

/// A [`CommandLauncher`] that records each argument vector; built with
/// [`FakeLauncher::failing`] or [`FakeLauncher::failing_with`], every run is recorded and then
/// fails.
#[derive(Debug, Default)]
pub struct FakeLauncher {
    runs: Mutex<Vec<Vec<String>>>,
    fault: Option<LaunchError>,
}

impl FakeLauncher {
    pub fn new() -> FakeLauncher {
        FakeLauncher::default()
    }

    /// Every run exits 1.
    pub fn failing() -> FakeLauncher {
        FakeLauncher::failing_with(LaunchError::Exit(1))
    }

    /// Every run ends with `error`.
    pub fn failing_with(error: LaunchError) -> FakeLauncher {
        FakeLauncher {
            fault: Some(error),
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
    fn run(&self, argv: &[&str]) -> Result<(), LaunchError> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(argv.iter().map(|arg| (*arg).to_owned()).collect());
        match &self.fault {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}
