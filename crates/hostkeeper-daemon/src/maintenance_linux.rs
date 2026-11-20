//! The `linux` and `reboot` kinds of the maintenance hook on a Linux host, under systemd or
//! OpenRC (docs/design/maintenance-updates.md, sections 5.2 step 3 and 5.5): the `cutover` that
//! installs the run's Linux updates and says whether the host now needs a reboot, the reboot of a
//! `reboot`-only run, and the version the host runs after the reboot. The hook keeps the run
//! record and asks for the reboot ([`crate::maintenance_hook`]); the platform's reboot is
//! [`rebooter::HostRebooter`].
//!
//! **The install.** The host's package manager ([`Upgrader`]: [`apt::AptUpgrade`],
//! [`dnf::DnfUpgrade`], [`apk::ApkUpgrade`], the first whose program the host has, as the
//! maintenance look picks its collector) installs the updates of the run's `linux_scope`:
//! `security` (the security updates and the kernel) or `all`. Each program runs as root with an
//! empty environment, no standard input, a deadline and capped output (a `LaunchctlProcess`);
//! apt's install alone is given its environment on its argument list, through `/usr/bin/env`
//! ([`apt::SCRIPTS_ENV`]), so that no package script asks a question and needrestart restarts no
//! service mid-run. The packages hostkeeper itself installs ([`crate::maintenance_own`]) never
//! move: apt holds them, dnf excludes them by name and apk pins them at the installed version.
//! The package manager's lock is waited for up to `--package-lock-wait-secs`, on top of the
//! install's own deadline ([`INSTALL_DEADLINE`]).
//!
//! **The reboot.** After the install the host is asked whether it needs a reboot, as the look
//! asks ([`crate::maintenance_host::HostFacts`]: a newer kernel installed than the one running;
//! [`crate::maintenance_reboot::RebootNeed`]: `/run/reboot-required`, `needrestart -b`,
//! `needs-restarting -r`): when it does, the `cutover` answers [`Cutover::NeedsReboot`], and the
//! hook records the reboot under way before it asks for it. A `reboot`-only run installs nothing
//! and always answers [`Cutover::NeedsReboot`]. After the reboot both kinds answer the running
//! kernel's release, and fail [`REASON_NEW_KERNEL_NOT_RUNNING`] when a newer one is still
//! installed and not running.

pub mod apk;
pub mod apt;
pub mod dnf;
pub mod kinds;
pub mod rebooter;

use std::collections::BTreeSet;
use std::time::Duration;

use crate::maintenance_collect::LinuxScope;
use crate::maintenance_hook::KindError;

/// The reason of a program of the install (an index refresh, a listing, a hold or pin, the
/// upgrade) that did not succeed; the detail names the step and its exit, and the first line of
/// its standard error.
pub const REASON_INSTALL_FAILED: &str = "install_failed";
/// The reason of a host with none of `apt-get`, `dnf` and `apk`.
pub const REASON_NO_PACKAGE_MANAGER: &str = "no_package_manager";
/// The reason of a `linux` kind whose run gives no scope it knows (`LINUX_SCOPE_UNSPECIFIED`, or
/// a value this release does not know).
pub const REASON_SCOPE_INVALID: &str = "linux_scope_invalid";
/// The reason of an install after which the host could not be asked whether it needs a reboot.
pub const REASON_REBOOT_CHECK_FAILED: &str = "reboot_check_failed";
/// The reason of a running kernel that could not be read after the reboot.
pub const REASON_KERNEL_UNREADABLE: &str = "kernel_unreadable";
/// The reason of a reboot after which a newer kernel is still installed and not running.
pub const REASON_NEW_KERNEL_NOT_RUNNING: &str = "new_kernel_not_running";
/// The reason of the packages hostkeeper itself installs that could not be read.
pub const REASON_OWN_PACKAGES_UNREADABLE: &str = "own_packages_unreadable";

/// How long the upgrade of the packages may take, on top of the wait for the package manager's
/// lock: an hour, a kernel and a distribution's security updates with their scripts.
pub const INSTALL_DEADLINE: Duration = Duration::from_secs(3600);

/// How long a refresh of the package index may take, on top of the wait for the lock: it fetches
/// the index of every source.
pub const REFRESH_DEADLINE: Duration = Duration::from_secs(600);

/// How long a listing (a simulation, a list of the updates) or a hold or pin may take, on top of
/// the wait for the lock.
pub const LIST_DEADLINE: Duration = Duration::from_secs(300);

/// What an install is asked for: the run's scope and the packages that never move.
#[derive(Clone, Copy, Debug)]
pub struct Upgrade<'a> {
    /// Which updates: `security` (the security updates and the kernel) or `all`.
    pub scope: LinuxScope,
    /// The package manager's names of the packages hostkeeper itself installs: held, excluded or
    /// pinned, never moved.
    pub own_packages: &'a BTreeSet<String>,
}

/// A host's package manager, installing the updates of a run.
pub trait Upgrader: Send + Sync {
    /// The package manager's name: `apt`, `dnf` or `apk`.
    fn name(&self) -> &'static str;

    /// Installs the updates `upgrade` asks for, non-interactively, keeping the configuration
    /// files the host has and leaving hostkeeper's own packages where they are.
    ///
    /// # Errors
    /// [`REASON_INSTALL_FAILED`], naming the program's step and its exit, when a program did not
    /// succeed.
    fn upgrade(&self, upgrade: &Upgrade<'_>) -> Result<(), KindError>;
}

/// The failure of a program of the install: `detail` names its step and its exit.
#[must_use]
pub fn install_failed(detail: String) -> KindError {
    KindError::Failed {
        reason: REASON_INSTALL_FAILED,
        detail,
    }
}

/// The name of `package` without an architecture suffix (`libc6:amd64` is `libc6`).
#[must_use]
pub fn without_arch(package: &str) -> &str {
    package.split_once(':').map_or(package, |(name, _)| name)
}

#[cfg(test)]
#[path = "maintenance_linux_tests.rs"]
mod tests;
