//! The hook's record: one file per key in the record directory, the files `prepare` replaced
//! under `saved/`, and the lock one step holds on the directory from the moment it opens it.
//!
//! The record is the hook's own state, kept as plain files in the directory the manifest's
//! params name (`record_dir`), which the hook makes owner-only. A key is recorded by writing its
//! file whole beside it and renaming it into place; a saved file likewise.

use std::fs::{self, DirBuilder, File, TryLockError};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use super::Failure;

/// `prepare` has run; its text is the version it prepared.
pub(crate) const PREPARED: &str = "prepared";
/// `cutover` has begun.
pub(crate) const CUTOVER: &str = "cutover";
/// `verify` has succeeded.
pub(crate) const VERIFIED: &str = "verified";
/// Every key of the record.
pub(crate) const KEYS: [&str; 3] = [PREPARED, CUTOVER, VERIFIED];

/// The directory of the files `prepare` replaced, inside the record directory.
const SAVED: &str = "saved";

/// What a file `prepare` changed was before it: its bytes, or nothing there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Before {
    /// The file held these bytes.
    Held(Vec<u8>),
    /// There was no file.
    Absent,
}

/// The record directory, locked for as long as this value lives.
#[derive(Debug)]
pub(crate) struct Record {
    dir: PathBuf,
    // Held, never read: dropping it releases the lock.
    _lock: File,
}

impl Record {
    /// Makes `dir` (owner only, with its parents) if it is missing, opens it and takes its lock
    /// without waiting. A path that cannot be opened or is not a directory, or a directory that
    /// another step holds, is a failure of class `environment`.
    pub(crate) fn open(dir: &Path) -> Result<Record, Failure> {
        // A failure to make it shows in the open below, which names the path.
        let _ = DirBuilder::new().recursive(true).mode(0o700).create(dir);
        let handle = File::open(dir).map_err(|why| {
            Failure::environment(
                "record",
                format!("cannot open the record directory {}: {why}", dir.display()),
            )
        })?;
        if !handle.metadata().is_ok_and(|meta| meta.is_dir()) {
            return Err(Failure::environment(
                "record",
                format!("the record directory {} is not a directory", dir.display()),
            ));
        }
        match handle.try_lock() {
            Ok(()) => Ok(Record {
                dir: dir.to_owned(),
                _lock: handle,
            }),
            Err(TryLockError::WouldBlock) => Err(Failure::environment(
                "record_locked",
                "another step holds the record".to_owned(),
            )),
            Err(TryLockError::Error(why)) => Err(Failure::environment(
                "record",
                format!("cannot lock the record directory {}: {why}", dir.display()),
            )),
        }
    }

    /// The text of `key`, `None` when the key is not recorded.
    pub(crate) fn get(&self, key: &str) -> Result<Option<String>, Failure> {
        let bytes = read(&self.dir.join(key))?;
        Ok(bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Whether `key` is recorded.
    pub(crate) fn has(&self, key: &str) -> Result<bool, Failure> {
        Ok(self.get(key)?.is_some())
    }

    /// Records `key` as `value`.
    pub(crate) fn set(&self, key: &str, value: &str) -> Result<(), Failure> {
        write_whole(&self.dir, key, value.as_bytes())
    }

    /// Forgets `key`; a key that is not recorded is already forgotten.
    pub(crate) fn remove(&self, key: &str) -> Result<(), Failure> {
        remove_file(&self.dir.join(key))
    }

    /// Keeps what `path` was before `prepare` changed it, unless something is kept for it
    /// already: a second `prepare` keeps what the first found, not what the first wrote.
    pub(crate) fn save(&self, path: &str, before: &Before) -> Result<(), Failure> {
        let dir = self.dir.join(SAVED);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&dir)
            .map_err(|why| failure("create", &dir, &why))?;
        if self.saved(path)?.is_some() {
            return Ok(());
        }
        match before {
            Before::Held(bytes) => write_whole(&dir, &name(path), bytes),
            Before::Absent => write_whole(&dir, &format!("{}.absent", name(path)), b""),
        }
    }

    /// What is kept for `path`, `None` when nothing is.
    pub(crate) fn saved(&self, path: &str) -> Result<Option<Before>, Failure> {
        let dir = self.dir.join(SAVED);
        if let Some(bytes) = read(&dir.join(name(path)))? {
            return Ok(Some(Before::Held(bytes)));
        }
        let absent = read(&dir.join(format!("{}.absent", name(path))))?;
        Ok(absent.map(|_| Before::Absent))
    }

    /// Forgets everything kept under `saved/`.
    pub(crate) fn forget_saved(&self) -> Result<(), Failure> {
        let dir = self.dir.join(SAVED);
        let names = match fs::read_dir(&dir) {
            Ok(names) => names,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(why) => return Err(failure("read", &dir, &why)),
        };
        for entry in names {
            let entry = entry.map_err(|why| failure("read", &dir, &why))?;
            remove_file(&entry.path())?;
        }
        fs::remove_dir(&dir).map_err(|why| failure("remove", &dir, &why))
    }
}

/// The bytes of the file at `path`, `None` when nothing is there.
fn read(path: &Path) -> Result<Option<Vec<u8>>, Failure> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(why) => Err(failure("read", path, &why)),
    }
}

/// The name a saved file of `path` has under `saved/`: the path with each `/` as `%`, which
/// no path the hook saves holds.
fn name(path: &str) -> String {
    path.replace('/', "%")
}

/// Writes `bytes` to `dir/key` whole: beside it first, then renamed over it.
fn write_whole(dir: &Path, key: &str, bytes: &[u8]) -> Result<(), Failure> {
    let path = dir.join(key);
    let beside = dir.join(format!(".{key}.new"));
    fs::write(&beside, bytes)
        .and_then(|()| fs::rename(&beside, &path))
        .map_err(|why| failure("write", &path, &why))
}

/// Removes a file; one that is not there is already removed.
fn remove_file(path: &Path) -> Result<(), Failure> {
    match fs::remove_file(path) {
        Err(why) if why.kind() != io::ErrorKind::NotFound => Err(failure("remove", path, &why)),
        _ => Ok(()),
    }
}

fn failure(what: &str, path: &Path, why: &io::Error) -> Failure {
    Failure::environment("record", format!("cannot {what} {}: {why}", path.display()))
}
