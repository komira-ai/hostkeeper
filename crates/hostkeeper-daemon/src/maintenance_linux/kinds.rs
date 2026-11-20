//! The `linux` and `reboot` kinds of the maintenance hook on a Linux host ([`LinuxKind`],
//! [`RebootKind`]), and the question they ask of the host after an install, whether it needs a
//! reboot and which kernel it runs ([`RebootCheck`]).
//!
//! `cutover` and `after_reboot` are written here. The `prepare` of `linux` is its stage
//! ([`Stage`], `crate::maintenance_hook::linux::stage`) when the kind is given one; the `prepare`
//! of `reboot` and the `verify` of both answer [`Unsupported`]: on a host the hook checks both
//! kinds with [`crate::maintenance_hook::LinuxCheck`] in place of their `verify`
//! ([`crate::maintenance_hook::registry_for`]).

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::agent::v1::{self as pb, LinuxScope as WireScope};

use crate::maintenance_apk::APK;
use crate::maintenance_apt::APT_GET;
use crate::maintenance_collect::{Collector, HostRoot, LinuxScope, Look};
use crate::maintenance_dnf::DNF;
use crate::maintenance_hook::linux::stage::Stage;
use crate::maintenance_hook::{
    Cutover, Kind, KindError, MaintainParams, MaintenanceKind, Unsupported,
};
use crate::maintenance_host::HostFacts;
use crate::maintenance_linux::apk::ApkUpgrade;
use crate::maintenance_linux::apt::AptUpgrade;
use crate::maintenance_linux::dnf::DnfUpgrade;
use crate::maintenance_linux::{
    REASON_KERNEL_UNREADABLE, REASON_NEW_KERNEL_NOT_RUNNING, REASON_NO_PACKAGE_MANAGER,
    REASON_OWN_PACKAGES_UNREADABLE, REASON_REBOOT_CHECK_FAILED, REASON_SCOPE_INVALID, Upgrade,
    Upgrader,
};
use crate::maintenance_own::OwnPackages;
use crate::maintenance_reboot::RebootNeed;

/// The label of the reboot item that [`HostFacts`] makes due for a kernel installed and not
/// running.
const KERNEL_LABEL: &str = "kernel";

/// The detail of a `linux` kind whose run gives no scope it knows.
const SCOPE_DETAIL: &str = "linux_scope is not security or all";

/// The detail of a host with no package manager.
const NO_PACKAGE_MANAGER_DETAIL: &str = "the host has none of apt-get, dnf and apk";

/// Whether `item` is a due reboot.
fn is_reboot(item: &pb::DueItem) -> bool {
    item.kind == i32::from(pb::MaintenanceKind::Reboot)
}

/// What the host says about its reboot and its kernel: the collectors of the maintenance look
/// that read them.
pub struct RebootCheck {
    host: HostFacts,
    need: RebootNeed,
}

impl RebootCheck {
    /// The check over `host` (the running and the newest kernel) and `need` (the reboot the host
    /// asks for).
    #[must_use]
    pub fn new(host: HostFacts, need: RebootNeed) -> RebootCheck {
        RebootCheck { host, need }
    }

    /// The check over this host's own programs and files.
    #[must_use]
    pub fn at_host() -> RebootCheck {
        RebootCheck::new(HostFacts::at_host(), RebootNeed::at_host())
    }

    /// Whether the host needs a reboot: either collector holds a due reboot (a newer kernel than
    /// the running one is installed, `/run/reboot-required`, `needrestart`, `needs-restarting`).
    ///
    /// # Errors
    /// [`REASON_REBOOT_CHECK_FAILED`] when a collector could not read the host; the detail names
    /// the collector and its reason.
    pub fn needed(&self) -> Result<bool, KindError> {
        // Neither collector reads the scope or the own packages of its look.
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        let collectors: [&dyn Collector; 2] = [&self.host, &self.need];
        for collector in collectors {
            let section = collector
                .collect(&look)
                .map_err(|reason| KindError::Failed {
                    reason: REASON_REBOOT_CHECK_FAILED,
                    detail: format!("{}: {reason}", collector.name()),
                })?;
            if section.due.iter().any(is_reboot) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The release of the kernel the host runs, once no newer one is installed.
    ///
    /// # Errors
    /// [`REASON_KERNEL_UNREADABLE`] when the host's facts could not be read;
    /// [`REASON_NEW_KERNEL_NOT_RUNNING`] when a newer kernel than the running one is installed.
    pub fn running(&self) -> Result<String, KindError> {
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        let section = self
            .host
            .collect(&look)
            .map_err(|reason| KindError::Failed {
                reason: REASON_KERNEL_UNREADABLE,
                detail: format!("{}: {reason}", self.host.name()),
            })?;
        let newer_installed = section
            .due
            .iter()
            .any(|item| is_reboot(item) && item.label == KERNEL_LABEL);
        if newer_installed {
            return Err(KindError::Failed {
                reason: REASON_NEW_KERNEL_NOT_RUNNING,
                detail: format!(
                    "the kernel {} runs; {} is installed",
                    section.host.running_kernel, section.host.newest_kernel
                ),
            });
        }
        Ok(section.host.running_kernel)
    }
}

/// The upgrader of the package manager whose program is a file under `root`: `apt-get`, else
/// `dnf`, else `apk`, in the order the maintenance look picks its collector; none when the host
/// has none of them.
#[must_use]
pub fn upgrader_under(root: &HostRoot, lock_wait_secs: u64) -> Option<Box<dyn Upgrader>> {
    if root.is_file(APT_GET) {
        Some(Box::new(AptUpgrade::at_host(lock_wait_secs)))
    } else if root.is_file(DNF) {
        Some(Box::new(DnfUpgrade::at_host(lock_wait_secs)))
    } else if root.is_file(APK) {
        Some(Box::new(ApkUpgrade::at_host(lock_wait_secs)))
    } else {
        None
    }
}

/// The scope a run's `linux_scope` names.
fn scope_of(wire: i32) -> Result<LinuxScope, KindError> {
    match WireScope::try_from(wire) {
        Ok(WireScope::Security) => Ok(LinuxScope::Security),
        Ok(WireScope::All) => Ok(LinuxScope::All),
        Ok(WireScope::Unspecified) | Err(_) => Err(KindError::Failed {
            reason: REASON_SCOPE_INVALID,
            detail: SCOPE_DETAIL.to_owned(),
        }),
    }
}

/// The `linux` kind: its stage, the install of the host's package manager, then the question
/// whether the host needs a reboot.
pub struct LinuxKind {
    upgrader: Option<Box<dyn Upgrader>>,
    own: Box<dyn OwnPackages>,
    check: RebootCheck,
    stage: Option<Stage>,
}

impl LinuxKind {
    /// The kind installing with `upgrader` (none for a host with no package manager), leaving
    /// the packages `own` names where they are, and asking `check` after the install.
    #[must_use]
    pub fn new(
        upgrader: Option<Box<dyn Upgrader>>,
        own: Box<dyn OwnPackages>,
        check: RebootCheck,
    ) -> LinuxKind {
        LinuxKind {
            upgrader,
            own,
            check,
            stage: None,
        }
    }

    /// This kind, its `prepare` staged by `stage`.
    #[must_use]
    pub(crate) fn with_stage(mut self, stage: Stage) -> LinuxKind {
        self.stage = Some(stage);
        self
    }

    /// The kind of this host: the package manager it has ([`upgrader_under`] its own files),
    /// waiting up to `lock_wait_secs` for the package manager's lock, its own reboot check, and
    /// its own stage ([`Stage::at_host`]), which waits as long.
    #[must_use]
    pub fn at_host(lock_wait_secs: u64, own: Box<dyn OwnPackages>) -> LinuxKind {
        LinuxKind::new(
            upgrader_under(&HostRoot::host(), lock_wait_secs),
            own,
            RebootCheck::at_host(),
        )
        .with_stage(Stage::at_host(lock_wait_secs))
    }
}

impl MaintenanceKind for LinuxKind {
    /// The stage ([`Stage::run`]): the free space checked, then a download alone; every failure
    /// deferred. [`Unsupported`] for a kind given no stage.
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        match &self.stage {
            Some(stage) => stage.run(params),
            None => Unsupported(Kind::Linux).prepare(params),
        }
    }

    /// Installs the updates of the run's `linux_scope`, then answers [`Cutover::NeedsReboot`]
    /// when the host needs a reboot and [`Cutover::Applied`] with the running kernel when it does
    /// not.
    ///
    /// # Errors
    /// [`REASON_SCOPE_INVALID`] for a scope that is not `security` or `all`;
    /// [`REASON_NO_PACKAGE_MANAGER`] for a host with none; [`REASON_OWN_PACKAGES_UNREADABLE`]
    /// when hostkeeper's packages could not be read; the upgrader's failure; the reboot check's.
    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        let scope = scope_of(params.maintain().linux_scope)?;
        let upgrader = self.upgrader.as_deref().ok_or_else(|| KindError::Failed {
            reason: REASON_NO_PACKAGE_MANAGER,
            detail: NO_PACKAGE_MANAGER_DETAIL.to_owned(),
        })?;
        let own_packages = self.own.names().map_err(|why| KindError::Failed {
            reason: REASON_OWN_PACKAGES_UNREADABLE,
            detail: format!("own packages: {why}"),
        })?;
        upgrader.upgrade(&Upgrade {
            scope,
            own_packages: &own_packages,
        })?;
        if self.check.needed()? {
            return Ok(Cutover::NeedsReboot);
        }
        Ok(Cutover::Applied {
            version: self.check.running()?,
        })
    }

    /// The kernel the host runs after the reboot.
    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        self.check.running()
    }

    /// Not written here: [`Unsupported`]. On a host the hook checks the kind with
    /// [`crate::maintenance_hook::LinuxCheck`] instead.
    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Linux).verify(params)
    }
}

/// The `reboot` kind: a reboot and nothing installed.
pub struct RebootKind {
    check: RebootCheck,
}

impl RebootKind {
    /// The kind asking `check` for the kernel the host runs after the reboot.
    #[must_use]
    pub fn new(check: RebootCheck) -> RebootKind {
        RebootKind { check }
    }

    /// The kind of this host.
    #[must_use]
    pub fn at_host() -> RebootKind {
        RebootKind::new(RebootCheck::at_host())
    }
}

impl MaintenanceKind for RebootKind {
    /// Not written yet: [`Unsupported`].
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Reboot).prepare(params)
    }

    /// Always [`Cutover::NeedsReboot`]; no program is run.
    fn cutover(&self, _params: &MaintainParams) -> Result<Cutover, KindError> {
        Ok(Cutover::NeedsReboot)
    }

    /// The kernel the host runs after the reboot.
    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        self.check.running()
    }

    /// Not written here: [`Unsupported`]. On a host the hook checks the kind with
    /// [`crate::maintenance_hook::LinuxCheck`] instead.
    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        Unsupported(Kind::Reboot).verify(params)
    }
}
