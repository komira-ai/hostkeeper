//! The write-ahead protocol of design section 4.7, "Placed paths", one function per step so that
//! a caller (a hooked update, a force) sequences them around the switch of `current`:
//!
//! 1. [`Placer::begin`] checks the command lines of the units it is given ([`crate::lint`]) and
//!    the claims ([`check`]), then writes `placed.json` with the target's paths `pending`, each
//!    with the owning service and version, and a `daemon-reload` owed;
//! 2. [`Placer::link`] makes each missing link in one step that fails if anything is there, and
//!    repoints a recorded link under a temporary name renamed over it;
//! 3. [`Placer::switch_current`] sets `previous` first, `current` second;
//! 4. [`Placer::reload`] runs `daemon-reload` and then clears the reload owed;
//! 5. [`Placer::commit`] marks the paths `placed`.
//!
//! Every placed link points into `<packages>/<service>/current/`, never into a version directory,
//! so the switch of `current` re-points them all. A crash between any two writes leaves only
//! links that `placed.json` lists, and [`Placer::reconcile`] finishes or undoes the work at start.
//!
//! `placed.json` is read and written, and `daemon-reload` run, under a short lock of its own
//! (`<state dir>/placed.lock`, design section 8.7), taken by each call for its duration; the
//! caller holds the service lock around the whole sequence. Temporary names are
//! `.hk~<owner>~tmp`, `<owner>` being the service (or `placed.json`): `~` is outside the path
//! grammar, so no placed path has such a name, and the name stays short whatever the length of
//! the link's own name.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::{ServiceName, Version, is_version};

use crate::check::{check, occupant};
use crate::claim::Claims;
use crate::error::{Owner, PlacedError, io_at};
use crate::fs::Fs;
use crate::lint::{check_unit_text, read_unit};
use crate::roots::Roots;
use crate::store::{Entry, PathKind, PathState, PlacedRecord, Store};
use crate::unit_control::UnitControl;

/// The prefix of every temporary name this crate writes.
pub const TEMPORARY_PREFIX: &str = ".hk~";
/// The suffix of every temporary name this crate writes.
pub const TEMPORARY_SUFFIX: &str = "~tmp";

/// The temporary name the links of `owner` are made under, in the directory of each.
pub fn temporary_name(owner: &str) -> String {
    format!("{TEMPORARY_PREFIX}{owner}{TEMPORARY_SUFFIX}")
}

/// Whether `name` is a temporary name this crate writes.
pub fn is_temporary(name: &str) -> bool {
    name.starts_with(TEMPORARY_PREFIX) & name.ends_with(TEMPORARY_SUFFIX)
}

/// The lock of `placed.json`, held until it is dropped. [`Store::save`] asks for one.
#[derive(Debug)]
pub struct PlacedLock {
    _file: File,
}

/// Points the link `<dir>/<name>` at `target`: a link made under the temporary name of `owner`,
/// renamed over the old one, and an `fsync` of `dir`. A temporary link a crash left is replaced.
/// A crash leaves either link whole.
pub(crate) fn set_link(
    fs: &dyn Fs,
    dir: &Path,
    name: &str,
    target: &Path,
    owner: &str,
) -> Result<(), PlacedError> {
    let tmp = dir.join(temporary_name(owner));
    if fs.kind(&tmp).map_err(io_at("inspect", &tmp))?.is_some() {
        fs.remove_file(&tmp).map_err(io_at("remove", &tmp))?;
    }
    fs.symlink(target, &tmp).map_err(io_at("link", &tmp))?;
    fs.rename(&tmp, &dir.join(name))
        .map_err(io_at("rename", &tmp))?;
    fs.sync_dir(dir).map_err(io_at("fsync", dir))
}

/// The last component of `path`: the name of a placed link.
pub(crate) fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The paths of one version, recorded `pending` by [`Placer::begin`]: the token [`Placer::link`]
/// and [`Placer::commit`] take.
#[derive(Debug)]
pub struct Linking {
    claims: Claims,
}

impl Linking {
    pub fn claims(&self) -> &Claims {
        &self.claims
    }
}

/// Links packages into the system of one host.
pub struct Placer<'a> {
    pub(crate) roots: Roots,
    pub(crate) fs: &'a dyn Fs,
    pub(crate) units: &'a dyn UnitControl,
}

impl<'a> Placer<'a> {
    /// Links under `roots`, reading and writing through `fs`, and asking `units` for reloads and
    /// stops.
    pub fn new(roots: Roots, fs: &'a dyn Fs, units: &'a dyn UnitControl) -> Placer<'a> {
        Placer { roots, fs, units }
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    /// `placed.json` of this host.
    pub fn store(&self) -> Store<'a> {
        Store::new(self.fs, self.roots.state_dir())
    }

    /// Takes the lock of `placed.json` (`<state dir>/placed.lock`, opened without following a
    /// link), or [`PlacedError::Locked`] when another holder has it. A caller that changes the
    /// record itself (the tombstone of uninstall) holds it from its read to its write.
    pub fn lock(&self) -> Result<PlacedLock, PlacedError> {
        let path = self.roots.lock_file();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.fs.host_path(&path))
            .map_err(io_at("open", &path))?;
        file.try_lock().map_err(|e| PlacedError::Locked {
            path,
            source: io::Error::from(e),
        })?;
        Ok(PlacedLock { _file: file })
    }

    /// The version the link `link` names, `None` when there is no link. The link must name a
    /// version directory beside it: its target one component that is a version. Any other
    /// target (another service's directory, an absolute path), or anything but a link, is a
    /// [`PlacedError::PathConflict`].
    fn read_version(&self, link: &Path) -> Result<Option<String>, PlacedError> {
        let conflict = |owner: Owner| PlacedError::PathConflict {
            path: link.to_path_buf(),
            owner,
        };
        match occupant(self.fs, link)? {
            None => Ok(None),
            Some(Owner::Link(target)) => match target.to_str().filter(|text| is_version(text)) {
                Some(version) => Ok(Some(version.to_owned())),
                None => Err(conflict(Owner::Link(target))),
            },
            Some(owner) => Err(conflict(owner)),
        }
    }

    /// The version `current` of `service` points at, `None` before its first switch.
    pub fn current(&self, service: &str) -> Result<Option<String>, PlacedError> {
        self.read_version(&self.roots.packages().current(service))
    }

    /// The version `previous` of `service` points at.
    pub fn previous(&self, service: &str) -> Result<Option<String>, PlacedError> {
        self.read_version(&self.roots.packages().previous(service))
    }

    /// [`check`] of `claims` against the record on disk: the refusal before anything changes.
    pub fn check(&self, claims: &Claims) -> Result<(), PlacedError> {
        check(claims, &self.store().load()?, self.fs)
    }

    /// The claims of `manifest` ([`Claims::of`]), with the command lines of its unit files
    /// checked: what a caller links with [`Placer::begin`].
    pub fn claims(&self, manifest: &Manifest) -> Result<Claims, PlacedError> {
        let claims = Claims::of(&self.roots, manifest)?;
        self.lint_units(&claims)?;
        Ok(claims)
    }

    /// Checks the command lines of every unit `claims` places, read from its version directory
    /// ([`crate::lint::check_unit_text`]).
    fn lint_units(&self, claims: &Claims) -> Result<(), PlacedError> {
        let service = claims.service().as_str();
        let packages = self.roots.packages();
        let units = packages
            .version(service, claims.version().as_str())
            .join("units");
        for claim in claims.iter().filter(|claim| claim.kind == PathKind::Unit) {
            let file = file_name_of(&claim.path);
            let text = read_unit(self.fs, &units, &file)?;
            check_unit_text(packages.dir(), service, &file, &text)?;
        }
        Ok(())
    }

    /// Step 1: under the lock, refuses a tombstoned service ([`PlacedError::Tombstoned`]), a unit
    /// whose command line names a package file other than through `current`
    /// ([`crate::LintError`]) and the claims [`check`] refuses, then records every path of
    /// `claims` `pending` for its service and version, with a `daemon-reload` owed. A path
    /// already recorded with the same link is left as it is, and a record that would not change
    /// is not written.
    pub fn begin(&self, claims: &Claims) -> Result<Linking, PlacedError> {
        let lock = self.lock()?;
        let mut record = self.store().load()?;
        self.begin_in(&mut record, claims, &lock)?;
        Ok(Linking {
            claims: claims.clone(),
        })
    }

    /// [`Placer::begin`] on a record in hand, under the lock; `true` when it was written.
    pub(crate) fn begin_in(
        &self,
        record: &mut PlacedRecord,
        claims: &Claims,
        lock: &PlacedLock,
    ) -> Result<bool, PlacedError> {
        let service = claims.service().as_str();
        if record.uninstalling.contains_key(service) {
            return Err(PlacedError::Tombstoned {
                service: service.to_owned(),
            });
        }
        self.lint_units(claims)?;
        check(claims, record, self.fs)?;
        let version = claims.version().as_str();
        let mut changed = false;
        for claim in claims.iter() {
            let wanted = Entry::new(
                service,
                version,
                claim.kind,
                PathState::Pending,
                &claim.link_target,
            );
            match record.paths.get_mut(&claim.path) {
                Some(entry) if entry.same_link(&wanted) => {}
                Some(entry) => {
                    entry.assign(wanted);
                    changed = true;
                }
                None => {
                    record.paths.insert(claim.path.clone(), wanted);
                    changed = true;
                }
            }
        }
        if changed {
            record.reload_pending = true;
            self.store().save(record, lock)?;
        }
        Ok(changed)
    }

    /// Points the link at `path` at `target`, making its missing directories (mode 0755); a
    /// link already right is left. A missing link is made at `path` itself, in one step that
    /// fails if anything got there first, which is then a [`PlacedError::PathConflict`] and is
    /// not touched. A link to elsewhere, which the caller's check found recorded for this
    /// service, is repointed by a rename over it from `owner`'s temporary name. Anything but a
    /// link at `path` is a conflict and is not touched. `true` when the link was written.
    pub(crate) fn ensure_link(
        &self,
        path: &Path,
        target: &Path,
        owner: &str,
    ) -> Result<bool, PlacedError> {
        let conflict = |owner: Owner| PlacedError::PathConflict {
            path: path.to_path_buf(),
            owner,
        };
        let dir = path.parent().unwrap_or(Path::new("/"));
        match occupant(self.fs, path)? {
            Some(Owner::Link(now)) if now.as_path() == target => Ok(false),
            None => {
                self.fs.create_dirs(dir).map_err(io_at("create", dir))?;
                match self.fs.symlink(target, path) {
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        Err(conflict(occupant(self.fs, path)?.unwrap_or(Owner::Other)))
                    }
                    other => other.map_err(io_at("link", path)),
                }?;
                self.fs.sync_dir(dir).map_err(io_at("fsync", dir))?;
                Ok(true)
            }
            Some(Owner::Link(_)) => {
                set_link(self.fs, dir, &file_name_of(path), target, owner)?;
                Ok(true)
            }
            Some(other) => Err(conflict(other)),
        }
    }

    /// Step 2: makes every link of `linking`, each pointing into `current`. Idempotent: a link
    /// already right is left. `true` when a link was written.
    pub fn link(&self, linking: &Linking) -> Result<bool, PlacedError> {
        let owner = linking.claims.service().as_str();
        let mut changed = false;
        for claim in linking.claims.iter() {
            changed |= self.ensure_link(&claim.path, &claim.link_target, owner)?;
        }
        Ok(changed)
    }

    /// Step 3: switches `current` of `service` to `to`, setting `previous` to the version
    /// `current` named first, then `current`, each by rename. A repeat after a crash between the
    /// two compares both links and writes only what is still missing; a repeat after both
    /// writes nothing. With no `current` yet (a first install) only `current` is written.
    /// `true` when a link was written.
    pub fn switch_current(&self, service: &ServiceName, to: &Version) -> Result<bool, PlacedError> {
        let name = service.as_str();
        let dir = self.roots.packages().service(name);
        let current = self.current(name)?;
        if current.as_deref() == Some(to.as_str()) {
            return Ok(false);
        }
        if let Some(from) = &current
            && self.previous(name)?.as_ref() != Some(from)
        {
            set_link(self.fs, &dir, "previous", Path::new(from), name)?;
        }
        set_link(self.fs, &dir, "current", Path::new(to.as_str()), name)?;
        Ok(true)
    }

    /// Step 4: `daemon-reload`, under the lock; once it succeeded, the reload owed in the record
    /// is cleared. A failed reload leaves it owed, for the next reload or the next start.
    pub fn reload(&self) -> Result<(), PlacedError> {
        let lock = self.lock()?;
        self.units.daemon_reload()?;
        self.reloaded(&lock)
    }

    /// Clears the reload owed in the record on disk, if it is set.
    pub(crate) fn reloaded(&self, lock: &PlacedLock) -> Result<(), PlacedError> {
        let mut record = self.store().load()?;
        if record.reload_pending {
            record.reload_pending = false;
            self.store().save(&record, lock)?;
        }
        Ok(())
    }

    /// Step 5: marks the paths of `linking` `placed`, under the lock. `true` when the record was
    /// written.
    pub fn commit(&self, linking: Linking) -> Result<bool, PlacedError> {
        let lock = self.lock()?;
        let mut record = self.store().load()?;
        self.commit_in(&mut record, &linking.claims, &lock)
    }

    /// [`Placer::commit`] on a record in hand, under the lock.
    pub(crate) fn commit_in(
        &self,
        record: &mut PlacedRecord,
        claims: &Claims,
        lock: &PlacedLock,
    ) -> Result<bool, PlacedError> {
        let service = claims.service().as_str();
        let version = claims.version().as_str();
        let mut changed = false;
        for claim in claims.iter() {
            let pending = record.paths.get_mut(&claim.path).filter(|entry| {
                (entry.service == service)
                    & (entry.version == version)
                    & (entry.state == PathState::Pending)
            });
            if let Some(entry) = pending {
                entry.state = PathState::Placed;
                changed = true;
            }
        }
        if changed {
            self.store().save(record, lock)?;
        }
        Ok(changed)
    }

    /// Every path the version of `manifest` places: its units and files ([`Placer::claims`],
    /// command lines checked) and the `WantedBy=` links of its `run` units.
    pub fn desired(&self, manifest: &Manifest) -> Result<Claims, PlacedError> {
        let mut claims = self.claims(manifest)?;
        claims.extend(self.wants_claims(manifest)?)?;
        Ok(claims)
    }

    /// Relinks the service to the version of `manifest`, the one `current` points at or is about
    /// to (before cutover, and back before rollback): [`Placer::begin`], [`Placer::link`] and
    /// [`Placer::commit`] for every path that version places, its `run` units' `WantedBy=` links
    /// included, then the removal of the service's paths that version does not list
    /// ([`Placer::unlink_unlisted`]), then one `daemon-reload` when one is owed. `true` when
    /// anything changed.
    pub fn relink_to(&self, manifest: &Manifest) -> Result<bool, PlacedError> {
        let desired = self.desired(manifest)?;
        let linking = self.begin(&desired)?;
        let linked = self.link(&linking)?;
        self.commit(linking)?;
        let keep = desired.paths();
        let removal = self.prune(manifest.name.as_str(), true, |_| keep)?;
        let owed = {
            let _lock = self.lock()?;
            self.store().load()?.reload_pending
        };
        if owed {
            self.reload()?;
        }
        Ok(linked | removal.reloaded | owed)
    }
}
