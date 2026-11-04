//! The write-ahead protocol of design section 4.7, "Placed paths", one function per step so that
//! a caller (a hooked update, a force) sequences them around the switch of `current`:
//!
//! 1. [`Placer::begin`] writes `placed.json` with the target's paths `pending`, each with the
//!    owning service and version, after the [`check`] of the claims;
//! 2. [`Placer::link`] makes each link under a temporary name in its directory and renames it
//!    into place;
//! 3. [`Placer::switch_current`] sets `previous` first, `current` second;
//! 4. [`Placer::reload`] runs `daemon-reload`;
//! 5. [`Placer::commit`] marks the paths `placed`.
//!
//! Every placed link points into `<packages>/<service>/current/`, never into a version directory,
//! so the switch of `current` re-points them all. A crash between any two writes leaves only
//! links that `placed.json` lists, and [`Placer::reconcile`] finishes or undoes the work at start.
//!
//! `placed.json` is read and written, and `daemon-reload` run, under a short lock of its own
//! (`<state dir>/placed.lock`, design section 8.7), taken by each call for its duration; the
//! caller holds the service lock around the whole sequence.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::{ServiceName, Version};

use crate::check::{check, occupant};
use crate::claim::Claims;
use crate::error::{Owner, PlacedError, io_at};
use crate::fs::Fs;
use crate::roots::Roots;
use crate::store::{Entry, PathState, PlacedRecord, Store};
use crate::unit_control::UnitControl;

/// The prefix of every temporary name this crate writes.
pub const TEMPORARY_PREFIX: &str = ".hk-";
/// The suffix of every temporary name this crate writes.
pub const TEMPORARY_SUFFIX: &str = ".tmp";

/// The temporary name a link or file named `name` is made under, in its own directory.
pub fn temporary_name(name: &str) -> String {
    format!("{TEMPORARY_PREFIX}{name}{TEMPORARY_SUFFIX}")
}

/// Whether `name` is a temporary name this crate writes.
pub fn is_temporary(name: &str) -> bool {
    name.starts_with(TEMPORARY_PREFIX) & name.ends_with(TEMPORARY_SUFFIX)
}

/// Points the link `<dir>/<name>` at `target`: a link made under the temporary name, renamed
/// over the old one, and an `fsync` of `dir`. A temporary link a crash left is replaced. A crash
/// leaves either link whole.
pub(crate) fn set_link(
    fs: &dyn Fs,
    dir: &Path,
    name: &str,
    target: &Path,
) -> Result<(), PlacedError> {
    let tmp = dir.join(temporary_name(name));
    if fs.kind(&tmp).map_err(io_at("inspect", &tmp))?.is_some() {
        fs.remove_file(&tmp).map_err(io_at("remove", &tmp))?;
    }
    fs.symlink(target, &tmp).map_err(io_at("link", &tmp))?;
    fs.rename(&tmp, &dir.join(name))
        .map_err(io_at("rename", &tmp))?;
    fs.sync_dir(dir).map_err(io_at("fsync", dir))
}

/// The last component of `path`: the version a `current` or `previous` link's target names,
/// or the name of a placed link.
fn file_name_of(path: &Path) -> String {
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

    /// Takes the lock of `placed.json`, held until the file is dropped.
    pub(crate) fn lock(&self) -> Result<File, PlacedError> {
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
        Ok(file)
    }

    /// The version the link `link` names, `None` when there is no link. Anything else at `link`
    /// is a [`PlacedError::PathConflict`].
    fn read_version(&self, link: &Path) -> Result<Option<String>, PlacedError> {
        match occupant(self.fs, link)? {
            None => Ok(None),
            Some(Owner::Link(target)) => Ok(Some(file_name_of(&target))),
            Some(owner) => Err(PlacedError::PathConflict {
                path: link.to_path_buf(),
                owner,
            }),
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

    /// Step 1: under the lock, refuses a tombstoned service ([`PlacedError::Tombstoned`]) and
    /// the claims [`check`] refuses, then records every path of `claims` `pending` for its
    /// service and version. A path already recorded with the same link is left as it is, and a
    /// record that would not change is not written.
    pub fn begin(&self, claims: &Claims) -> Result<Linking, PlacedError> {
        let _lock = self.lock()?;
        let mut record = self.store().load()?;
        self.begin_in(&mut record, claims)?;
        Ok(Linking {
            claims: claims.clone(),
        })
    }

    /// [`Placer::begin`] on a record in hand, under the lock; `true` when it was written.
    pub(crate) fn begin_in(
        &self,
        record: &mut PlacedRecord,
        claims: &Claims,
    ) -> Result<bool, PlacedError> {
        let service = claims.service().as_str();
        if record.uninstalling.contains_key(service) {
            return Err(PlacedError::Tombstoned {
                service: service.to_owned(),
            });
        }
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
            self.store().save(record)?;
        }
        Ok(changed)
    }

    /// Points the link at `path` at `target`, making its missing directories (mode 0755); a
    /// link already right is left. Anything but a link at `path` is a
    /// [`PlacedError::PathConflict`] and is not touched. `true` when the link was written.
    pub(crate) fn ensure_link(&self, path: &Path, target: &Path) -> Result<bool, PlacedError> {
        match occupant(self.fs, path)? {
            Some(Owner::Link(now)) if now.as_path() == target => return Ok(false),
            None | Some(Owner::Link(_)) => {}
            Some(owner) => {
                return Err(PlacedError::PathConflict {
                    path: path.to_path_buf(),
                    owner,
                });
            }
        }
        let dir = path.parent().unwrap_or(Path::new("/"));
        let name = file_name_of(path);
        self.fs.create_dirs(dir).map_err(io_at("create", dir))?;
        set_link(self.fs, dir, &name, target)?;
        Ok(true)
    }

    /// Step 2: makes every link of `linking`, each pointing into `current`. Idempotent: a link
    /// already right is left. `true` when a link was written.
    pub fn link(&self, linking: &Linking) -> Result<bool, PlacedError> {
        let mut changed = false;
        for claim in linking.claims.iter() {
            changed |= self.ensure_link(&claim.path, &claim.link_target)?;
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
            set_link(self.fs, &dir, "previous", Path::new(from))?;
        }
        set_link(self.fs, &dir, "current", Path::new(to.as_str()))?;
        Ok(true)
    }

    /// Step 4: `daemon-reload`, under the lock.
    pub fn reload(&self) -> Result<(), PlacedError> {
        let _lock = self.lock()?;
        Ok(self.units.daemon_reload()?)
    }

    /// Step 5: marks the paths of `linking` `placed`, under the lock. `true` when the record was
    /// written.
    pub fn commit(&self, linking: Linking) -> Result<bool, PlacedError> {
        let _lock = self.lock()?;
        let mut record = self.store().load()?;
        self.commit_in(&mut record, &linking.claims)
    }

    /// [`Placer::commit`] on a record in hand, under the lock.
    pub(crate) fn commit_in(
        &self,
        record: &mut PlacedRecord,
        claims: &Claims,
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
            self.store().save(record)?;
        }
        Ok(changed)
    }

    /// Every path the version of `manifest` places: its units and files ([`Claims::of`]) and
    /// the `WantedBy=` links of its `run` units.
    pub fn desired(&self, manifest: &Manifest) -> Result<Claims, PlacedError> {
        let mut claims = Claims::of(&self.roots, manifest)?;
        claims.extend(self.wants_claims(manifest)?)?;
        Ok(claims)
    }

    /// Relinks the service to the version of `manifest`, the one `current` points at or is about
    /// to (before cutover, and back before rollback): [`Placer::begin`], [`Placer::link`] and
    /// [`Placer::commit`] for every path that version places, its `run` units' `WantedBy=` links
    /// included, then the removal of the service's paths that version does not list
    /// ([`Placer::unlink_unlisted`]), then one `daemon-reload` when anything changed. `true`
    /// when anything changed.
    pub fn relink_to(&self, manifest: &Manifest) -> Result<bool, PlacedError> {
        let desired = self.desired(manifest)?;
        let linking = self.begin(&desired)?;
        let linked = self.link(&linking)?;
        self.commit(linking)?;
        let keep = desired.paths();
        let removal = self.prune(manifest.name.as_str(), true, |_| keep)?;
        let changed = linked | removal.reloaded;
        if linked & !removal.reloaded {
            self.reload()?;
        }
        Ok(changed)
    }
}
