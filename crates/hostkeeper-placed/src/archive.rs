//! What an archive version left in the system's directories, and its removal
//! (docs/design/packages-native.md, section 10.6, "The move of one service on one host").
//!
//! A release before the native installers linked each unit and file of a version into the
//! system's directories, a link to `<packages>/<service>/current/...` at each path, and recorded
//! every path in `placed.json` under the top-level key `paths`:
//!
//! ```json
//! "paths": {
//!   "/etc/systemd/system/hostkeeper.service": {
//!     "service": "hostkeeper", "version": "0.1.240", "kind": "unit", "state": "placed",
//!     "link_target": "/var/lib/hostkeeper/packages/hostkeeper/current/units/hostkeeper.service"
//!   }
//! }
//! ```
//!
//! This daemon links nothing, and keeps the key as it is ([`crate::store`]), but a version
//! installed natively over such a service has to take those paths away: a unit in
//! `/etc/systemd/system/` hides the package's unit of the same name in `/usr/lib/systemd/system/`,
//! and once `current` names the native version its link dangles, so systemd finds no unit at all.
//!
//! [`Placer::take_away_archive_paths`] does it, under the lock of `placed.json`, before the
//! native install: each path the record lists for the service is looked at without following a
//! link, and removed when it is the archive's:
//!
//! | kind | removed when the path holds |
//! |---|---|
//! | `unit` | a regular file or a link, wherever the link points, dangling or not |
//! | `wants_link`, `file` | a link to the recorded `link_target`, or into the packages directory |
//!
//! A `WantedBy=` link or a file that is anything else is not the archive's (a distribution
//! package's `systemctl enable`, a conffile, an operator's) and is left as it is.
//! `daemon-reload` runs when a unit or a `WantedBy=` link went. The record is not written: its
//! entries go only once the native version is installed and its units checked
//! ([`Placer::forget_archive_paths`]), so an install tried again checks the same units again.
//! An install that fails or defers puts the links it removed back
//! ([`Placer::restore_archive_links`]): until a native version is installed, the archive's unit
//! is the one systemd starts the running service by.
//!
//! [`Placer::remove_dangling_unit_links`] is the repair of a host a release moved without that
//! removal: a link directly in the unit directory that points into the package directory of a
//! service the daemon runs natively, and whose target is gone, is removed, then `daemon-reload`.
//! A link that points anywhere else, or whose target is there, is left as it is.

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::error::{Owner, PlacedError, io_at};
use crate::fs::FileKind;
use crate::placer::{Placer, occupant};
use crate::store::PlacedRecord;

/// The top-level key of `placed.json` under which an archive version's paths are recorded.
pub const PATHS_KEY: &str = "paths";

/// What an archive version placed at a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// A unit file, `<unit dir>/<name>`.
    Unit,
    /// A file of the version, at the path its manifest named.
    File,
    /// The `WantedBy=` link of a `run` unit, `<unit dir>/<target>.wants/<unit>`.
    WantsLink,
}

impl ArchiveKind {
    /// The kind as `placed.json` spells it; `None` for a spelling this daemon does not know.
    fn parse(text: &str) -> Option<ArchiveKind> {
        match text {
            "unit" => Some(ArchiveKind::Unit),
            "file" => Some(ArchiveKind::File),
            "wants_link" => Some(ArchiveKind::WantsLink),
            _ => None,
        }
    }
}

/// One path the record lists for an archive version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivePath {
    /// The path, as the host names it.
    pub path: PathBuf,
    pub kind: ArchiveKind,
    /// The target the archive's link was written with; `None` when the entry names none.
    pub link_target: Option<PathBuf>,
}

/// What [`Placer::take_away_archive_paths`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakenAway {
    /// The paths removed from the disk, in the record's order.
    pub removed: Vec<PathBuf>,
    /// Each removed link and its target as it was written: what
    /// [`Placer::restore_archive_links`] puts back.
    pub links: Vec<(PathBuf, PathBuf)>,
    /// The paths the record listed that hold something not the archive's: left as they are.
    pub left_alone: Vec<PathBuf>,
    /// The file names of the units the record lists that are not left alone (removed now, or
    /// already gone): the units whose name the native package's own unit now has to answer to.
    pub units: Vec<String>,
    /// Whether `daemon-reload` ran.
    pub reloaded: bool,
}

impl PlacedRecord {
    /// The paths the record lists for `service` under [`PATHS_KEY`], in the record's order. An
    /// entry of another service, or one whose `service` or `kind` is missing or not a kind this
    /// daemon knows, is not one.
    pub fn archive_paths(&self, service: &str) -> Vec<ArchivePath> {
        let Some(Value::Object(paths)) = self.extra().get(PATHS_KEY) else {
            return Vec::new();
        };
        paths
            .iter()
            .filter_map(|(path, entry)| {
                let field = |key: &str| entry.get(key).and_then(Value::as_str);
                if field("service")? != service {
                    return None;
                }
                Some(ArchivePath {
                    path: PathBuf::from(path),
                    kind: ArchiveKind::parse(field("kind")?)?,
                    link_target: field("link_target").map(PathBuf::from),
                })
            })
            .collect()
    }

    /// Drops the entries of `gone` from [`PATHS_KEY`]; the key itself stays, empty or not, as
    /// the release that wrote it reads it.
    fn drop_archive_paths(&mut self, gone: &[ArchivePath]) {
        let paths = self
            .extra_mut()
            .get_mut(PATHS_KEY)
            .and_then(Value::as_object_mut);
        paths.into_iter().for_each(|paths| {
            for one in gone {
                paths.remove(&one.path.to_string_lossy().into_owned());
            }
        });
    }
}

/// The target of a link at `link`, made absolute against the link's directory, with its `.`
/// and `..` components resolved by their spelling (a `..` above `/` stays at `/`).
fn absolute(link: &Path, target: &Path) -> PathBuf {
    let dir = link.parent().unwrap_or(Path::new("/"));
    let mut out = PathBuf::from("/");
    for part in dir.join(target).components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

impl Placer<'_> {
    /// Whether `found`, what is at `one.path`, is what the archive placed there ([the
    /// module](self)'s table).
    fn is_archives(&self, one: &ArchivePath, found: &Owner) -> bool {
        match (one.kind, found) {
            (ArchiveKind::Unit, Owner::File | Owner::Link(_)) => true,
            (_, Owner::Link(target)) => {
                (one.link_target.as_deref() == Some(target.as_path()))
                    | absolute(&one.path, target).starts_with(self.roots.packages().dir())
            }
            _ => false,
        }
    }

    /// Takes away the paths an archive version of `service` placed, under the lock of
    /// `placed.json` ([the module](self)): each path that is the archive's, removed, then
    /// `daemon-reload` when a unit or a `WantedBy=` link went. The record is read, not written.
    pub fn take_away_archive_paths(&self, service: &str) -> Result<TakenAway, PlacedError> {
        let _lock = self.lock()?;
        let listed = self.store().load()?.archive_paths(service);
        let mut taken = TakenAway::default();
        let mut units_changed = false;
        for one in &listed {
            let found = occupant(self.fs, &one.path)?;
            if let Some(found) = &found {
                if !self.is_archives(one, found) {
                    taken.left_alone.push(one.path.clone());
                    continue;
                }
                self.remove_synced(&one.path)?;
                taken.removed.push(one.path.clone());
                if let Owner::Link(target) = found {
                    taken.links.push((one.path.clone(), target.clone()));
                }
                units_changed |= one.kind != ArchiveKind::File;
            }
            let unit = (one.kind == ArchiveKind::Unit).then(|| one.path.file_name());
            taken.units.extend(
                unit.flatten()
                    .map(|name| name.to_string_lossy().into_owned()),
            );
        }
        if units_changed {
            self.units.daemon_reload()?;
            taken.reloaded = true;
        }
        Ok(taken)
    }

    /// Puts back, under the lock, each link of `taken` whose path holds nothing now, as it was
    /// written, then `daemon-reload` when one was put back: what an install that failed after
    /// [`Placer::take_away_archive_paths`] leaves. The links put back. A regular file it removed
    /// is not put back (the archive linked every path it placed).
    pub fn restore_archive_links(&self, taken: &TakenAway) -> Result<Vec<PathBuf>, PlacedError> {
        let _lock = self.lock()?;
        let mut restored = Vec::new();
        for (path, target) in &taken.links {
            if self
                .fs
                .kind(path)
                .map_err(io_at("inspect", path))?
                .is_some()
            {
                continue;
            }
            self.fs.symlink(target, path).map_err(io_at("link", path))?;
            let dir = path.parent().unwrap_or(Path::new("/"));
            self.fs.sync_dir(dir).map_err(io_at("fsync", dir))?;
            restored.push(path.clone());
        }
        if !restored.is_empty() {
            self.units.daemon_reload()?;
        }
        Ok(restored)
    }

    /// Drops every path the record lists for `service` under [`PATHS_KEY`], under the lock: the
    /// record no longer owns them once a native version of the service is installed. `true` when
    /// the record listed one and was written; a record that lists none is not written.
    pub fn forget_archive_paths(&self, service: &str) -> Result<bool, PlacedError> {
        let lock = self.lock()?;
        let store = self.store();
        let mut record = store.load()?;
        let listed = record.archive_paths(service);
        if listed.is_empty() {
            return Ok(false);
        }
        record.drop_archive_paths(&listed);
        store.save(&record, &lock)?;
        Ok(true)
    }

    /// Whether nothing is at `path`: no file there, or a component above it that is not a
    /// directory.
    fn is_gone(&self, path: &Path) -> Result<bool, PlacedError> {
        match self.fs.kind(path) {
            Ok(found) => Ok(found.is_none()),
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => Ok(true),
            Err(e) => Err(io_at("inspect", path)(e)),
        }
    }

    /// Removes the file or link at `path` and syncs its directory.
    fn remove_synced(&self, path: &Path) -> Result<(), PlacedError> {
        let dir = path.parent().unwrap_or(Path::new("/"));
        self.fs.remove_file(path).map_err(io_at("remove", path))?;
        self.fs.sync_dir(dir).map_err(io_at("fsync", dir))
    }

    /// Removes each link directly in the unit directory that points into the package directory
    /// of one of `services` and whose target is gone, then runs `daemon-reload` when one went
    /// ([the module](self)). The links removed, by name order. A unit directory that is not
    /// there has none.
    pub fn remove_dangling_unit_links(
        &self,
        services: &BTreeSet<String>,
    ) -> Result<Vec<PathBuf>, PlacedError> {
        let dir = self.roots.unit_dir();
        let names = match self.fs.list(dir) {
            Ok(names) => names,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at("list", dir)(e)),
        };
        let packages = self.roots.packages();
        let mut removed = Vec::new();
        for name in names {
            let path = dir.join(name);
            if self.fs.kind(&path).map_err(io_at("inspect", &path))? != Some(FileKind::Symlink) {
                continue;
            }
            let target = absolute(
                &path,
                &self
                    .fs
                    .read_link(&path)
                    .map_err(io_at("read the link", &path))?,
            );
            let ours = services
                .iter()
                .any(|service| target.starts_with(packages.service(service)));
            if !ours || !self.is_gone(&target)? {
                continue;
            }
            self.remove_synced(&path)?;
            removed.push(path);
        }
        if !removed.is_empty() {
            self.units.daemon_reload()?;
        }
        Ok(removed)
    }
}
