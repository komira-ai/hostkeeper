//! The host's file system as an installer sees it: logical absolute paths (`/Library/...`, as
//! the tools print them) under a root directory, which is `/` on a host and a scratch directory
//! in a test, and the accounts a check trusts, which are root, `wheel` and `admin` on a Mac and
//! the test's own on a test host.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// `wheel`'s group id on a Mac.
pub const GID_WHEEL: u32 = 0;
/// `admin`'s group id on a Mac.
pub const GID_ADMIN: u32 = 80;

/// The accounts the checks know by role, as the host numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ids {
    /// The owner every installed and staged entry must have: root, 0 on a Mac.
    pub root: u32,
    /// The group `wheel`.
    pub wheel: u32,
    /// The group `admin`, which may write a parent directory of a payload path.
    pub admin: u32,
}

impl Ids {
    /// A Mac's: root 0, `wheel` 0, `admin` 80.
    pub const MAC: Ids = Ids {
        root: 0,
        wheel: GID_WHEEL,
        admin: GID_ADMIN,
    };

    /// `uid` as a Mac numbers it: 0 for [`Ids::root`], itself otherwise (never 0).
    pub fn uid(&self, uid: u32) -> u32 {
        match uid == self.root {
            true => 0,
            false => uid.max(1),
        }
    }

    /// `gid` as a Mac numbers it: 0 for [`Ids::wheel`], 80 for [`Ids::admin`], itself otherwise
    /// (never 0 or 80).
    pub fn gid(&self, gid: u32) -> u32 {
        if gid == self.wheel {
            GID_WHEEL
        } else if gid == self.admin {
            GID_ADMIN
        } else if gid == GID_WHEEL || gid == GID_ADMIN {
            u32::MAX
        } else {
            gid
        }
    }
}

/// The kind of an entry, as a `Bom` and a walk name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    File,
    Dir,
    Link,
    /// A device, a fifo or a socket.
    Other,
}

impl Kind {
    /// The kind of `mode`'s file-type bits.
    pub fn of_mode(mode: u32) -> Kind {
        match mode & 0o170000 {
            0o100000 => Kind::File,
            0o040000 => Kind::Dir,
            0o120000 => Kind::Link,
            _ => Kind::Other,
        }
    }
}

/// One entry: its kind, its permission bits (`0o7777`), and its owner and group as a Mac numbers
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Meta {
    /// The entry's metadata, read without following a link.
    pub fn of(meta: &fs::Metadata, ids: &Ids) -> Meta {
        Meta {
            kind: Kind::of_mode(meta.mode()),
            mode: meta.mode() & 0o7777,
            uid: ids.uid(meta.uid()),
            gid: ids.gid(meta.gid()),
        }
    }
}

/// The host: the directory that stands for `/`, and its accounts.
#[derive(Clone, Debug)]
pub struct Host {
    root: PathBuf,
    ids: Ids,
}

impl Host {
    /// A Mac's own file system.
    pub fn mac() -> Host {
        Host::new(Path::new("/"), Ids::MAC)
    }

    /// The file system under `root`, with the accounts `ids`.
    pub fn new(root: &Path, ids: Ids) -> Host {
        Host {
            root: root.to_owned(),
            ids,
        }
    }

    /// The directory that stands for `/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The accounts the checks trust.
    pub fn ids(&self) -> &Ids {
        &self.ids
    }

    /// Where the logical absolute path `path` is on this host.
    pub fn real(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// The metadata of `path`, without following a link; `None` when nothing is there.
    pub fn meta(&self, path: &str) -> io::Result<Option<Meta>> {
        meta_of(&self.real(path), &self.ids)
    }

    /// Whether a directory above `path` (below `/`) is a link, or is not a directory: a delete
    /// through it would reach another place than the path names.
    pub fn through_link(&self, path: &str) -> io::Result<bool> {
        for parent in parents(path) {
            match self.meta(&parent)? {
                Some(meta) if meta.kind != Kind::Dir => return Ok(true),
                _ => {}
            }
        }
        Ok(false)
    }

    /// Deletes the file or link at `path`; a directory, or nothing there, is left.
    pub fn delete_file(&self, path: &str) -> io::Result<bool> {
        match self.meta(path)? {
            Some(meta) if meta.kind != Kind::Dir => {
                fs::remove_file(self.real(path))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Deletes the directory at `path` when it is empty; one that is not, or not there, is left.
    pub fn delete_empty_dir(&self, path: &str) -> io::Result<bool> {
        match self.meta(path)? {
            Some(meta) if meta.kind == Kind::Dir => match fs::read_dir(self.real(path))?.next() {
                None => {
                    fs::remove_dir(self.real(path))?;
                    Ok(true)
                }
                Some(_) => Ok(false),
            },
            _ => Ok(false),
        }
    }
}

/// The metadata of `real`, without following a link; `None` when nothing is there.
pub fn meta_of(real: &Path, ids: &Ids) -> io::Result<Option<Meta>> {
    match fs::symlink_metadata(real) {
        Ok(meta) => Ok(Some(Meta::of(&meta, ids))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The logical directories above `path`, `/` excluded, outermost first: `/Library` and
/// `/Library/LaunchDaemons` for `/Library/LaunchDaemons/x.plist`.
pub fn parents(path: &str) -> Vec<String> {
    let names: Vec<&str> = path.split('/').filter(|name| !name.is_empty()).collect();
    (1..names.len())
        .map(|n| format!("/{}", names[..n].join("/")))
        .collect()
}

/// Every entry under `dir`, by its logical path (`/Library/...`, `dir` standing for `/`), with
/// its metadata, links not followed. `dir` itself is not listed.
pub fn walk(dir: &Path, ids: &Ids) -> io::Result<BTreeMap<String, Meta>> {
    let mut found = BTreeMap::new();
    let mut todo = vec![dir.to_owned()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next)? {
            let path = entry?.path();
            let meta = Meta::of(&fs::symlink_metadata(&path)?, ids);
            if meta.kind == Kind::Dir {
                todo.push(path.clone());
            }
            found.insert(logical(dir, &path)?, meta);
        }
    }
    Ok(found)
}

/// `path` below `dir` as a logical absolute path, `dir` standing for `/`.
fn logical(dir: &Path, path: &Path) -> io::Result<String> {
    let rel = path
        .strip_prefix(dir)
        .map_err(|_| io::Error::other("an entry outside the walked directory"))?;
    let mut out = String::new();
    for part in rel.components() {
        let Component::Normal(name) = part else {
            return Err(io::Error::other("an entry name that is not a plain name"));
        };
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("an entry name that is not UTF-8"))?;
        out.push('/');
        out.push_str(name);
    }
    Ok(out)
}

/// The SHA-256 of the file at `path`, as 64 lower-case hex digits; `None` when it is no regular
/// file (a link, a directory, nothing).
pub fn file_digest(path: &Path) -> io::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

/// The target text of the link at `path`; `None` when it is no link.
pub fn link_target(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(Some(fs::read_link(path)?)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Deletes the tree at `path`, making each directory writable first so a sealed (0555) tree
/// goes too; nothing there is done already.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return fs::remove_file(path);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    fs::remove_dir(path)
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
