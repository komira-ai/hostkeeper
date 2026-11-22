//! What the maintenance look ([`crate::maintenance_facts`], docs/design/maintenance-updates.md
//! section 1) is made of: one [`Collector`] per source of facts, each in a file of its own, each
//! giving a [`Section`] of the report; the look joins the sections in order
//! ([`crate::maintenance_collectors`] lists the collectors of each platform).
//!
//! A collector reads its host through ports: a program through a [`Launchctl`] (on a host, a
//! `LaunchctlProcess`: as root, an empty environment, no standard input, a deadline, and each
//! output stream kept to [`OUTPUT_MAX`] bytes), and a file through a [`HostRoot`], the directory
//! its absolute paths are read under (`/` on a host, a scratch directory in a test). A listing is
//! parsed by one function per tool, pinned by fixtures; a line that function does not know is
//! counted in [`Section::unparsed`] and never guessed at.
//!
//! A collector that cannot read what it reads fails with a reason ([`Collector::collect`]): the
//! program's step and exit code and the first line of its standard error, a deadline, or a file
//! that cannot be read. The look then fails as a whole (`MaintenanceReport.look_failed`). A
//! package manager's program that takes its lock is waited for while another process holds it
//! ([`LockWait`]), and fails the look only when the lock is still held at the bound.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_install::repository::wait::LOCK_POLL_SECS;
use hostkeeper_launchd::launchctl::OUTPUT_MAX;
use hostkeeper_launchd::{Launchctl, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::{MAX_DUE_LABEL, MAX_DUE_VERSION};

use crate::login_account::{exit_failure, run};

/// Which Linux updates a host reports due (`hostkeeper daemon --maintenance-linux-scope`, the
/// group setting `linux_scope` of the design's section 3): `security`, the default, reports
/// security updates and newer kernels; `all` reports every update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LinuxScope {
    /// Security updates and newer kernels.
    #[default]
    Security,
    /// Every update.
    All,
}

/// What a look is made under, the same for every collector of it.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    /// Which Linux updates are due.
    pub scope: LinuxScope,
    /// The packages hostkeeper itself installs, by the package manager's name: never due.
    pub own_packages: &'a BTreeSet<String>,
}

impl Look<'_> {
    /// Whether a Linux update of `package` is due under this look: never for a package of
    /// hostkeeper's own ([`Look::own_packages`], by its name without an architecture suffix
    /// `:amd64`); under [`LinuxScope::Security`], when it is a security update or a kernel; under
    /// [`LinuxScope::All`], always.
    #[must_use]
    pub fn due(&self, package: &str, security: bool, kernel: bool) -> bool {
        let name = package.split_once(':').map_or(package, |(name, _)| name);
        if self.own_packages.contains(name) {
            return false;
        }
        match self.scope {
            LinuxScope::Security => security || kernel,
            LinuxScope::All => true,
        }
    }
}

/// What one collector found: the facts of the report it gives. The look joins the sections in
/// the collectors' order: a host text a section leaves empty (and a boot time it leaves 0) is
/// taken from the others, the first that is not empty winning; the lists are joined in order;
/// the Mac facts are joined field by field, each taken from the first section that gives it (a
/// FileVault state other than unspecified, `authrestart_supported` true, a free size, an update
/// account, Command Line Tools) and the device-management conflicts joined in order, each once;
/// the unparsed lines are added up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    /// What the host runs, as far as this collector reads it.
    pub host: pb::MaintenanceHost,
    /// The Xcodes installed.
    pub xcodes: Vec<pb::InstalledXcode>,
    /// On a Mac, what a macOS update needs.
    pub mac: Option<pb::MacUpdateFacts>,
    /// What is due.
    pub due: Vec<pb::DueItem>,
    /// How many lines of its listings no parser knew.
    pub unparsed: u32,
}

/// One source of the facts of a maintenance look.
pub trait Collector: Send + Sync {
    /// Its name, which a failed look's reason starts with (`apt`, `kernel`).
    fn name(&self) -> &'static str;

    /// Reads the host under `look`.
    ///
    /// # Errors
    /// Why it could not: the reason a failed look reports after the collector's name.
    fn collect(&self, look: &Look<'_>) -> Result<Section, String>;
}

/// The directory a collector reads the host's absolute paths under: `/` on a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRoot(PathBuf);

impl HostRoot {
    /// The host's own files.
    #[must_use]
    pub fn host() -> HostRoot {
        HostRoot(PathBuf::from("/"))
    }

    /// The files under `dir`, for a test's fixture.
    #[must_use]
    pub fn at(dir: &Path) -> HostRoot {
        HostRoot(dir.to_owned())
    }

    /// Where the host's absolute `path` is under this root.
    #[must_use]
    pub fn path(&self, path: &str) -> PathBuf {
        self.0.join(path.trim_start_matches('/'))
    }

    /// The text of the host's file `path`.
    ///
    /// # Errors
    /// Why it could not be read, naming the host's path.
    pub fn read(&self, path: &str) -> Result<String, String> {
        std::fs::read_to_string(self.path(path))
            .map_err(|error| format!("{path} could not be read: {error}"))
    }

    /// The text of the host's file `path`, or `None` when there is none.
    ///
    /// # Errors
    /// Why a file that is there could not be read, naming the host's path.
    pub fn read_if_there(&self, path: &str) -> Result<Option<String>, String> {
        match std::fs::read_to_string(self.path(path)) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{path} could not be read: {error}")),
        }
    }

    /// Whether the host's `path` is there, as a file or anything else (never following a last
    /// link that dangles, which is there too).
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.path(path).symlink_metadata().is_ok()
    }

    /// Whether the host's `path` is a file a program could be run from.
    #[must_use]
    pub fn is_file(&self, path: &str) -> bool {
        self.path(path).is_file()
    }

    /// The names of the directories in the host's directory `path`, in no order; an entry that
    /// cannot be read is left out.
    ///
    /// # Errors
    /// Why the directory could not be read, naming the host's path.
    pub fn dirs_in(&self, path: &str) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(self.path(path))
            .map_err(|error| format!("{path} could not be read: {error}"))?;
        Ok(entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect())
    }
}

/// The directory of a Mac's applications, where its Xcodes are.
pub const APPLICATIONS: &str = "/Applications";

/// The names of the Xcodes in [`APPLICATIONS`] under `root`: each directory whose name starts
/// with `Xcode` and ends with `.app`, sorted by name; none when there is no [`APPLICATIONS`].
///
/// # Errors
/// Why [`APPLICATIONS`] could not be read.
pub fn xcode_apps(root: &HostRoot) -> Result<Vec<String>, String> {
    if !root.exists(APPLICATIONS) {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = root
        .dirs_in(APPLICATIONS)?
        .into_iter()
        .filter(|name| name.starts_with("Xcode") && name.ends_with(".app"))
        .collect();
    names.sort();
    Ok(names)
}

/// Runs `tool` with `args` (`what` names the step) and gives its exit code and standard output
/// when it exited with one of `codes`; else why not: it did not run, did not end by its deadline,
/// exited with another code (with the first line of its standard error), or wrote [`OUTPUT_MAX`]
/// bytes or more to its standard output, which may have been cut.
///
/// # Errors
/// The reason, naming `what` and never the arguments.
pub fn listing(
    tool: &dyn Launchctl,
    what: &str,
    args: &[&str],
    codes: &[i32],
) -> Result<(i32, String), String> {
    checked(run(tool, what, args)?, what, codes)
}

/// The exit code and standard output of `output` when it exited with one of `codes`, as
/// [`listing`] says.
fn checked(output: LaunchctlOutput, what: &str, codes: &[i32]) -> Result<(i32, String), String> {
    match output.code {
        Some(code) if codes.contains(&code) => {
            if output.stdout.len() >= OUTPUT_MAX {
                return Err(format!(
                    "{what} wrote {OUTPUT_MAX} bytes or more, which may have been cut"
                ));
            }
            Ok((code, String::from_utf8_lossy(&output.stdout).into_owned()))
        }
        _ => Err(exit_failure(what, &output, true)),
    }
}

/// Whether a run of a package manager's program found its lock held, from its exit code and its
/// standard output and standard error joined: each package manager's own
/// ([`crate::maintenance_apt::lock_held`], [`crate::maintenance_dnf::lock_held`],
/// [`crate::maintenance_apk::lock_held`]).
pub type LockHeld = fn(Option<i32>, &str) -> bool;

/// The wait between two tries of a program whose lock is held.
pub trait LockWaiter: Send + Sync {
    /// Waits `secs` seconds.
    fn wait(&self, secs: u64);
}

/// A wait made by a sleep: the host's is the thread's ([`SleepWaiter::host`]).
pub struct SleepWaiter {
    sleep: Box<dyn Fn(Duration) + Send + Sync>,
}

impl SleepWaiter {
    /// The wait that asks `sleep` for each wait's duration.
    pub fn new(sleep: Box<dyn Fn(Duration) + Send + Sync>) -> SleepWaiter {
        SleepWaiter { sleep }
    }

    /// The host's wait: the thread sleeps ([`std::thread::sleep`]).
    pub fn host() -> SleepWaiter {
        SleepWaiter::new(Box::new(std::thread::sleep))
    }
}

impl LockWaiter for SleepWaiter {
    /// Sleeps `secs` seconds.
    fn wait(&self, secs: u64) {
        (self.sleep)(Duration::from_secs(secs));
    }
}

/// How a look waits for a package manager's lock that another process holds (an `apt-get` of the
/// repository installer, unattended-upgrades, an operator's `dnf`): a program that finds it held
/// is run again every [`LOCK_POLL_SECS`] seconds, the wait made by its [`LockWaiter`], for at most
/// `bound_secs` per run of a program (the daemon's `--package-lock-wait-secs`, which is the
/// maintenance hook's bound too), as the hook's stage waits: dnf's two listings may each wait
/// that long. The package managers' own timeouts are not used: apt's
/// `DPkg::Lock::Timeout` does not reach the lock of `/var/lib/apt/lists`, which `apt-get update`
/// takes, and dnf waits with no bound of its own.
#[derive(Clone)]
pub struct LockWait {
    waiter: Arc<dyn LockWaiter>,
    bound_secs: u64,
}

impl LockWait {
    /// The wait of at most `bound_secs`, made by `waiter`.
    #[must_use]
    pub fn new(waiter: Arc<dyn LockWaiter>, bound_secs: u64) -> LockWait {
        LockWait { waiter, bound_secs }
    }

    /// The host's wait of at most `bound_secs`: the thread sleeps ([`SleepWaiter::host`]).
    #[must_use]
    pub fn host(bound_secs: u64) -> LockWait {
        LockWait::new(Arc::new(SleepWaiter::host()), bound_secs)
    }

    /// [`listing`], run again while `held` says the lock is held, waiting [`LOCK_POLL_SECS`]
    /// between two tries (less before the last, so the wait is the bound and never more).
    ///
    /// # Errors
    /// As [`listing`]; a lock still held once the bound is waited is `the package manager's lock
    /// was held for <waited>s of <bound>s: ` and then the run's failure ([`exit_failure`]: the
    /// step, the exit code and the first line of its standard error, which names the lock).
    pub fn listing(
        &self,
        tool: &dyn Launchctl,
        what: &str,
        args: &[&str],
        codes: &[i32],
        held: LockHeld,
    ) -> Result<(i32, String), String> {
        let mut waited: u64 = 0;
        loop {
            let output = run(tool, what, args)?;
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push('\n');
            text.push_str(&output.stderr_text());
            if !held(output.code, &text) {
                return checked(output, what, codes);
            }
            let left = self.bound_secs.saturating_sub(waited);
            if left == 0 {
                return Err(format!(
                    "the package manager's lock was held for {waited}s of {}s: {}",
                    self.bound_secs,
                    exit_failure(what, &output, true)
                ));
            }
            let pause = left.min(LOCK_POLL_SECS);
            self.waiter.wait(pause);
            waited += pause;
        }
    }
}

/// `text` cut to at most `max` bytes, at a character's boundary.
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A due Linux package update: `package` from `from` (empty for a package not yet installed) to
/// `to`, its `security`, and a reboot needed when it is a `kernel`; each text cut to its bound.
#[must_use]
pub fn linux_item(
    package: &str,
    from: &str,
    to: &str,
    security: bool,
    kernel: bool,
) -> pb::DueItem {
    pb::DueItem {
        kind: pb::MaintenanceKind::Linux.into(),
        from: clip(from, MAX_DUE_VERSION),
        to: clip(to, MAX_DUE_VERSION),
        needs_reboot: kernel,
        label: clip(package, MAX_DUE_LABEL),
        security,
    }
}

/// A due reboot for `reason` (`reboot-required`, `kernel`), from `from` (the running kernel, or
/// empty), cut to its bounds.
#[must_use]
pub fn reboot_item(reason: &str, from: &str) -> pb::DueItem {
    pb::DueItem {
        kind: pb::MaintenanceKind::Reboot.into(),
        from: clip(from, MAX_DUE_VERSION),
        to: String::new(),
        needs_reboot: true,
        label: clip(reason, MAX_DUE_LABEL),
        security: false,
    }
}

/// `left` against `right` as versions: runs of digits compared as numbers (`6.10` after `6.9`,
/// leading zeros ignored), any other runs as bytes, run by run; a version that is a prefix of the
/// other comes first.
#[must_use]
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut left = runs(left);
    let mut right = runs(right);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let order = compare_runs(a, b);
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// The runs of `text`: each a longest stretch of ASCII digits or of anything else.
fn runs(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let digits = first.is_ascii_digit();
        let end = rest
            .find(|c: char| c.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        let (run, after) = rest.split_at(end);
        rest = after;
        Some(run)
    })
}

/// Two runs: as numbers when both are digits, else as bytes (digits before anything else).
fn compare_runs(a: &str, b: &str) -> Ordering {
    let a_digits = a.starts_with(|c: char| c.is_ascii_digit());
    let b_digits = b.starts_with(|c: char| c.is_ascii_digit());
    match (a_digits, b_digits) {
        (true, true) => {
            let a = a.trim_start_matches('0');
            let b = b.trim_start_matches('0');
            a.len().cmp(&b.len()).then_with(|| a.cmp(b))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

#[cfg(test)]
#[path = "maintenance_collect_tests.rs"]
mod tests;
