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

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};

use crate::check::{check, occupant};
use crate::error::{Owner, PlacedError, io_at};
use crate::placer::{PlacedLock, Placer, is_temporary};
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
    /// Each service whose `current` was removed: a first install undone.
    pub current_removed: Vec<String>,
    /// The `path_conflict` cases: each such service was not touched.
    pub conflicts: Vec<Conflict>,
    pub unresolved: Vec<Unresolved>,
    /// Whether `daemon-reload` ran.
    pub reloaded: bool,
}

impl ReconcileReport {
    /// Whether a link or a switch changed, so that `daemon-reload` is due.
    pub fn changed(&self) -> bool {
        !self.switched.is_empty()
            | !self.current_removed.is_empty()
            | !self.linked.is_empty()
            | !self.removed.is_empty()
    }
}

impl Placer<'_> {
    /// Reconciles every placed path with the version `current` names, finishing or undoing the
    /// switches of `view`'s intents first. Run at daemon start, before any unit. Two different
    /// intents for one service are [`PlacedError::ConflictingIntents`], before anything is
    /// written; an intent given twice counts once. `daemon-reload` runs once when anything
    /// changed or a reload is owed in the record (a crash or a failed reload before), and the
    /// reload owed is cleared after it.
    pub fn reconcile(&self, view: &BootView) -> Result<ReconcileReport, PlacedError> {
        let mut intents: BTreeMap<&str, &SwitchIntent> = BTreeMap::new();
        for intent in &view.intents {
            let service = intent.service.as_str();
            if intents
                .insert(service, intent)
                .is_some_and(|before| before != intent)
            {
                return Err(PlacedError::ConflictingIntents {
                    service: service.to_owned(),
                });
            }
        }
        let lock = self.lock()?;
        let mut record = self.store().load()?;
        let mut report = ReconcileReport::default();
        let services: BTreeSet<String> = record
            .paths
            .values()
            .map(|entry| entry.service.clone())
            .chain(intents.keys().map(|service| (*service).to_owned()))
            .filter(|service| !record.uninstalling.contains_key(service))
            .collect();
        self.remove_temporaries(&record, &services, &mut report)?;
        for service in &services {
            let intent = intents.get(service.as_str()).copied();
            self.reconcile_service(&mut record, service, intent, view, &mut report, &lock)?;
        }
        if report.changed() | record.reload_pending {
            self.units.daemon_reload()?;
            self.reloaded(&lock)?;
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

    /// The version `service` is to run after reconciliation, without switching anything: the
    /// intent's side, or the version `current` names. `Ok(Err(why))` when it cannot be known (no
    /// `current`, a `current` or `previous` that is not a link to a version beside it), and
    /// `Ok(Ok(None))` for an intent undone to no version (a first install).
    fn chosen_version(
        &self,
        service: &str,
        intent: Option<&SwitchIntent>,
    ) -> Result<Result<Option<String>, String>, PlacedError> {
        let links = self.current(service).and_then(|current| {
            self.previous(service)?;
            Ok(current)
        });
        let current = match links {
            Ok(current) => current,
            Err(PlacedError::PathConflict { path, owner }) => {
                return Ok(Err(format!("{} is held by {owner}", path.display())));
            }
            Err(e) => return Err(e),
        };
        Ok(match intent {
            None => current
                .map(Some)
                .ok_or_else(|| "it has no `current` link".to_owned()),
            Some(intent) => Ok(match intent.side {
                Side::Forward => Some(intent.target.to_string()),
                Side::Back => intent.from.as_ref().map(Version::to_string),
            }),
        })
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
            platform: hostkeeper_config::platform::Platform::current(),
        };
        let dir = self.roots.packages().version(service, version);
        Manifest::load(&self.fs.host_path(&dir), &expected, running)
            .map_err(|e| format!("the manifest of {version}: {e}"))
    }

    /// Removes `current` of `service` when it names `target`: the undoing of a first install's
    /// switch. A `current` naming anything else is left. `true` when it was removed.
    fn unswitch(&self, service: &str, target: &Version) -> Result<bool, PlacedError> {
        if self.current(service)?.as_deref() != Some(target.as_str()) {
            return Ok(false);
        }
        let dir = self.roots.packages().service(service);
        let link = dir.join("current");
        self.fs.remove_file(&link).map_err(io_at("remove", &link))?;
        self.fs.sync_dir(&dir).map_err(io_at("fsync", &dir))?;
        Ok(true)
    }

    fn reconcile_service(
        &self,
        record: &mut PlacedRecord,
        service: &str,
        intent: Option<&SwitchIntent>,
        view: &BootView,
        report: &mut ReconcileReport,
        lock: &PlacedLock,
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
        let version = match self.chosen_version(service, intent)? {
            Ok(version) => version,
            Err(why) => {
                report.unresolved.push(unresolved(why));
                return Ok(());
            }
        };
        let Some(version) = version else {
            // A first install undone: every path of the service goes, then `current`.
            let (removal, _) = self.prune_in(record, service, &BTreeSet::new(), true, lock)?;
            report.stopped.extend(removal.stopped);
            report.removed.extend(removal.removed);
            report.left_alone.extend(removal.left_alone);
            // Only an intent undoes to no version, so there is always one here.
            let unswitched = intent
                .map(|intent| self.unswitch(service, &intent.target))
                .transpose()?;
            if unswitched == Some(true) {
                report.current_removed.push(service.to_owned());
            }
            return Ok(());
        };
        // Everything is judged before anything changes: the manifest, its claims and lint, and
        // every destination against the record and the disk.
        let desired =
            match self
                .manifest_of(service, &version, &view.running)
                .and_then(|manifest| {
                    self.desired_in(&manifest, record)
                        .map_err(|e| e.to_string())
                }) {
                Ok(desired) => desired,
                Err(why) => {
                    report.unresolved.push(unresolved(why));
                    return Ok(());
                }
            };
        match check(&desired, record, self.fs) {
            Ok(()) => {}
            Err(PlacedError::PathConflict { path, owner }) => {
                report.conflicts.push(Conflict {
                    service: service.to_owned(),
                    path,
                    owner,
                });
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        if let Some(intent) = intent
            && self.switch_current(&intent.service, desired.version())?
        {
            report.switched.push((service.to_owned(), version.clone()));
        }
        self.begin_in(record, &desired, lock)?;
        for claim in desired.iter() {
            if self.ensure_link(&claim.path, &claim.link_target, service)? {
                report.linked.push(claim.path.clone());
            }
        }
        self.commit_in(record, &desired, lock)?;
        let (removal, _) = self.prune_in(record, service, &desired.paths(), true, lock)?;
        report.stopped.extend(removal.stopped);
        report.removed.extend(removal.removed);
        report.left_alone.extend(removal.left_alone);
        Ok(())
    }
}
