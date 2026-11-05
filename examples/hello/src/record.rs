//! The hook record: one file per key in the record directory, the slot markers in `slots/`, and
//! the lock that one step holds on the directory from the moment it opens it.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::report::Failure;

/// The slot that serves outside an update; absent on a first install.
pub const ACTIVE: &str = "active";
/// The slot the update starts.
pub const NEXT: &str = "next";
/// `prepare` has run.
pub const PREPARED: &str = "prepared";
/// `cutover` has begun.
pub const CUTOVER: &str = "cutover";
/// `verify` has succeeded.
pub const VERIFIED: &str = "verified";
/// Every key of the record.
pub const KEYS: [&str; 5] = [ACTIVE, NEXT, PREPARED, CUTOVER, VERIFIED];

/// The directory of the slot markers, inside the record directory.
const SLOTS: &str = "slots";

/// One of the two slots, `a` and `b`; one of them serves at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    /// Both slots.
    pub const ALL: [Slot; 2] = [Slot::A, Slot::B];

    /// The name in the record, in the marker and in the unit.
    pub const fn name(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }

    /// The slot an update starts when `self` serves.
    pub const fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
}

/// The record directory, locked for as long as this value lives.
#[derive(Debug)]
pub struct Record {
    dir: PathBuf,
    // Held, never read: dropping it releases the lock.
    _lock: File,
}

impl Record {
    /// Opens `dir` and takes its lock without waiting. A directory that is missing, or that
    /// another step holds, is a failure of class `environment`.
    pub fn open(dir: &Path) -> Result<Record, Failure> {
        let handle = File::open(dir).map_err(|why| {
            Failure::environment(format!("cannot open the record directory {}: {why}", dir.display()))
        })?;
        if !handle.metadata().is_ok_and(|meta| meta.is_dir()) {
            return Err(Failure::environment(format!(
                "the record directory {} is not a directory",
                dir.display()
            )));
        }
        match handle.try_lock() {
            Ok(()) => Ok(Record {
                dir: dir.to_owned(),
                _lock: handle,
            }),
            Err(TryLockError::WouldBlock) => {
                Err(Failure::environment("another step holds the record"))
            }
            Err(TryLockError::Error(why)) => Err(Failure::environment(format!(
                "cannot lock the record directory {}: {why}",
                dir.display()
            ))),
        }
    }

    /// The text of `key`, `None` when the key is not recorded.
    pub fn get(&self, key: &str) -> Result<Option<String>, Failure> {
        let path = self.dir.join(key);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(why) => Err(failure("read", &path, &why)),
        }
    }

    /// Whether `key` is recorded.
    pub fn has(&self, key: &str) -> Result<bool, Failure> {
        Ok(self.get(key)?.is_some())
    }

    /// The slot recorded under `key`.
    pub fn slot(&self, key: &str) -> Result<Option<Slot>, Failure> {
        let Some(text) = self.get(key)? else {
            return Ok(None);
        };
        Slot::ALL
            .into_iter()
            .find(|slot| slot.name() == text.trim())
            .map(Some)
            .ok_or_else(|| {
                Failure::environment(format!("the record key {key} holds {:?}, not a slot", text.trim()))
            })
    }

    /// Records `key` as `value`: the file is written whole beside the key and renamed over it.
    pub fn set(&self, key: &str, value: &str) -> Result<(), Failure> {
        let path = self.dir.join(key);
        let beside = self.dir.join(format!(".{key}.new"));
        fs::write(&beside, value)
            .and_then(|()| fs::rename(&beside, &path))
            .map_err(|why| failure("write", &path, &why))
    }

    /// Records the slot under `key`.
    pub fn set_slot(&self, key: &str, slot: Slot) -> Result<(), Failure> {
        self.set(key, slot.name())
    }

    /// Forgets `key`; a key that is not recorded is already forgotten.
    pub fn remove(&self, key: &str) -> Result<(), Failure> {
        remove_file(&self.dir.join(key))
    }

    /// Creates the marker of `slot`: the unit of a slot with no marker never starts.
    pub fn mark(&self, slot: Slot) -> Result<(), Failure> {
        let dir = self.dir.join(SLOTS);
        let path = dir.join(slot.name());
        fs::create_dir_all(&dir)
            .and_then(|()| fs::write(&path, ""))
            .map_err(|why| failure("create", &path, &why))
    }

    /// Removes the marker of `slot`.
    pub fn unmark(&self, slot: Slot) -> Result<(), Failure> {
        remove_file(&self.dir.join(SLOTS).join(slot.name()))
    }

    /// Removes the directory of the markers, when it is empty.
    pub fn remove_slots(&self) -> Result<(), Failure> {
        let path = self.dir.join(SLOTS);
        match fs::remove_dir(&path) {
            Err(why) if why.kind() != io::ErrorKind::NotFound => {
                Err(failure("remove", &path, &why))
            }
            _ => Ok(()),
        }
    }
}

/// Removes a file; one that is not there is already removed.
fn remove_file(path: &Path) -> Result<(), Failure> {
    match fs::remove_file(path) {
        Err(why) if why.kind() != io::ErrorKind::NotFound => Err(failure("remove", path, &why)),
        _ => Ok(()),
    }
}

fn failure(what: &str, path: &Path, why: &io::Error) -> Failure {
    Failure::environment(format!("cannot {what} {}: {why}", path.display()))
}
