//! The placer: what the daemon keeps of the host's `current` and `previous` links and of
//! `placed.json`, and the service manager's reload.
//!
//! [`Placer::switch_current`] sets `previous` first, `current` second, each a link made under a
//! temporary name and renamed over the old one ([`set_link`]); [`Placer::reload`] runs
//! `daemon-reload` under the lock; [`Placer::lock`] is the lock every writer of `placed.json`
//! holds around its read and write.
//!
//! `placed.json` is read and written, and `daemon-reload` run, under a short lock of its own
//! (`<state dir>/placed.lock`, design section 8.7); the caller holds the service lock around the
//! whole sequence. Temporary names are `.hk~<owner>~tmp`, `<owner>` being the service (or
//! `placed.json`), or a hash of a long service name ([`temporary_name`]): `~` is outside the
//! path grammar, so no placed path has such a name, and the name stays short whatever the length
//! of the link's own name.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version, is_version};

use crate::error::{Owner, PlacedError, io_at};
use crate::fs::{FileKind, Fs};
use crate::roots::Roots;
use crate::store::Store;
use crate::unit_control::UnitControl;

/// The prefix of every temporary name this crate writes.
pub const TEMPORARY_PREFIX: &str = ".hk~";
/// The suffix of every temporary name this crate writes.
pub const TEMPORARY_SUFFIX: &str = "~tmp";

/// The longest owner a temporary name spells out; a longer one is named by its hash.
pub const MAX_SPELLED_OWNER: usize = 64;

/// The 64-bit FNV-1a hash of `bytes`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The temporary name the links of `owner` are made under, in the directory of each:
/// `.hk~<owner>~tmp` for an owner of at most [`MAX_SPELLED_OWNER`] bytes, else
/// `.hk~#<16 hex digits of its hash>~tmp`, so that the name stays far below a path component's
/// 255 bytes whatever the length of a service name. `#` is in no service name, so the two forms
/// never meet.
pub fn temporary_name(owner: &str) -> String {
    match owner.len() <= MAX_SPELLED_OWNER {
        true => format!("{TEMPORARY_PREFIX}{owner}{TEMPORARY_SUFFIX}"),
        false => format!(
            "{TEMPORARY_PREFIX}#{:016x}{TEMPORARY_SUFFIX}",
            fnv1a(owner.as_bytes())
        ),
    }
}

/// Whether `name` is a temporary name this crate writes.
pub fn is_temporary(name: &str) -> bool {
    name.starts_with(TEMPORARY_PREFIX) & name.ends_with(TEMPORARY_SUFFIX)
}

/// The lock of `placed.json`, held until it is dropped. [`Store::save`] asks for one.
#[derive(Debug)]
pub struct PlacedLock {
    _file: File,
    /// The lock file on disk, which says whose record the lock is for.
    path: PathBuf,
}

impl PlacedLock {
    /// The lock file on disk this lock holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Points the link `<dir>/<name>` at `target`: a link made under the temporary name of `owner`,
/// renamed over the old one, and an `fsync` of `dir`. A temporary link a crash left is replaced.
/// A crash leaves either link whole.
fn set_link(
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

/// What is at `path` on disk, as the [`Owner`] a conflict would name: `None` when nothing is.
pub(crate) fn occupant(fs: &dyn Fs, path: &Path) -> Result<Option<Owner>, PlacedError> {
    let kind = fs.kind(path).map_err(io_at("inspect", path))?;
    Ok(match kind {
        None => None,
        Some(FileKind::Symlink) => Some(Owner::Link(
            fs.read_link(path).map_err(io_at("read the link", path))?,
        )),
        Some(FileKind::File) => Some(Owner::File),
        Some(FileKind::Dir) => Some(Owner::Directory),
        Some(FileKind::Other) => Some(Owner::Other),
    })
}

/// The links and the record of one host.
pub struct Placer<'a> {
    pub(crate) roots: Roots,
    pub(crate) fs: &'a dyn Fs,
    pub(crate) units: &'a dyn UnitControl,
}

impl<'a> Placer<'a> {
    /// Works under `roots`, reading and writing through `fs`, and asking `units` for reloads.
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
        let host = self.fs.host_path(&path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&host)
            .map_err(io_at("open", &path))?;
        file.try_lock().map_err(|e| PlacedError::Locked {
            path,
            source: io::Error::from(e),
        })?;
        Ok(PlacedLock {
            _file: file,
            path: host,
        })
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

    /// Switches `current` of `service` to `to`, setting `previous` to the version
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

    /// `daemon-reload`, under the lock.
    pub fn reload(&self) -> Result<(), PlacedError> {
        let _lock = self.lock()?;
        self.units.daemon_reload()?;
        Ok(())
    }
}
