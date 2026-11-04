//! The start-up reconciliation of placed paths (design section 6.5; section 8.10, "Crash
//! mid-link"), run by the daemon at start before any unit and before any step record is resumed:
//!
//! 1. the temporary names this crate writes (`.hk-*.tmp`) are removed from every directory it
//!    manages: the state directory, the unit directory, each directory a recorded path is in,
//!    and each service's package directory;
//! 2. for each service the record lists, or a [`SwitchIntent`] names, that is not tombstoned:
//!    - a `pending` path occupied by anything but a link into this service's package directory
//!      is reported as a [`Conflict`], and nothing of that service is touched;
//!    - with an intent, the switch of `current` is finished to the recorded side or undone;
//!    - every path the manifest of the version `current` names places is linked, by begin, link
//!      and commit, and every other path of the service is removed, as
//!      [`Placer::relink_to`] does; this also completes a switch of `current` made by hand;
//! 3. `daemon-reload` runs once if any link or switch changed.
//!
//! A service whose version cannot be read (no `current`, a manifest that does not load) is
//! reported in [`ReconcileReport::unresolved`] and left as it is. Nothing here reads a clock.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};

use crate::check::{check, occupant};
use crate::error::{Owner, PlacedError, io_at};
use crate::placer::{Placer, is_temporary};
use crate::store::{PathState, PlacedRecord};

/// Which side of a switch a crash is to be finished to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// To the target: the switch is finished.
    Forward,
    /// To the version switched from: the switch is undone.
    Back,
}

/// A switch the step record or the force record says was under way: the caller reads it from
/// there and passes it in; this crate reads neither record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchIntent {
    pub service: ServiceName,
    /// The version switched from; `None` for a first install, which has none.
    pub from: Option<Version>,
    pub target: Version,
    pub side: Side,
}

/// What the daemon knows at start that reconciliation needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootView {
    pub intents: Vec<SwitchIntent>,
    /// The running daemon's version, against which each manifest's `min_hostkeeper` is read.
    pub running: HostkeeperVersion,
}

/// A path that blocks the reconciliation of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub service: String,
    pub path: PathBuf,
    pub owner: Owner,
}

/// A service reconciliation left as it is, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unresolved {
    pub service: String,
    pub why: String,
}

/// What a reconciliation did, for the daemon's log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// The temporary names removed.
    pub temporaries_removed: Vec<PathBuf>,
    /// Each service whose `current` was switched, with the version it now names.
    pub switched: Vec<(String, String)>,
    /// The paths whose link was written.
    pub linked: Vec<PathBuf>,
    /// The paths removed and dropped from the record.
    pub removed: Vec<PathBuf>,
    /// The paths the record listed whose content was not the recorded link: left as they are.
    pub left_alone: Vec<PathBuf>,
    /// The units stopped because their version no longer lists them.
    pub stopped: Vec<String>,
    /// The `path_conflict` cases: each such service was not touched.
    pub conflicts: Vec<Conflict>,
    pub unresolved: Vec<Unresolved>,
    /// Whether `daemon-reload` ran.
    pub reloaded: bool,
}

impl ReconcileReport {
    /// Whether a link or a switch changed, so that `daemon-reload` is due.
    pub fn changed(&self) -> bool {
        !self.switched.is_empty() | !self.linked.is_empty() | !self.removed.is_empty()
    }
}

impl Placer<'_> {
    /// Reconciles every placed path with the version `current` names, finishing or undoing the
    /// switches of `view`'s intents first. Run at daemon start, before any unit.
    pub fn reconcile(&self, view: &BootView) -> Result<ReconcileReport, PlacedError> {
        let _lock = self.lock()?;
        let mut record = self.store().load()?;
        let mut report = ReconcileReport::default();
        let services: BTreeSet<String> = record
            .paths
            .values()
            .map(|entry| entry.service.clone())
            .chain(view.intents.iter().map(|intent| intent.service.to_string()))
            .filter(|service| !record.uninstalling.contains_key(service))
            .collect();
        self.remove_temporaries(&record, &services, &mut report)?;
        for service in &services {
            self.reconcile_service(&mut record, service, view, &mut report)?;
        }
        if report.changed() {
            self.units.daemon_reload()?;
            report.reloaded = true;
        }
        Ok(report)
    }

    /// Removes the temporary names from every directory this crate writes in.
    fn remove_temporaries(
        &self,
        record: &PlacedRecord,
        services: &BTreeSet<String>,
        report: &mut ReconcileReport,
    ) -> Result<(), PlacedError> {
        let packages = self.roots.packages();
        let dirs: BTreeSet<PathBuf> = record
            .paths
            .keys()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .chain([
                self.roots.state_dir().to_path_buf(),
                self.roots.unit_dir().to_path_buf(),
            ])
            .chain(services.iter().map(|service| packages.service(service)))
            .collect();
        for dir in dirs {
            let names = match self.fs.list(&dir) {
                Ok(names) => names,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at("list", &dir)(e)),
            };
            for name in names {
                if !is_temporary(&name.to_string_lossy()) {
                    continue;
                }
                let path = dir.join(&name);
                self.fs.remove_file(&path).map_err(io_at("remove", &path))?;
                report.temporaries_removed.push(path);
            }
        }
        Ok(())
    }

    /// The `pending` paths of `service` held by anything but a link into its package
    /// directory or the link the record lists.
    fn pending_conflicts(
        &self,
        record: &PlacedRecord,
        service: &str,
    ) -> Result<Vec<Conflict>, PlacedError> {
        let own = self.roots.packages().service(service);
        let mut conflicts = Vec::new();
        let pending = record
            .of_service(service)
            .filter(|(_, entry)| entry.state == PathState::Pending);
        for (path, entry) in pending {
            let owner = match occupant(self.fs, path)? {
                Some(Owner::Link(target))
                    if (target == entry.link_target) | target.starts_with(&own) =>
                {
                    None
                }
                other => other,
            };
            conflicts.extend(owner.map(|owner| Conflict {
                service: service.to_owned(),
                path: path.clone(),
                owner,
            }));
        }
        Ok(conflicts)
    }

    /// The version `service` is to run after reconciliation, switching `current` to it when an
    /// intent says so. `Ok(None)` for an intent undone to no version (a first install).
    fn chosen_version(
        &self,
        service: &str,
        intent: Option<&SwitchIntent>,
        report: &mut ReconcileReport,
    ) -> Result<Result<Option<String>, String>, PlacedError> {
        let Some(intent) = intent else {
            return Ok(self
                .current(service)?
                .map(Some)
                .ok_or_else(|| "it has no `current` link".to_owned()));
        };
        let side = match intent.side {
            Side::Forward => Some(&intent.target),
            Side::Back => intent.from.as_ref(),
        };
        if let Some(version) = side
            && self.switch_current(&intent.service, version)?
        {
            report
                .switched
                .push((service.to_owned(), version.to_string()));
        }
        Ok(Ok(side.map(Version::to_string)))
    }

    /// The manifest of `version` of `service`, loaded from its version directory.
    fn manifest_of(
        &self,
        service: &str,
        version: &str,
        running: &HostkeeperVersion,
    ) -> Result<Manifest, String> {
        let expected = Expected {
            name: ServiceName::new(service).map_err(|e| e.to_string())?,
            version: Version::new(version).map_err(|e| e.to_string())?,
        };
        let dir = self.roots.packages().version(service, version);
        Manifest::load(&self.fs.host_path(&dir), &expected, running)
            .map_err(|e| format!("the manifest of {version}: {e}"))
    }

    fn reconcile_service(
        &self,
        record: &mut PlacedRecord,
        service: &str,
        view: &BootView,
        report: &mut ReconcileReport,
    ) -> Result<(), PlacedError> {
        let conflicts = self.pending_conflicts(record, service)?;
        if !conflicts.is_empty() {
            report.conflicts.extend(conflicts);
            return Ok(());
        }
        let unresolved = |why: String| Unresolved {
            service: service.to_owned(),
            why,
        };
        let intent = view
            .intents
            .iter()
            .find(|intent| intent.service.as_str() == service);
        let version = match self.chosen_version(service, intent, report)? {
            Ok(version) => version,
            Err(why) => {
                report.unresolved.push(unresolved(why));
                return Ok(());
            }
        };
        let mut keep = BTreeSet::new();
        if let Some(version) = version {
            let desired = match self
                .manifest_of(service, &version, &view.running)
                .and_then(|manifest| self.desired(&manifest).map_err(|e| e.to_string()))
            {
                Ok(desired) => desired,
                Err(why) => {
                    report.unresolved.push(unresolved(why));
                    return Ok(());
                }
            };
            if let Err(PlacedError::PathConflict { path, owner }) = check(&desired, record, self.fs)
            {
                report.conflicts.push(Conflict {
                    service: service.to_owned(),
                    path,
                    owner,
                });
                return Ok(());
            }
            self.begin_in(record, &desired)?;
            for claim in desired.iter() {
                if self.ensure_link(&claim.path, &claim.link_target)? {
                    report.linked.push(claim.path.clone());
                }
            }
            self.commit_in(record, &desired)?;
            keep = desired.paths();
        }
        let (removal, _) = self.prune_in(record, service, &keep, true)?;
        report.stopped.extend(removal.stopped);
        report.removed.extend(removal.removed);
        report.left_alone.extend(removal.left_alone);
        Ok(())
    }
}
