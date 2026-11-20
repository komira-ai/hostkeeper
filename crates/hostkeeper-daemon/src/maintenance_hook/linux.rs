//! The check of the Linux kinds of the maintenance hook, `linux` and `reboot`: their `verify` is
//! [`LinuxCheck`] (docs/design/maintenance-updates.md, section 5.6), put in front of the kinds that
//! install and reboot ([`crate::maintenance_linux`]) by [`kinds`].
//!
//! **What the check reads.** The collectors of the maintenance look, run again
//! ([`crate::maintenance_collect`]): the host's facts ([`HostFacts`]: `uname -r` and the newest
//! kernel under `/lib/modules`), the reboot the host says it needs ([`RebootNeed`]), and for
//! `linux` the updates its package manager lists ([`package_manager`]) under the order's
//! `linux_scope`, leaving out the packages hostkeeper itself installs ([`OwnPackages`]); and the
//! health of every installed service as its heartbeat reports it ([`ServiceHealth`]: the
//! `health` of a `ServiceBeat`, as [`beat_health`] gives it, over the daemon's observer).
//!
//! **What it fails on.** The first wrong fact, in this order, each named in the detail (which
//! the hook bounds to its [`MAX_DETAIL`](super::MAX_DETAIL)):
//!
//! | Fact | Reason |
//! |---|---|
//! | the running kernel is older than the newest installed | [`REASON_KERNEL_NOT_NEWEST`] |
//! | the host still says it needs a reboot | [`REASON_REBOOT_NEEDED`] |
//! | for `linux`: an update of the order's scope is still due | [`REASON_UPDATE_DUE`] |
//! | an installed service is unhealthy or absent | [`REASON_SERVICE_UNHEALTHY`] |
//!
//! A collector that cannot read the host fails [`REASON_FACTS_UNREADABLE`], naming it as a
//! failed look does; services whose health cannot be listed fail [`REASON_HEALTH_UNREADABLE`].
//! Before any of this the hook itself checked the kind recorded applied and the host booted
//! again when the kind needed it (`not_applied`, `not_rebooted`, in [`super`]).
//!
//! A `reboot` kind in a run that also holds `linux` checks nothing more: the hook checks `linux`
//! first, and the `linux` check is the `reboot` check and more, so the host is read once.
//!
//! **The stage.** The `prepare` of `linux` is its stage, in the submodule `stage`
//! (docs/design/maintenance-updates.md, section 5.2 step 1): the free space checked, then a
//! download alone with `apt-get`, `dnf` or `apk`, nothing installed. The kind that holds it is
//! [`crate::maintenance_linux::kinds::LinuxKind`].

mod apk;
mod apt;
mod dnf;
pub(crate) mod stage;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_observe::{Observer, beat_health};
use hostkeeper_placed::roots::STATE_DIR;
use hostkeeper_placed::{RealFs, Roots};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::agent::v1::Health;

use super::kind::{Cutover, KindError, MaintenanceKind};
use super::params::{Kind, MaintainParams};
use super::registry::Registry;
use crate::Placement;
use crate::adapters::Adapters;
use crate::cli::{DEFAULT_PACKAGES_DIR, DEFAULT_STATE_DIR};
use crate::installed;
use crate::maintenance_collect::{
    Collector, HostRoot, LinuxScope, Look, Section, compare_versions,
};
use crate::maintenance_collectors::package_manager;
use crate::maintenance_host::HostFacts;
use crate::maintenance_own::{OwnPackages, PlacedPackages};
use crate::maintenance_reboot::RebootNeed;
use crate::observer::{self, OwnService, observer_with};
use crate::selfupdate::binary_dir;

/// The reason of a collector that could not read the host.
pub const REASON_FACTS_UNREADABLE: &str = "facts_unreadable";
/// The reason of a host whose running kernel is older than the newest installed.
pub const REASON_KERNEL_NOT_NEWEST: &str = "kernel_not_newest";
/// The reason of a host that still says it needs a reboot.
pub const REASON_REBOOT_NEEDED: &str = "reboot_needed";
/// The reason of an update of the order's scope that is still due.
pub const REASON_UPDATE_DUE: &str = "update_due";
/// The reason of an installed service that is unhealthy or absent.
pub const REASON_SERVICE_UNHEALTHY: &str = "service_unhealthy";
/// The reason of services whose health could not be listed.
pub const REASON_HEALTH_UNREADABLE: &str = "health_unreadable";

/// The health of the services a host runs, as its heartbeat reports them.
pub trait ServiceHealth: Send + Sync {
    /// Each installed service's name and its health (the `health` of its `ServiceBeat`), in name
    /// order.
    ///
    /// # Errors
    /// Why the services could not be listed.
    fn services(&self) -> Result<Vec<(String, Health)>, String>;
}

/// The services of a host's package directory, each observed by the daemon's observer as a
/// status read observes it ([`observer::observe`]); a service the observer has no input for (no
/// `current`, or a manifest that does not load) is left out, as the heartbeat leaves it out.
pub struct HostServices {
    packages: PathBuf,
    binaries: PathBuf,
    own: HostkeeperVersion,
    observer: Arc<Observer>,
}

impl HostServices {
    /// The services under `packages`, hostkeeper's own binary directory `binaries`, observed by
    /// `observer` for a daemon of version `own`.
    #[must_use]
    pub fn new(
        packages: PathBuf,
        binaries: PathBuf,
        own: HostkeeperVersion,
        observer: Arc<Observer>,
    ) -> HostServices {
        HostServices {
            packages,
            binaries,
            own,
            observer,
        }
    }
}

impl ServiceHealth for HostServices {
    fn services(&self) -> Result<Vec<(String, Health)>, String> {
        let names = installed::service_dirs(&self.packages)
            .map_err(|error| format!("{}: {error}", self.packages.display()))?;
        Ok(names
            .into_values()
            .filter_map(|service| {
                let observation = observer::observe(
                    &self.observer,
                    &self.packages,
                    &self.binaries,
                    &self.own,
                    &service,
                )?;
                Some((service.to_string(), beat_health(&observation).0))
            })
            .collect())
    }
}

/// The check of the Linux kinds: what it reads, as ports.
pub struct LinuxCheck {
    host: Box<dyn Collector>,
    reboot: Box<dyn Collector>,
    packages: Option<Box<dyn Collector>>,
    own: Box<dyn OwnPackages>,
    services: Box<dyn ServiceHealth>,
}

impl LinuxCheck {
    /// The check over the collectors `host` (the kernels), `reboot` (the reboot the host says it
    /// needs) and `packages` (the package manager's updates; none on a host with no package
    /// manager), the packages `own` names, and the services' health `services`.
    #[must_use]
    pub fn new(
        host: Box<dyn Collector>,
        reboot: Box<dyn Collector>,
        packages: Option<Box<dyn Collector>>,
        own: Box<dyn OwnPackages>,
        services: Box<dyn ServiceHealth>,
    ) -> LinuxCheck {
        LinuxCheck {
            host,
            reboot,
            packages,
            own,
            services,
        }
    }

    /// The check of this host over the platform's `adapters`, for a daemon of version `own`: the
    /// host's own collectors and files, the packages the `placed.json` of the default state
    /// directory records, and the services of the default package directory, observed as the
    /// daemon observes them.
    #[must_use]
    pub fn at_host(adapters: Adapters, own: HostkeeperVersion) -> LinuxCheck {
        let packages = PathBuf::from(DEFAULT_PACKAGES_DIR);
        let binaries = binary_dir(Path::new(DEFAULT_STATE_DIR));
        let placement = Placement {
            roots: Roots::new(Path::new(STATE_DIR), &adapters.unit_dir, &packages),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        };
        let observer = observer_with(
            adapters.manager,
            adapters.process,
            &packages,
            own,
            &OwnService {
                unit: adapters.own_unit,
                binary_dir: binaries.clone(),
            },
        );
        LinuxCheck::new(
            Box::new(HostFacts::at_host()),
            Box::new(RebootNeed::at_host()),
            package_manager(&HostRoot::host()),
            Box::new(PlacedPackages::new(&placement)),
            Box::new(HostServices::new(
                packages,
                binaries,
                own,
                Arc::new(observer),
            )),
        )
    }

    /// Checks `kind` of the run `params` (the module's header): the first wrong fact fails.
    ///
    /// # Errors
    /// [`KindError::Failed`] with the reason of the first wrong fact and what was found.
    pub fn verify(&self, kind: Kind, params: &MaintainParams) -> Result<(), KindError> {
        if kind == Kind::Reboot && params.kinds().any(|other| other == Kind::Linux) {
            return Ok(());
        }
        let scope = scope_of(params);
        let none = BTreeSet::new();
        let look = Look {
            scope,
            own_packages: &none,
        };
        let host = collect(self.host.as_ref(), &look)?;
        let (running, newest) = (&host.host.running_kernel, &host.host.newest_kernel);
        if compare_versions(running, newest).is_lt() {
            return Err(failed(
                REASON_KERNEL_NOT_NEWEST,
                format!("the host runs kernel {running}, and {newest} is installed"),
            ));
        }
        let reboot = collect(self.reboot.as_ref(), &look)?;
        if let Some(item) = first_due(&reboot, pb::MaintenanceKind::Reboot) {
            return Err(failed(
                REASON_REBOOT_NEEDED,
                format!("the host still needs a reboot: {}", item.label),
            ));
        }
        if kind == Kind::Linux
            && let Some(packages) = &self.packages
        {
            let own = self.own.names().map_err(|error| {
                failed(REASON_FACTS_UNREADABLE, format!("own packages: {error}"))
            })?;
            let look = Look {
                scope,
                own_packages: &own,
            };
            let updates = collect(packages.as_ref(), &look)?;
            if let Some(item) = first_due(&updates, pb::MaintenanceKind::Linux) {
                return Err(failed(
                    REASON_UPDATE_DUE,
                    format!("{} {} is still due", item.label, item.to),
                ));
            }
        }
        let services = self
            .services
            .services()
            .map_err(|error| failed(REASON_HEALTH_UNREADABLE, error))?;
        let unwell = services
            .into_iter()
            .find_map(|(service, health)| match health {
                Health::Healthy | Health::Unspecified => None,
                Health::Unhealthy => Some((service, "unhealthy")),
                Health::Absent => Some((service, "absent")),
            });
        match unwell {
            Some((service, word)) => Err(failed(
                REASON_SERVICE_UNHEALTHY,
                format!("the service {service} is {word}"),
            )),
            None => Ok(()),
        }
    }
}

/// The scope of the run `params`: [`LinuxScope::All`] for `LINUX_SCOPE_ALL`, else
/// [`LinuxScope::Security`], the default.
fn scope_of(params: &MaintainParams) -> LinuxScope {
    match pb::LinuxScope::try_from(params.maintain().linux_scope) {
        Ok(pb::LinuxScope::All) => LinuxScope::All,
        _ => LinuxScope::Security,
    }
}

/// What `collector` finds under `look`, or the failure naming it.
fn collect(collector: &dyn Collector, look: &Look<'_>) -> Result<Section, KindError> {
    collector.collect(look).map_err(|reason| {
        failed(
            REASON_FACTS_UNREADABLE,
            format!("{}: {reason}", collector.name()),
        )
    })
}

/// The first item of `section` due of `kind`.
fn first_due(section: &Section, kind: pb::MaintenanceKind) -> Option<&pb::DueItem> {
    section.due.iter().find(|item| item.kind == i32::from(kind))
}

/// The failure for `reason`, saying `detail`.
fn failed(reason: &'static str, detail: String) -> KindError {
    KindError::Failed { reason, detail }
}

/// One Linux kind: its `prepare`, `cutover` and `after_reboot` are `work`'s, its `verify` the
/// shared [`LinuxCheck`].
struct Checked {
    kind: Kind,
    work: Box<dyn MaintenanceKind>,
    check: Arc<LinuxCheck>,
}

impl MaintenanceKind for Checked {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.work.prepare(params)
    }

    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        self.work.cutover(params)
    }

    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError> {
        self.work.after_reboot(params)
    }

    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        self.check.verify(self.kind, params)
    }
}

/// The kinds of a Linux host, `linux` and `reboot`: the steps but `verify` of each are `linux`'s
/// and `reboot`'s (on a host, [`crate::maintenance_linux`]'s kinds), and both are checked by
/// `check`.
#[must_use]
pub fn kinds(
    check: LinuxCheck,
    linux: Box<dyn MaintenanceKind>,
    reboot: Box<dyn MaintenanceKind>,
) -> Registry {
    let check = Arc::new(check);
    [(Kind::Linux, linux), (Kind::Reboot, reboot)]
        .into_iter()
        .fold(Registry::new(), |registry, (kind, work)| {
            registry.with(
                kind,
                Box::new(Checked {
                    kind,
                    work,
                    check: Arc::clone(&check),
                }),
            )
        })
}
