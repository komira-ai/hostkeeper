//! The wait for the package manager's lock (packages-repositories.md section 4.5): a tool with
//! no lock timeout of its own (`dpkg --purge`, `apt-mark`) that finds the lock held is tried
//! again every [`LOCK_POLL_SECS`] on the installer's [`super::Pause`], for at most the
//! installer's `lock_wait_secs`; the wait is reported when it begins and every
//! [`LOCK_REPORT_SECS`] after ([`super::Pause::lock_wait`]). A lock still held at the bound is
//! `package_manager_busy`: deferred, the host's condition, never the release's. `apt-get`
//! waits for the lock itself, for the same bound (`DPkg::Lock::Timeout`, [`super::apt`]).

use super::RepositoryInstaller;
use super::outcome::{Failure, Reason};
use super::tools::{LockWait, Tool};

/// The seconds between two tries of a tool whose lock is held.
pub const LOCK_POLL_SECS: u64 = 2;

/// The seconds between two reports of a wait that goes on.
pub const LOCK_REPORT_SECS: u64 = 30;

/// How one try of a tool that takes the lock ended.
pub(super) enum Try<T> {
    /// It ran: its value.
    Done(T),
    /// The lock was held: the tool's last line, which names its holder.
    Busy(String),
}

impl RepositoryInstaller<'_> {
    /// Runs `once`, a try of `tool`, until it is not [`Try::Busy`], waiting
    /// [`LOCK_POLL_SECS`] between two tries (less for the last pause, so the wait is the whole
    /// bound and never more) for `lock_wait_secs` in all; a wait is reported at its first second
    /// and every [`LOCK_REPORT_SECS`] after. A lock still held once the bound is waited is
    /// `package_manager_busy`; any other failure of a try ends the wait as it is.
    pub(super) fn waiting<T>(
        &self,
        tool: Tool,
        mut once: impl FnMut() -> Result<Try<T>, Failure>,
    ) -> Result<T, Failure> {
        let mut waited = 0;
        loop {
            let holder = match once()? {
                Try::Done(value) => return Ok(value),
                Try::Busy(holder) => holder,
            };
            let left = self.lock_wait_secs.saturating_sub(waited);
            if left == 0 {
                return Err(Failure::new(
                    Reason::PackageManagerBusy,
                    format!(
                        "the package manager's lock was held for {waited}s of {}s ({}): {holder}",
                        self.lock_wait_secs,
                        tool.name()
                    ),
                ));
            }
            if waited % LOCK_REPORT_SECS == 0 {
                self.pause.lock_wait(&LockWait {
                    tool,
                    waited_secs: waited,
                    bound_secs: self.lock_wait_secs,
                    holder,
                });
            }
            let pause = left.min(LOCK_POLL_SECS);
            self.pause.pause(pause);
            waited += pause;
        }
    }

    /// `apt-mark hold` or `unhold` of `package`, tried again while the lock is held.
    pub(super) fn hold_waiting(
        &self,
        package: &str,
        hold: bool,
        bound: u64,
    ) -> Result<(), Failure> {
        self.waiting(Tool::AptMark, || {
            match self.backend.hold(self.tools, package, hold, bound) {
                Ok(()) => Ok(Try::Done(())),
                Err(f) if f.reason() == Reason::PackageManagerBusy => {
                    Ok(Try::Busy(f.detail().to_owned()))
                }
                Err(f) => Err(f),
            }
        })
    }
}
