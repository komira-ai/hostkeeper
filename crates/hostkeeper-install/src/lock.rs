//! The host-wide installer lock (packages design section 5.2): `activate` and `remove` of a
//! native installer hold it for their tool runs, so two services never run `installer` at once.
//! It is an advisory lock on one file of the daemon's state directory, waited for; the deadline
//! of the step that holds it bounds the wait.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The lock's file, which the daemon creates when it first takes it.
#[derive(Clone, Debug)]
pub struct InstallerLock {
    path: PathBuf,
}

/// The lock while it is held; dropping it lets the next holder in.
#[derive(Debug)]
pub struct Held {
    _file: File,
}

impl InstallerLock {
    /// The lock of the file at `path`.
    pub fn new(path: &Path) -> InstallerLock {
        InstallerLock {
            path: path.to_owned(),
        }
    }

    /// Waits for the lock, and holds it until the value returned is dropped.
    pub fn hold(&self) -> io::Result<Held> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)?;
        file.lock()?;
        Ok(Held { _file: file })
    }

    /// Takes the lock if no one holds it; `None` when someone does.
    pub fn try_hold(&self) -> io::Result<Option<Held>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Held { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
