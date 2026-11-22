//! Whether a Mac's Full Disk Access grant is in force for the daemon, which every gRPC heartbeat
//! reports (`HeartbeatRequest.full_disk_access`).
//!
//! The check is a listing of one directory only the grant allows, [`TCC_DIR`]: the daemon opens
//! it and reads one entry, and opens no file inside it, so the privacy database is never read.
//! The listing is behind the port [`TccDirectory`], so the whole logic runs against fakes. A
//! listing that works says [`FullDiskAccess::Granted`]; one the system refuses for want of
//! permission (`EPERM` or `EACCES`, the kind `PermissionDenied`) says
//! [`FullDiskAccess::NotGranted`]; any other error says [`FullDiskAccess::Undetermined`] and is
//! logged. A host that is not a Mac says [`FullDiskAccess::NotApplicable`] and runs no check.
//!
//! The grant is a profile an administrator installs or removes while the daemon runs, so the
//! state is not fixed at the start. The source ([`HostFullDiskAccess`]) checks once when it is
//! made, and again when a heartbeat reads it and the last check is [`RECHECK_SECS`] old or more
//! (on the unix clock, in either direction), so a profile installed or removed shows within a
//! minute, without the daemon reading the profile store. The state of the last check is kept
//! behind a mutex. The log gets one line at the start and one line whenever the state changes,
//! naming the state before and after; a check that finds the same state logs nothing.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{FullDiskAccess, FullDiskAccessSource, Log, UnixClock};

/// The directory only the grant lets a process list.
pub const TCC_DIR: &str = "/Library/Application Support/com.apple.TCC";

/// The seconds a check stands before a heartbeat that reads the state makes it again.
pub const RECHECK_SECS: u64 = 60;

/// The listing the check makes.
pub trait TccDirectory: Send + Sync {
    /// Opens the directory and reads one entry, opening none.
    ///
    /// # Errors
    /// The error the system gave: `PermissionDenied` when the grant is not in force.
    fn list(&self) -> io::Result<()>;
}

/// [`TCC_DIR`] on the host, through `std::fs::read_dir`.
pub struct HostTccDirectory {
    path: PathBuf,
}

impl HostTccDirectory {
    /// The host's own [`TCC_DIR`].
    #[must_use]
    pub fn host() -> HostTccDirectory {
        HostTccDirectory::at(PathBuf::from(TCC_DIR))
    }

    /// The directory `path`.
    #[must_use]
    pub fn at(path: PathBuf) -> HostTccDirectory {
        HostTccDirectory { path }
    }
}

impl TccDirectory for HostTccDirectory {
    fn list(&self) -> io::Result<()> {
        fs::read_dir(&self.path)?.next().transpose().map(|_| ())
    }
}

/// A listing now, as a state: the error kind decides, and an error that is not a refusal for want
/// of permission leaves the state undetermined and is logged.
fn check(tcc: &dyn TccDirectory, log: &dyn Log) -> FullDiskAccess {
    match tcc.list() {
        Ok(()) => FullDiskAccess::Granted,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => FullDiskAccess::NotGranted,
        Err(error) => {
            log.line(&format!("full disk access: the check failed: {error}"));
            FullDiskAccess::Undetermined
        }
    }
}

/// The state of the last check and the unix second it was made.
#[derive(Clone, Copy)]
struct Checked {
    state: FullDiskAccess,
    at: u64,
}

/// The state of the grant a heartbeat reports, kept from the last check.
pub struct HostFullDiskAccess {
    tcc: Arc<dyn TccDirectory>,
    clock: Arc<dyn UnixClock>,
    log: Arc<dyn Log>,
    /// Whether this host is a Mac: any other host is never checked.
    mac: bool,
    last: Mutex<Checked>,
}

impl HostFullDiskAccess {
    /// The source of a host of `kind`, which has made its first check and logged its state: a Mac
    /// lists `tcc`; any other host answers not applicable without listing.
    #[must_use]
    pub fn new(
        kind: PlatformKind,
        tcc: Arc<dyn TccDirectory>,
        clock: Arc<dyn UnixClock>,
        log: Arc<dyn Log>,
    ) -> HostFullDiskAccess {
        let mac = kind == PlatformKind::Macos;
        let state = if mac {
            let state = check(tcc.as_ref(), log.as_ref());
            log.line(&format!("full disk access: {}", state.as_str()));
            state
        } else {
            FullDiskAccess::NotApplicable
        };
        let at = clock.unix_secs();
        HostFullDiskAccess {
            tcc,
            clock,
            log,
            mac,
            last: Mutex::new(Checked { state, at }),
        }
    }
}

impl FullDiskAccessSource for HostFullDiskAccess {
    fn full_disk_access(&self) -> FullDiskAccess {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.unix_secs();
        if !self.mac || now.abs_diff(last.at) < RECHECK_SECS {
            return last.state;
        }
        let state = check(self.tcc.as_ref(), self.log.as_ref());
        if state != last.state {
            self.log.line(&format!(
                "full disk access: {} -> {}",
                last.state.as_str(),
                state.as_str()
            ));
        }
        *last = Checked { state, at: now };
        state
    }
}

/// The source a host of `kind` reports its state with: a Mac's lists [`TCC_DIR`] on `clock`; any
/// other host's answers not applicable.
#[must_use]
pub fn at_host(
    kind: PlatformKind,
    clock: Arc<dyn UnixClock>,
    log: Arc<dyn Log>,
) -> Arc<dyn FullDiskAccessSource> {
    let tcc = Arc::new(HostTccDirectory::host());
    Arc::new(HostFullDiskAccess::new(kind, tcc, clock, log))
}

#[cfg(test)]
#[path = "full_disk_access_tests.rs"]
mod tests;
