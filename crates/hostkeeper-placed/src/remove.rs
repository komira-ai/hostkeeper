//! Removal of placed paths (design section 4.7, "Placed paths" and "Uninstall"; section 5.6,
//! step 10): the paths a version no longer lists, after `cleanup` of a hooked update or at step
//! 10 of a force, and every path of a service on uninstall.
//!
//! For a unit no longer listed the order is: stop, disable (its `WantedBy=` links and their
//! entries), remove its link, then one `daemon-reload` for the whole removal. A path whose
//! content on disk is not the link the record lists is never deleted: it is left as it is,
//! reported, and dropped from the record, which no longer owns it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::ServiceName;

use crate::check::occupant;
use crate::error::{Owner, PlacedError, io_at};
use crate::placer::{PlacedLock, Placer, file_name_of};
use crate::store::{Entry, PathKind, PlacedRecord};

/// What a removal did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removal {
    /// The units stopped, in the order they were.
    pub stopped: Vec<String>,
    /// The paths whose link was removed, or was already gone, and that were dropped from the
    /// record.
    pub removed: Vec<PathBuf>,
    /// The paths the record listed whose content is not the recorded link: left on disk as they
    /// are, and dropped from the record.
    pub left_alone: Vec<PathBuf>,
    /// Whether `daemon-reload` ran.
    pub reloaded: bool,
}

/// What is at a path the record lists, against the link it lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// Nothing.
    Absent,
    /// The recorded link.
    Ours,
    /// Anything else.
    Foreign,
}

/// The unit to stop before the path of `entry` at `path` goes: the unit of a `WantedBy=` link,
/// or a unit file that is not a template (a template's instances are stopped by their links).
fn stop_name(path: &Path, entry: &Entry) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    match entry.kind {
        PathKind::WantsLink => Some(name),
        PathKind::Unit => (!name.contains("@.")).then_some(name),
        PathKind::File => None,
    }
}

impl Placer<'_> {
    /// Removes the link at `path`, if one is there, and syncs its directory.
    fn remove_link(&self, path: &Path, found: Found) -> Result<(), PlacedError> {
        if found == Found::Absent {
            return Ok(());
        }
        let dir = path.parent().unwrap_or(Path::new("/"));
        self.fs.remove_file(path).map_err(io_at("remove", path))?;
        self.fs.sync_dir(dir).map_err(io_at("fsync", dir))
    }

    /// What is at the recorded path `path` against the link `entry` lists.
    fn found(&self, path: &Path, entry: &Entry) -> Result<Found, PlacedError> {
        Ok(match occupant(self.fs, path)? {
            None => Found::Absent,
            Some(Owner::Link(target)) if target == entry.link_target => Found::Ours,
            Some(_) => Found::Foreign,
        })
    }

    /// Removes every path of `service` in `record` that is not in `keep`, stopping first, when
    /// `stop`, the units that go and that the service does not keep under another path (a unit
    /// that loses one `WantedBy=` link but keeps another, or its unit file, is not stopped), and
    /// writes the record once, with a `daemon-reload` owed when a unit or `WantedBy=` link went.
    /// Each path is looked at again after the stops, so one replaced meanwhile is left alone.
    /// The returned flag says whether the reload is owed. No reload runs here.
    pub(crate) fn prune_in(
        &self,
        record: &mut PlacedRecord,
        service: &str,
        keep: &BTreeSet<PathBuf>,
        stop: bool,
        lock: &PlacedLock,
    ) -> Result<(Removal, bool), PlacedError> {
        let mut doomed: Vec<(PathBuf, Entry, Found)> = Vec::new();
        for (path, entry) in record.of_service(service) {
            if keep.contains(path) {
                continue;
            }
            doomed.push((path.clone(), entry.clone(), self.found(path, entry)?));
        }
        // The `WantedBy=` links first: a unit is disabled before its own link goes.
        doomed.sort_by_key(|(path, entry, _)| (entry.kind != PathKind::WantsLink, path.clone()));
        let kept: BTreeSet<String> = record
            .of_service(service)
            .filter(|(path, entry)| keep.contains(*path) & (entry.kind != PathKind::File))
            .map(|(path, _)| file_name_of(path))
            .collect();
        let mut removal = Removal::default();
        if stop {
            let units: BTreeSet<String> = doomed
                .iter()
                .filter(|(_, _, found)| *found != Found::Foreign)
                .filter_map(|(path, entry, _)| stop_name(path, entry))
                .filter(|unit| !kept.contains(unit))
                .collect();
            for unit in units {
                self.units.stop_unit(&unit)?;
                removal.stopped.push(unit);
            }
        }
        let mut units_changed = false;
        for (path, entry, _) in &doomed {
            match self.found(path, entry)? {
                Found::Foreign => removal.left_alone.push(path.clone()),
                found => {
                    self.remove_link(path, found)?;
                    removal.removed.push(path.clone());
                    units_changed |= entry.kind != PathKind::File;
                }
            }
            record.paths.remove(path);
        }
        record.reload_pending |= units_changed;
        if !doomed.is_empty() {
            self.store().save(record, lock)?;
        }
        Ok((removal, units_changed))
    }

    /// [`Placer::prune_in`] under the lock, on the record on disk, keeping the paths `keep`
    /// names, then one `daemon-reload` when a unit or `WantedBy=` link went, after which the
    /// reload owed is cleared; a failed reload leaves it owed.
    pub(crate) fn prune(
        &self,
        service: &str,
        stop: bool,
        keep: impl FnOnce(&PlacedRecord) -> BTreeSet<PathBuf>,
    ) -> Result<Removal, PlacedError> {
        let lock = self.lock()?;
        let mut record = self.store().load()?;
        let keep = keep(&record);
        let (mut removal, units_changed) =
            self.prune_in(&mut record, service, &keep, stop, &lock)?;
        if units_changed {
            self.units.daemon_reload()?;
            self.reloaded(&lock)?;
            removal.reloaded = true;
        }
        Ok(removal)
    }

    /// Removes the placed paths of the service of `manifest` that its version does not place
    /// ([`Placer::desired`]): after `cleanup` of a hooked update, or at step 10 of a force. A
    /// unit no longer listed is stopped, disabled and unlinked, then `daemon-reload` runs once.
    /// The record, not the old manifest, says what is placed.
    pub fn unlink_unlisted(&self, manifest: &Manifest) -> Result<Removal, PlacedError> {
        let keep = self.desired(manifest)?.paths();
        self.prune(manifest.name.as_str(), true, |_| keep)
    }

    /// Removes every placed path of `service` and drops them from the record: what uninstall
    /// does after its tombstone and its stop of the `run` units (design 4.7, "Uninstall", steps
    /// 4 and 5). The tombstone is left to the caller.
    pub fn remove_all(&self, service: &ServiceName) -> Result<Removal, PlacedError> {
        self.prune(service.as_str(), true, |_| BTreeSet::new())
    }
}
