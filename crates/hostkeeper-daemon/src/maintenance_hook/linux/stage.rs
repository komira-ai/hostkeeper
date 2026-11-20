//! The stage of the `linux` kind: the maintenance hook's `prepare` on a Linux host
//! (docs/design/maintenance-updates.md, section 5.2 step 1 and section 5.7). It runs while the
//! host still serves, so it never fills the disk and installs nothing:
//!
//! 1. The run's scope (`linux_scope`: `security` or `all`) and the host's package manager
//!    (`apt-get`, else `dnf`, else `apk`, as the maintenance look picks it).
//! 2. The package manager's own estimate of what the scope will download, hostkeeper's own
//!    packages left out ([`OwnPackages`]): apt from `--print-uris`, dnf from `--assumeno`'s total,
//!    apk from `info --size` (each in its own file beside this one).
//! 3. The free space where the download lands ([`Disk`]): at least twice the download, or the
//!    stage defers `free_space_short` and downloads nothing.
//! 4. The download alone: `apt-get --download-only`, `dnf upgrade --downloadonly`, `apk fetch`.
//!
//! **The lock.** A program that finds the package manager's lock held is run again every
//! [`LOCK_POLL_SECS`] seconds, the wait made by the injected [`LockWaiter`], for at most the
//! hook's `--package-lock-wait-secs` per run of a program (the refresh and the download may each
//! wait that long), as the repository installer waits for each tool; a lock still held then
//! defers `package_manager_busy`.
//!
//! **Failure.** Every failure of the stage is [`KindError::Deferred`] (exit 75): the stage changed
//! nothing the host runs, and the run goes on. Its detail names the step and the exit code and the
//! first line of the program's standard error, never a parameter.
//!
//! **Programs.** Each runs by its absolute path as the hook's user (root), with an empty
//! environment and no standard input ([`HostPrograms`]): a command that may meet the lock
//! (`update`, the download) through a [`Launchctl`], whose output is kept to 64 KiB; a listing
//! through a [`LineTool`], read line by line and never cut, since a large upgrade lists more.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_install::repository::wait::LOCK_POLL_SECS;
use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput, LaunchctlProcess};
use hostkeeper_placed::RealFs;
use hostkeeper_placed::roots::Roots;
use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;

use super::{apk, apt, dnf};
use crate::Placement;
use crate::line_process::{LineProcess, LineTool};
use crate::login_account::exit_failure;
use crate::maintenance_apk::APK;
use crate::maintenance_apt::APT_GET;
use crate::maintenance_collect::{HostRoot, LinuxScope, Look};
use crate::maintenance_dnf::DNF;
use crate::maintenance_hook::kind::KindError;
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_own::{OwnPackages, PlacedPackages};

/// The reason of an order whose `linux_scope` is not `security` or `all`.
pub const REASON_SCOPE_INVALID: &str = "linux_scope_invalid";
/// The reason of a host with none of `apt-get`, `dnf` and `apk`.
pub const REASON_NO_PACKAGE_MANAGER: &str = "no_package_manager";
/// The reason of hostkeeper's own packages that could not be read.
pub const REASON_OWN_PACKAGES: &str = "own_packages_unreadable";
/// The reason of a package manager's lock held past `--package-lock-wait-secs`.
pub const REASON_BUSY: &str = "package_manager_busy";
/// The reason of a refresh, listing or estimate that failed or could not be read.
pub const REASON_LISTING_FAILED: &str = "stage_listing_failed";
/// The reason of a download directory that could not be made or measured.
pub const REASON_STAGE_DIR: &str = "stage_dir_failed";
/// The reason of less free space than twice the download.
pub const REASON_SPACE_SHORT: &str = "free_space_short";
/// The reason of a download that failed.
pub const REASON_DOWNLOAD_FAILED: &str = "stage_download_failed";

/// How long a refresh, a listing or an estimate may take.
pub const LISTING_DEADLINE: Duration = Duration::from_secs(600);
/// How long the download may take; one stopped at it is deferred, and apt and dnf keep what
/// they had downloaded for the next stage.
pub const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(3600);

/// The package manager of a Linux host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Apt,
    Dnf,
    Apk,
}

impl Manager {
    /// The package manager whose program is a file under `root`: `apt-get`, else `dnf`, else
    /// `apk` (the maintenance look's order); `None` when there is none of them.
    pub fn under(root: &HostRoot) -> Option<Manager> {
        [Manager::Apt, Manager::Dnf, Manager::Apk]
            .into_iter()
            .find(|manager| root.is_file(manager.program()))
    }

    /// The program's absolute path.
    pub const fn program(self) -> &'static str {
        match self {
            Manager::Apt => APT_GET,
            Manager::Dnf => DNF,
            Manager::Apk => APK,
        }
    }

    /// Whether a run of the program ended because the lock was held, from its exit code and
    /// what it wrote.
    fn busy(self) -> Busy {
        match self {
            Manager::Apt => apt::busy,
            Manager::Dnf => dnf::busy,
            Manager::Apk => apk::busy,
        }
    }
}

/// Whether a run ended because the package manager's lock was held: its exit code and what it
/// wrote (both streams of a command; nothing of a listing, whose standard error is not read).
pub(super) type Busy = fn(Option<i32>, &str) -> bool;

/// A run that never meets the lock: a simulation or a listing that opens nothing for writing.
pub(super) fn never_busy(_code: Option<i32>, _text: &str) -> bool {
    false
}

/// Where the stage's programs run.
pub trait Programs: Send + Sync {
    /// The runner of a command `program` (a refresh, the download), bounded by `deadline`.
    fn command(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl>;
    /// The runner of a listing `program`, its standard output read line by line, bounded by
    /// `deadline`.
    fn listing(&self, program: &Path, deadline: Duration) -> Box<dyn LineTool>;
}

/// The host's programs: each with an empty environment and no standard input, killed at its
/// deadline ([`LaunchctlProcess`], [`LineProcess`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct HostPrograms;

impl Programs for HostPrograms {
    fn command(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(LaunchctlProcess::new(program, deadline))
    }

    fn listing(&self, program: &Path, deadline: Duration) -> Box<dyn LineTool> {
        Box::new(LineProcess::new(program, deadline))
    }
}

/// The disk the download lands on.
pub trait Disk: Send + Sync {
    /// Makes the host's directory `dir` and its parents when they are not there.
    ///
    /// # Errors
    /// Why it could not, naming `dir`.
    fn make_dir(&self, dir: &str) -> Result<(), String>;

    /// The bytes free in the filesystem of the host's directory `dir`, as an unprivileged user
    /// may use them: a lower bound of what the package manager (root) may.
    ///
    /// # Errors
    /// Why they could not be read, naming `dir`.
    fn free_bytes(&self, dir: &str) -> Result<u64, String>;
}

/// The disk under a [`HostRoot`]: `/` on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDisk(HostRoot);

impl HostDisk {
    /// The disk of the host's paths under `root`.
    pub fn new(root: HostRoot) -> HostDisk {
        HostDisk(root)
    }
}

impl Disk for HostDisk {
    fn make_dir(&self, dir: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.0.path(dir))
            .map_err(|error| format!("{dir} could not be made: {error}"))
    }

    /// The filesystem's available blocks (`f_bavail`) times its fragment size (`f_frsize`).
    fn free_bytes(&self, dir: &str) -> Result<u64, String> {
        let stat = rustix::fs::statvfs(&self.0.path(dir))
            .map_err(|error| format!("the free space of {dir} could not be read: {error}"))?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
}

/// The wait between two tries of a program whose lock is held.
pub trait LockWaiter: Send + Sync {
    /// Waits `secs` seconds.
    fn wait(&self, secs: u64);
}

/// The host's wait: the thread sleeps.
#[derive(Clone, Copy, Debug, Default)]
pub struct SleepWaiter;

impl LockWaiter for SleepWaiter {
    fn wait(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

/// What the scope will download, as the package manager estimates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Download {
    /// The host's directory it lands in.
    pub dir: &'static str,
    /// The package manager's estimate of its bytes, more than 0.
    pub bytes: u64,
    /// The step's name in a reason.
    pub what: &'static str,
    /// The arguments of the download.
    pub args: Vec<String>,
}

/// The deferral of `reason` with `detail`.
pub(super) fn deferred(reason: &'static str, detail: String) -> KindError {
    KindError::Deferred { reason, detail }
}

/// `prefix` and then `rest`, as owned arguments.
pub(super) fn arguments(prefix: &[&str], rest: &[String]) -> Vec<String> {
    prefix
        .iter()
        .map(|arg| (*arg).to_owned())
        .chain(rest.iter().cloned())
        .collect()
}

/// How one try of a program that may meet the lock ended.
enum Try<T> {
    /// It ran: its value.
    Done(T),
    /// The lock was held.
    Busy,
}

/// The runs of one package manager's program, each waiting for its lock.
pub(super) struct Calls<'a> {
    program: &'static str,
    programs: &'a dyn Programs,
    waiter: &'a dyn LockWaiter,
    lock_wait_secs: u64,
}

impl Calls<'_> {
    /// Runs the command `args` (`what` names it) until the lock is not held; its output when it
    /// exited 0, else the deferral of `reason`.
    pub(super) fn command(
        &self,
        what: &str,
        args: &[String],
        busy: Busy,
        reason: &'static str,
        deadline: Duration,
    ) -> Result<LaunchctlOutput, KindError> {
        let tool = self.programs.command(Path::new(self.program), deadline);
        let output = self.waiting(what, || {
            let output = tool
                .run(args)
                .map_err(|error| deferred(reason, not_run(what, &error)))?;
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push('\n');
            text.push_str(&output.stderr_text());
            Ok(match busy(output.code, &text) {
                true => Try::Busy,
                false => Try::Done(output),
            })
        })?;
        match output.code {
            Some(0) => Ok(output),
            _ => Err(deferred(reason, exit_failure(what, &output, true))),
        }
    }

    /// Runs the listing `args` (`what` names it) until the lock is not held; its exit code, one
    /// of `codes`, and the lines of its standard output, else the deferral `stage_listing_failed`.
    pub(super) fn listing(
        &self,
        what: &str,
        args: &[String],
        busy: Busy,
        codes: &[i32],
    ) -> Result<(i32, Vec<String>), KindError> {
        let tool = self
            .programs
            .listing(Path::new(self.program), LISTING_DEADLINE);
        self.waiting(what, || {
            let mut lines = Vec::new();
            let code = tool
                .run_lines(args, &mut |line| lines.push(line.to_owned()))
                .map_err(|why| listing_failed(format!("{what} could not be run: {why}")))?;
            if busy(code, "") {
                return Ok(Try::Busy);
            }
            match code {
                Some(code) if codes.contains(&code) => Ok(Try::Done((code, lines))),
                Some(code) => Err(listing_failed(format!(
                    "{what} failed with exit code {code}"
                ))),
                None => Err(listing_failed(format!("{what} was ended by a signal"))),
            }
        })
    }

    /// Runs `once` until it is not [`Try::Busy`], waiting [`LOCK_POLL_SECS`] between two tries
    /// (less before the last, so the wait is the bound and never more) for `lock_wait_secs` in
    /// all; a lock still held then is `package_manager_busy`.
    fn waiting<T>(
        &self,
        what: &str,
        mut once: impl FnMut() -> Result<Try<T>, KindError>,
    ) -> Result<T, KindError> {
        let mut waited: u64 = 0;
        loop {
            if let Try::Done(value) = once()? {
                return Ok(value);
            }
            let left = self.lock_wait_secs.saturating_sub(waited);
            if left == 0 {
                return Err(deferred(
                    REASON_BUSY,
                    format!(
                        "the package manager's lock was held for {waited}s of {}s ({what})",
                        self.lock_wait_secs
                    ),
                ));
            }
            let pause = left.min(LOCK_POLL_SECS);
            self.waiter.wait(pause);
            waited += pause;
        }
    }
}

/// The deferral of a listing that failed: `stage_listing_failed`.
pub(super) fn listing_failed(detail: String) -> KindError {
    deferred(REASON_LISTING_FAILED, detail)
}

/// Why a command did not run to its end, naming `what` and not its arguments.
fn not_run(what: &str, error: &LaunchctlError) -> String {
    match error {
        LaunchctlError::Spawn { why, .. } => format!("{what} could not be run: {why}"),
        LaunchctlError::TimedOut { .. } => format!("{what} did not finish within its deadline"),
    }
}

/// The bytes of `number` (decimal digits, with at most one point and at most 6 digits after it)
/// times `unit`, rounded up; `None` for any other text, or more than `u64` holds.
pub(super) fn scaled(number: &str, unit: u64) -> Option<u64> {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) || fraction.len() > 6 {
        return None;
    }
    let scale = fraction.bytes().fold(1u128, |scale, _| scale * 10);
    let value: u128 = format!("{whole}{fraction}").parse().ok()?;
    let bytes = value.checked_mul(u128::from(unit))?.div_ceil(scale);
    u64::try_from(bytes).ok()
}

/// The stage of a Linux host.
pub struct Stage {
    root: HostRoot,
    programs: Box<dyn Programs>,
    disk: Box<dyn Disk>,
    waiter: Box<dyn LockWaiter>,
    own: Box<dyn OwnPackages>,
    lock_wait_secs: u64,
}

impl Stage {
    /// The stage of the host whose files are under `root`, running its programs through
    /// `programs`, measuring and making its download directory through `disk`, waiting for a held
    /// lock through `waiter` for at most `lock_wait_secs`, and leaving `own`'s packages out.
    pub fn new(
        root: HostRoot,
        programs: Box<dyn Programs>,
        disk: Box<dyn Disk>,
        waiter: Box<dyn LockWaiter>,
        own: Box<dyn OwnPackages>,
        lock_wait_secs: u64,
    ) -> Stage {
        Stage {
            root,
            programs,
            disk,
            waiter,
            own,
            lock_wait_secs,
        }
    }

    /// The stage of this host: its own files, programs and disk, a sleep between two tries, and
    /// the packages of the daemon's `placed.json` (in [`hostkeeper_placed::roots::STATE_DIR`]),
    /// with hostkeeper's own, left out.
    pub fn at_host(lock_wait_secs: u64) -> Stage {
        let placement = Placement {
            roots: Roots::linux(),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        };
        Stage::new(
            HostRoot::host(),
            Box::new(HostPrograms),
            Box::new(HostDisk::new(HostRoot::host())),
            Box::new(SleepWaiter),
            Box::new(PlacedPackages::new(&placement)),
            lock_wait_secs,
        )
    }

    /// Stages the run `params`: downloads what its scope will install when there is room for
    /// twice the download, and installs nothing.
    ///
    /// # Errors
    /// [`KindError::Deferred`], whatever went wrong: nothing the host runs was touched.
    pub fn run(&self, params: &MaintainParams) -> Result<(), KindError> {
        let scope = scope(params.maintain().linux_scope)?;
        let manager = Manager::under(&self.root).ok_or_else(|| {
            deferred(
                REASON_NO_PACKAGE_MANAGER,
                "this host has none of apt-get, dnf and apk".to_owned(),
            )
        })?;
        let own = self
            .own
            .names()
            .map_err(|why| deferred(REASON_OWN_PACKAGES, format!("own packages: {why}")))?;
        let look = Look {
            scope,
            own_packages: &own,
        };
        let calls = Calls {
            program: manager.program(),
            programs: &*self.programs,
            waiter: &*self.waiter,
            lock_wait_secs: self.lock_wait_secs,
        };
        let planned = match manager {
            Manager::Apt => apt::plan(&calls, &look)?,
            Manager::Dnf => dnf::plan(&calls, &look)?,
            Manager::Apk => apk::plan(&calls, &look)?,
        };
        let Some(download) = planned else {
            return Ok(());
        };
        self.room_for(&download)?;
        calls.command(
            download.what,
            &download.args,
            manager.busy(),
            REASON_DOWNLOAD_FAILED,
            DOWNLOAD_DEADLINE,
        )?;
        Ok(())
    }

    /// Makes the download's directory, and checks its filesystem has twice the download free.
    fn room_for(&self, download: &Download) -> Result<(), KindError> {
        let dir = download.dir;
        self.disk
            .make_dir(dir)
            .map_err(|why| deferred(REASON_STAGE_DIR, why))?;
        let free = self
            .disk
            .free_bytes(dir)
            .map_err(|why| deferred(REASON_STAGE_DIR, why))?;
        let needed = download.bytes.saturating_mul(2);
        if free < needed {
            return Err(deferred(
                REASON_SPACE_SHORT,
                format!(
                    "{dir} has {free} bytes free; the download of {} bytes needs {needed}",
                    download.bytes
                ),
            ));
        }
        Ok(())
    }
}

/// The scope of an order's `linux_scope`.
fn scope(value: i32) -> Result<LinuxScope, KindError> {
    match WireScope::try_from(value) {
        Ok(WireScope::Security) => Ok(LinuxScope::Security),
        Ok(WireScope::All) => Ok(LinuxScope::All),
        Ok(WireScope::Unspecified) | Err(_) => Err(deferred(
            REASON_SCOPE_INVALID,
            format!("linux_scope {value} is not security or all"),
        )),
    }
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
