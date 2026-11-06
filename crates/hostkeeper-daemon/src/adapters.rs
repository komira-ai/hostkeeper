//! The host's adapters, chosen by its platform (design section 5.9: a Mac runs the same daemon;
//! only what is behind these ports differs). [`Adapters::for_platform`] is the one place the
//! choice is made:
//!
//! - Linux: steps as transient systemd units ([`SystemdLauncher::system`]), the observer over
//!   `systemctl` and `/proc`, no service manager yet ([`Unwired`]), the units under
//!   `/etc/systemd/system`, and no package check beyond the archive rules;
//! - macOS: steps as launchd jobs ([`LaunchdLauncher`]), the service manager, the unit control
//!   and the observer over `launchctl` ([`LaunchdServiceManager`], [`LaunchdUnitControl`]), the
//!   process probe over `proc_pidpath` and the image's device and inode
//!   ([`LaunchdProcessProbe`]), the plists under `/Library/LaunchDaemons`, and the quarantine and
//!   code-signature checks of every unpacked package ([`HostMacVerify`]).
//!
//! The host's clock is chosen here too ([`host_clock`]): the kernel's monotonic clock under the
//! boot id of the system the daemon was built for.
//!
//! [`Adapters::macos`] assembles the macOS set from the ports it is given, so it is measured on
//! Linux against fakes; only the host's own `launchctl` clock and `proc_pidpath` are compiled
//! for macOS alone (`host_macos`). A Linux build asked for the macOS set refuses to start.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enroll::{ClaimsSource, Ioreg, Ipmi, LinuxClaims, MacClaims};
use hostkeeper_launchd::{
    Launchctl, LaunchdProcessProbe, LaunchdServiceManager, LaunchdUnitControl, PidInfo, Platform,
    PlatformKind, StdFileIdentity,
};
use hostkeeper_observe::{ProcessProbe, ServiceManager};
use hostkeeper_placed::UnitControl;
use hostkeeper_steps::{BootIdSource, StepLauncher, SystemClock, SystemdLauncher};
use hostkeeper_unpack::MacVerify;

use crate::cli::Unwired;
use crate::hook_log::log_hook_output;
use crate::observer::{proc_probe, systemd_manager};
use crate::start::StartError;

/// How long one `launchctl` command of the unit control may run: `bootout` waits for the job to
/// end, up to launchd's exit timeout.
pub const LAUNCHCTL_DEADLINE: Duration = Duration::from_secs(60);
/// How long one `launchctl print` of the observer may run, as `systemctl show` on Linux.
pub const PRINT_DEADLINE: Duration = Duration::from_secs(5);

/// The package check of a platform: none on Linux, the quarantine and signature check on macOS.
pub type PackageCheck = Option<Arc<dyn MacVerify + Send + Sync>>;

/// What differs between the platforms, as ports.
pub struct Adapters {
    /// The platform these are for.
    pub kind: PlatformKind,
    /// Runs each hook step.
    pub launcher: Arc<dyn StepLauncher>,
    /// Stops, starts and asks after a service's units.
    pub units: Arc<dyn UnitControl + Send + Sync>,
    /// Where the units of a service are linked.
    pub unit_dir: PathBuf,
    /// The observer's service manager.
    pub manager: Box<dyn ServiceManager>,
    /// The observer's process probe.
    pub process: Box<dyn ProcessProbe>,
    /// The check every unpacked package passes before it is sealed.
    pub package_check: PackageCheck,
}

impl Adapters {
    /// The adapters of `platform` on this host. `Err` when this build has none for it (a Linux
    /// build asked for macOS), or when the macOS unit directory cannot be read.
    pub fn for_platform(platform: &Platform) -> Result<Adapters, StartError> {
        match platform.kind {
            PlatformKind::Linux => Ok(Adapters::linux(platform)),
            PlatformKind::Macos => host_macos(platform),
        }
    }

    /// The Linux set, the units under `platform`'s unit directory.
    pub fn linux(platform: &Platform) -> Adapters {
        Adapters {
            kind: PlatformKind::Linux,
            launcher: Arc::new(SystemdLauncher::system().with_log(Arc::new(log_hook_output))),
            units: Arc::new(Unwired),
            unit_dir: platform.unit_dir.clone(),
            manager: Box::new(systemd_manager()),
            process: Box::new(proc_probe()),
            package_check: None,
        }
    }

    /// The macOS set over the ports given: `launcher` runs the steps, `launchctl` is what the
    /// unit control (with [`LAUNCHCTL_DEADLINE`]) and the service manager ask, `print` what the
    /// observer's service manager asks, `pids` what the process probe asks, and `check` judges
    /// every unpacked package. `Err` when the unit directory of `platform` cannot be read.
    pub fn macos<L, P>(
        platform: &Platform,
        launcher: Arc<dyn StepLauncher>,
        launchctl: L,
        print: L,
        pids: P,
        check: Arc<dyn MacVerify + Send + Sync>,
    ) -> Result<Adapters, StartError>
    where
        L: Launchctl + 'static,
        P: PidInfo + 'static,
    {
        let units = LaunchdUnitControl::new(launchctl, &platform.unit_dir).map_err(|source| {
            StartError::Io {
                path: platform.unit_dir.clone(),
                source,
            }
        })?;
        Ok(Adapters {
            kind: PlatformKind::Macos,
            launcher,
            units: Arc::new(units),
            unit_dir: platform.unit_dir.clone(),
            manager: Box::new(LaunchdServiceManager::new(print)),
            process: Box::new(LaunchdProcessProbe::new(pids, StdFileIdentity)),
            package_check: Some(check),
        })
    }
}

/// The host's macOS set: `/bin/launchctl`, the host's step launcher, `proc_pidpath`, `codesign`
/// and `getxattr`.
#[cfg(target_os = "macos")]
fn host_macos(platform: &Platform) -> Result<Adapters, StartError> {
    use hostkeeper_launchd::procid::darwin::DarwinPidInfo;
    use hostkeeper_launchd::{LaunchctlProcess, LaunchdLauncher};

    use crate::mac::HostMacVerify;

    Adapters::macos(
        platform,
        Arc::new(LaunchdLauncher::system().with_log(Arc::new(log_hook_output))),
        LaunchctlProcess::system(LAUNCHCTL_DEADLINE),
        LaunchctlProcess::system(PRINT_DEADLINE),
        DarwinPidInfo,
        Arc::new(HostMacVerify::system()),
    )
}

/// A build for another system has no macOS adapters to give.
#[cfg(not(target_os = "macos"))]
fn host_macos(_platform: &Platform) -> Result<Adapters, StartError> {
    Err(StartError::NoAdapters { platform: "macOS" })
}

/// The host's clock: the kernel's monotonic clock, under the boot id of the system the daemon
/// was built for ([`host_boot_id`]).
pub fn host_clock() -> SystemClock {
    SystemClock::new(host_boot_id())
}

/// The boot id of a Mac: `kern.bootsessionuuid`.
#[cfg(target_os = "macos")]
pub fn host_boot_id() -> Box<dyn BootIdSource> {
    Box::new(hostkeeper_launchd::SysctlBootId)
}

/// The boot id of Linux: the kernel's `boot_id` file.
#[cfg(not(target_os = "macos"))]
pub fn host_boot_id() -> Box<dyn BootIdSource> {
    Box::new(hostkeeper_steps::ProcBootId::host())
}

/// The enrollment claims of a host of `kind`. On Linux, the files of `linux` (the host's own:
/// [`LinuxClaims::host`]) and the BMC's MAC asked of `bmc` (the host's: [`DevIpmi::host`]), so a
/// host with a BMC claims its MAC and a host with none claims the rest; on macOS the serial and
/// the platform UUID that `ioreg` gives ([`MacClaims`]), and nothing of Linux's.
///
/// [`DevIpmi::host`]: crate::ipmi::DevIpmi::host
pub fn host_claims(
    kind: PlatformKind,
    linux: LinuxClaims,
    ioreg: Box<dyn Ioreg>,
    bmc: Arc<dyn Ipmi>,
) -> Box<dyn ClaimsSource> {
    match kind {
        PlatformKind::Linux => Box::new(linux.with_ipmi(bmc)),
        PlatformKind::Macos => Box::new(MacClaims::new(ioreg)),
    }
}
