//! What the daemon's start does around the package manager's host, at every start (boot or
//! restart): it removes the `policy-rc.d` file a killed run left, and starts the run units of
//! repository services.
//!
//! [`Daemon::clear_policy_rc_d`] runs first, before any package-manager call of the start: it
//! removes the host root's `policy-rc.d` when it is byte-equal to the one hostkeeper writes
//! ([`policy::clear_stale`]), and nothing else there. A failure is an alert, and the start goes on.
//!
//! An apt package enables its own units: the installer writes the `WantedBy=` links of a run unit
//! whose unit file has an `[Install]` target, and systemd starts it at boot. A run unit the tree
//! gives no such link ([`hostkeeper_install::repository::enable::unlinked`]) has nothing that
//! starts it after a reboot, so hostkeeper does: [`Daemon::start_run_units`] starts each of them,
//! from the `current` version of each service staged by the `apt` installer, before the `boot`
//! steps. [`UnitControl::start_unit`] on a unit already active changes nothing, so a restart of the
//! daemon alone disturbs no running service.
//!
//! A service whose update the start pass left unfinished or held is skipped, as is hostkeeper's
//! own package (its unit runs this daemon) and a version with no `target.json` or staged by
//! another installer. A unit that cannot be started is an alert (`start_incomplete`, with the
//! service and the unit), and the start goes on.
//!
//! Then the units of an archive version the start opened: a version an earlier release unpacked
//! from hostkeeper's own archive is the tree its service runs from, as the service's own user, and
//! a release closed it to root, so that the service could not start again after a restart or a
//! reboot (`status=203/EXEC`, and systemd gives up). When the start gave such a version, the one
//! `current` names, its layout's mode ([`crate::package_dirs`]), the run units that the archive enabled and that are
//! enabled still ([`hostkeeper_placed::Placer::archive_enabled_units`]) are started again, in the
//! same stage, under the same rules: the service's update not unfinished or held, hostkeeper's
//! own package skipped, a unit already active left as it is, and a unit that cannot be started an
//! alert. A version whose mode was already its layout's starts nothing, so this runs once, at the
//! start that opens the version. A `placed.json` that cannot be read is an alert for the service,
//! and nothing of it is started.

use std::path::Path;

use hostkeeper_config::names::ServiceName;
use hostkeeper_install::repository::{TREE, enable, policy};

use crate::boot::{ALERT_START, BootReport, Outcome, Stage, current_version};
use crate::executor::Daemon;
use crate::installed;
use crate::target;
use crate::verbs::force::SELF_SERVICE;

/// The installer whose versions this start pass looks at.
const APT: &str = "apt";

impl Daemon {
    /// Removes the stale `policy-rc.d` of the host's root, and records the stage.
    pub fn clear_policy_rc_d(&self, report: &mut BootReport) {
        let ctx = self.ctx();
        report.stages.push(Stage::ClearPolicyRcD);
        match policy::clear_stale(&ctx.repository_ports.dirs.root) {
            Ok(true) => ctx.log(&format!(
                "level=info removed the stale {} that a killed run of the package manager left",
                policy::POLICY_RC_D
            )),
            Ok(false) => {}
            Err(error) => ctx.alert(
                ALERT_START,
                &format!("the stale policy-rc.d could not be removed: {error}"),
            ),
        }
    }

    /// Starts the run units the tree of each apt service's `current` version gives no enablement
    /// link, and records the stage.
    pub fn start_run_units(&self, report: &mut BootReport) {
        report.stages.push(Stage::StartRunUnits);
        let ctx = self.ctx();
        let names = installed::service_dirs(&ctx.dirs.packages).unwrap_or_default();
        for service in names.into_values() {
            let free = report
                .services
                .get(service.as_str())
                .is_none_or(|s| matches!(s.outcome, Outcome::Idle | Outcome::Finished(_)));
            if !free | (service.as_str() == SELF_SERVICE) {
                continue;
            }
            for unit in self.unlinked_units(&service) {
                if let Err(error) = ctx.units.start_unit(&unit) {
                    ctx.alert(
                        ALERT_START,
                        &format!("service={service} unit={unit}: it could not be started: {error}"),
                    );
                }
            }
        }
        self.start_archive_units(report);
    }

    /// Starts again the run units the archive enabled of each service whose archive version the
    /// start opened ([the module](self)), and records each unit started in the report.
    fn start_archive_units(&self, report: &mut BootReport) {
        let ctx = self.ctx();
        for service in report.package_dirs.opened.clone() {
            let free = report
                .services
                .get(&service)
                .is_none_or(|s| matches!(s.outcome, Outcome::Idle | Outcome::Finished(_)));
            if !free | (service == SELF_SERVICE) {
                continue;
            }
            let units = match ctx.placer().archive_enabled_units(&service) {
                Ok(units) => units,
                Err(error) => {
                    ctx.alert(
                        ALERT_START,
                        &format!(
                            "service={service}: the units its archive version enabled cannot be \
                             read, so none is started: {error}"
                        ),
                    );
                    continue;
                }
            };
            for unit in units {
                match ctx.units.start_unit(&unit) {
                    Ok(()) => report.archive_units_started.push(unit),
                    Err(error) => ctx.alert(
                        ALERT_START,
                        &format!("service={service} unit={unit}: it could not be started: {error}"),
                    ),
                }
            }
        }
    }

    /// The run units of `service`'s `current` version that its tree gives no link; none when the
    /// version is not an apt one or cannot be read.
    fn unlinked_units(&self, service: &ServiceName) -> Vec<String> {
        let ctx = self.ctx();
        let package_dir = ctx.dirs.packages.join(service.as_str());
        let Ok(Some(version)) = current_version(&package_dir) else {
            return Vec::new();
        };
        let dir = package_dir.join(version.as_str());
        let Ok(Some(entry)) = target::read(&dir) else {
            return Vec::new();
        };
        if entry.installer != APT {
            return Vec::new();
        }
        let run = entry.manifest.map(|m| m.run).unwrap_or_default();
        enable::unlinked(&tree_of(&dir), &run)
    }
}

/// The extracted tree of the version directory `dir`.
fn tree_of(dir: &Path) -> std::path::PathBuf {
    dir.join(TREE)
}
