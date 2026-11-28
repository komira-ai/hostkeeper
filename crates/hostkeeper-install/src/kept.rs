//! The files a first install landed on that no package owned (packages-lifecycle.md section 6.5,
//! "A file nobody owns is kept at a first install"): an operator's hand-placed file at a payload
//! path. An installer's `activate` of a first install (no record of the service yet) keeps an
//! exact copy of each, its bytes, mode, owner and group, and those of its parent directories,
//! before the package manager replaces it; the service's removal, however it comes (a failed
//! first install's rollback or an uninstall), puts each copy back at its path once the package is
//! gone, and drops the copies. An update keeps them: the file is still the operator's when the
//! package is removed.
//!
//! The store is root's alone: `<kept>/<service>/` (`/var/lib/hostkeeper/kept/<service>/` on a
//! host), mode 0700 like its parent, holding [`INDEX`] and one copy per file, each 0600 from its
//! creation. A set is written whole into `<kept>/.<service>.new` and renamed into place, so a
//! crash leaves the earlier set or the new one (a service name never begins with `.`).
//!
//! A removal puts the set back while the service's record is still there, and drops the set
//! once the record is gone; with no record, a set is only dropped: the install it was kept for
//! never ran, or a removal already put it back. A copy that cannot be put back (missing,
//! unreadable, its bytes no longer its recorded digest, or its path unwritable) is [`Lost`]: the
//! removal completes, the path stays as the removal left it, the set is set aside in
//! `<kept>/.lost-<service>/` with the copies it still holds, and the daemon raises an alert
//! naming the path and the copy. Nothing here fails the removal but a set aside or a drop of the
//! set that cannot be made, which the removal runs again.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, fchown};
use std::path::{Path, PathBuf};

use hostkeeper_config::names::ServiceName;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The index of a service's set, in its directory.
pub const INDEX: &str = "kept.toml";
/// The mode of the store, of each service's directory, and of each new directory a put-back
/// makes before it is given its recorded mode.
pub const DIR_MODE: u32 = 0o700;
/// The mode of the index and of each copy.
pub const FILE_MODE: u32 = 0o600;

/// A directory above a kept file, as it was when the file was kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptDir {
    /// Its logical absolute path (`/etc/js-agent`).
    pub path: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

/// A kept file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptFile {
    /// Its logical absolute path (`/etc/js-agent/flags`).
    pub path: String,
    /// The copy's name in the service's directory.
    pub copy: String,
    /// The SHA-256 of its bytes, lower-case hex.
    pub sha256: String,
    /// Its permission bits (`0o7777`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// The directories above it, `/` excluded, outermost first: those a put-back finds gone are
    /// made again as they were.
    pub parents: Vec<KeptDir>,
}

/// A service's set: the package it was kept for and its files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptSet {
    /// The package (the `apt` package, the `macos-pkg` identifier) whose first install landed on
    /// the files.
    pub package: String,
    pub files: Vec<KeptFile>,
}

/// A kept file that was not put back, and why: the alert's path and detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lost {
    /// The logical path left as the removal left it; the index's own path when the index cannot
    /// be read, which names no file.
    pub path: String,
    pub why: String,
    /// Where the copy still is, set aside for the operator ([`Kept::aside_of`]); `None` when
    /// none is left (the copy itself was missing).
    pub copy: Option<PathBuf>,
}

/// Why the store could not be written.
#[derive(Debug, thiserror::Error)]
#[error("the kept files of {service}: {what}: {source}")]
pub struct KeptError {
    service: String,
    what: String,
    #[source]
    source: io::Error,
}

impl KeptError {
    fn new(service: &ServiceName, what: impl Into<String>, source: io::Error) -> KeptError {
        KeptError {
            service: service.to_string(),
            what: what.into(),
            source,
        }
    }
}

/// The store, over the host whose root is `root`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    dir: PathBuf,
    root: PathBuf,
}

impl Kept {
    /// The store at `dir` (`/var/lib/hostkeeper/kept`), for the host whose root is `root` (`/`,
    /// a scratch directory in a test).
    pub fn new(dir: &Path, root: &Path) -> Kept {
        Kept {
            dir: dir.to_path_buf(),
            root: root.to_path_buf(),
        }
    }

    /// The directory of `service`'s set.
    pub fn dir_of(&self, service: &ServiceName) -> PathBuf {
        self.dir.join(service.as_str())
    }

    /// Where the logical absolute path `path` is on this host.
    fn real(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// The logical paths of `tree`'s entries that are not directories (a package's payload, its
    /// staged `tree/` standing for `/`) at which this host holds a regular file now: the paths a
    /// first install would replace. An entry whose name is not UTF-8 is not one, nor is a path
    /// the host cannot say what it holds (nothing is kept for either, as before this store).
    pub fn candidates(&self, tree: &Path) -> io::Result<Vec<String>> {
        let mut found = Vec::new();
        let mut todo = vec![(tree.to_path_buf(), String::new())];
        while let Some((dir, logical)) = todo.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let path = format!("{logical}/{name}");
                if entry.file_type()?.is_dir() {
                    todo.push((entry.path(), path));
                } else if is_file(&self.real(&path)) {
                    found.push(path);
                }
            }
        }
        found.sort();
        Ok(found)
    }

    /// Keeps a copy of the file at each of `paths` for `service`'s first install of `package`,
    /// in place of any set kept before. A path that holds no regular file any longer is not
    /// kept. With no path at all, any earlier set is dropped and nothing is written.
    pub fn keep(
        &self,
        service: &ServiceName,
        package: &str,
        paths: &[String],
    ) -> Result<(), KeptError> {
        let fail = |what: &str, e| KeptError::new(service, what, e);
        if paths.is_empty() {
            return self.drop_set(service);
        }
        make_closed(&self.dir).map_err(|e| fail("the store", e))?;
        let staging = self.dir.join(format!(".{}.new", service.as_str()));
        remove_tree(&staging).map_err(|e| fail("the staging directory", e))?;
        make_closed(&staging).map_err(|e| fail("the staging directory", e))?;
        let mut set = KeptSet {
            package: package.to_owned(),
            files: Vec::new(),
        };
        for path in paths {
            let copy = set.files.len().to_string();
            if let Some(file) =
                self.copy_in(path, &staging.join(&copy), copy).map_err(|e| fail(path.as_str(), e))?
            {
                set.files.push(file);
            }
        }
        write_index(&staging.join(INDEX), &set).map_err(|e| fail(INDEX, e))?;
        let dir = self.dir_of(service);
        remove_tree(&dir).map_err(|e| fail("the earlier set", e))?;
        fs::rename(&staging, &dir)
            .and_then(|()| sync_dir(&self.dir))
            .map_err(|e| fail("the set", e))
    }

    /// The copy of the file at the logical `path` written to `to`, and its entry; `None` when
    /// the path holds no regular file (nothing, a link, which is not followed, a directory, a
    /// fifo, which is not waited on).
    fn copy_in(&self, path: &str, to: &Path, copy: String) -> io::Result<Option<KeptFile>> {
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.real(path));
        let mut from = match opened {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
            opened => opened?,
        };
        let meta = from.metadata()?;
        if !meta.is_file() {
            return Ok(None);
        }
        let mut out = new_closed(to)?;
        let sha256 = copy_hashing(&mut from, &mut out)?;
        out.sync_all()?;
        let mut parents = Vec::new();
        for parent in crate::host::parents(path) {
            let dir = fs::symlink_metadata(self.real(&parent))?;
            parents.push(KeptDir {
                path: parent,
                mode: dir.mode() & 0o7777,
                uid: dir.uid(),
                gid: dir.gid(),
            });
        }
        Ok(Some(KeptFile {
            path: path.to_owned(),
            copy,
            sha256,
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
            parents,
        }))
    }

    /// `service`'s set, `None` when it has none; a set directory with no index, or one that is
    /// not the index's TOML, is an error.
    pub fn set_of(&self, service: &ServiceName) -> io::Result<Option<KeptSet>> {
        let dir = self.dir_of(service);
        match fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            found => found?,
        };
        let text = fs::read_to_string(dir.join(INDEX))?;
        toml::from_str(&text)
            .map(Some)
            .map_err(|e| io::Error::other(e.message().to_owned()))
    }

    /// Where the copies of `service`'s files that a removal could not put back are set aside:
    /// `<kept>/.lost-<service>/`, which no set's drop touches and a service name cannot be.
    pub fn aside_of(&self, service: &ServiceName) -> PathBuf {
        self.dir.join(format!(".lost-{}", service.as_str()))
    }

    /// Puts each file of `service`'s set back at its path: what a removal runs once the package
    /// is gone and before it drops the service's record. The set stays until [`Kept::drop_set`],
    /// which the removal runs once the record is dropped, so a removal run again after a crash
    /// between the two puts the files back again. Each file not put back is [`Lost`]; then the
    /// whole set is set aside ([`Kept::aside_of`], in place of one set aside before), so that the
    /// operator can put such a file back by hand from its copy, and the next run finds no set.
    /// With no set, nothing is put back. `Err` only when a set with a lost file cannot be set
    /// aside, which leaves it for the removal's next run.
    pub fn put_back(&self, service: &ServiceName) -> Result<Vec<Lost>, KeptError> {
        let dir = self.dir_of(service);
        let mut lost: Vec<(Lost, String)> = match self.set_of(service) {
            Ok(None) => Vec::new(),
            Ok(Some(set)) => set
                .files
                .iter()
                .filter_map(|file| {
                    self.put_file(&dir, file).err().map(|e| {
                        let lost = Lost {
                            path: file.path.clone(),
                            why: e.to_string(),
                            copy: None,
                        };
                        (lost, file.copy.clone())
                    })
                })
                .collect(),
            Err(e) => vec![(
                Lost {
                    path: dir.join(INDEX).display().to_string(),
                    why: format!("the index of the kept files cannot be read: {e}"),
                    copy: None,
                },
                String::new(),
            )],
        };
        if lost.is_empty() {
            return Ok(Vec::new());
        }
        let aside = self.aside_of(service);
        let fail = |e| KeptError::new(service, "the set aside", e);
        remove_tree(&aside).map_err(fail)?;
        fs::rename(&dir, &aside)
            .and_then(|()| sync_dir(&self.dir))
            .map_err(fail)?;
        for (one, copy) in &mut lost {
            // An index that cannot be read names no copy: what is left is the whole set.
            let at = match copy.is_empty() {
                true => aside.clone(),
                false => aside.join(copy.as_str()),
            };
            one.copy = fs::symlink_metadata(&at).is_ok().then_some(at);
        }
        Ok(lost.into_iter().map(|(one, _)| one).collect())
    }

    /// Puts the copy of `file` back at its path: the parents it finds gone made again as they
    /// were, the bytes written beside the path and checked against the recorded digest, given
    /// the recorded owner, group and mode, then renamed over the path.
    fn put_file(&self, dir: &Path, file: &KeptFile) -> io::Result<()> {
        let mut from = File::open(dir.join(&file.copy))
            .map_err(|e| io::Error::new(e.kind(), format!("the kept copy: {e}")))?;
        let real = self.real(&file.path);
        let (Some(parent), Some(name)) = (real.parent(), real.file_name()) else {
            return Err(io::Error::other("the path names no file"));
        };
        for kept in &file.parents {
            let at = self.real(&kept.path);
            if fs::symlink_metadata(&at).is_err() {
                DirBuilder::new().mode(DIR_MODE).create(&at)?;
                std::os::unix::fs::lchown(&at, Some(kept.uid), Some(kept.gid))?;
                fs::set_permissions(&at, fs::Permissions::from_mode(kept.mode))?;
            }
        }
        let mut temporary = name.to_os_string();
        temporary.push(".hostkeeper-kept");
        let temporary = parent.join(temporary);
        remove_tree(&temporary)?;
        let written = (|| {
            let mut out = new_closed(&temporary)?;
            let sha256 = copy_hashing(&mut from, &mut out)?;
            if sha256 != file.sha256 {
                return Err(io::Error::other(format!(
                    "the kept copy's SHA-256 is {sha256}, not the recorded {}",
                    file.sha256
                )));
            }
            fchown(&out, Some(file.uid), Some(file.gid))?;
            out.set_permissions(fs::Permissions::from_mode(file.mode))?;
            out.sync_all()?;
            fs::rename(&temporary, &real)?;
            sync_dir(parent)
        })();
        if written.is_err() {
            drop(fs::remove_file(&temporary));
        }
        written
    }

    /// Drops `service`'s set, and any staging directory a crashed keep left.
    pub fn drop_set(&self, service: &ServiceName) -> Result<(), KeptError> {
        let fail = |e| KeptError::new(service, "the set's drop", e);
        remove_tree(&self.dir.join(format!(".{}.new", service.as_str()))).map_err(fail)?;
        remove_tree(&self.dir_of(service)).map_err(fail)?;
        match fs::metadata(&self.dir) {
            Ok(_) => sync_dir(&self.dir).map_err(fail),
            Err(_) => Ok(()),
        }
    }
}

/// The paths of `candidates` that are not in `owned`.
pub fn unowned(candidates: &[String], owned: &BTreeSet<String>) -> Vec<String> {
    candidates
        .iter()
        .filter(|path| !owned.contains(*path))
        .cloned()
        .collect()
}

/// Whether `path` is a regular file, not a link to one; `false` when it cannot be read.
fn is_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file())
}

/// Copies `from` into `to`, answering the SHA-256 of what was copied.
fn copy_hashing(from: &mut File, to: &mut File) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = from.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
        to.write_all(&buffer[..read])?;
    }
}

/// A new file at `path`, [`FILE_MODE`] from its creation.
fn new_closed(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
}

/// Writes `set` as TOML to the new file `path`, [`FILE_MODE`], durably.
fn write_index(path: &Path, set: &KeptSet) -> io::Result<()> {
    let text = toml::to_string(set).map_err(io::Error::other)?;
    let mut file = new_closed(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Makes `dir` (and its missing parents) and sets it [`DIR_MODE`], whatever it was.
fn make_closed(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match DirBuilder::new().mode(DIR_MODE).create(dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        made => made?,
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))
}

/// Flushes `dir`'s entries.
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Removes `path` and everything below it; nothing to do when it is not there.
fn remove_tree(path: &Path) -> io::Result<()> {
    crate::host::remove_tree(path)
}

#[cfg(test)]
#[path = "kept_tests.rs"]
mod tests;
