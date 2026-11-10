//! The host-wide installer lock (packages.md section 5.2): `activate`, `remove` and the
//! dependency check of every service hold it while their tool runs, so two services never run
//! the package manager at once.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The lock file, `<packages>/../installer.lock` on a host.
#[derive(Clone, Debug)]
pub struct InstallerLock {
    path: PathBuf,
}

/// The lock, held until it is dropped.
#[derive(Debug)]
pub struct Held {
    _file: File,
}

impl InstallerLock {
    /// The lock at `path`, created on first use.
    pub fn new(path: &Path) -> InstallerLock {
        InstallerLock {
            path: path.to_path_buf(),
        }
    }

    /// Holds the lock, waiting for another holder to let it go.
    pub fn hold(&self) -> io::Result<Held> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)?;
        file.lock()?;
        Ok(Held { _file: file })
    }

    /// Holds the lock when no other holder has it, else `None`: it never waits.
    pub fn try_hold(&self) -> io::Result<Option<Held>> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)?;
        match file.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            taken => taken
                .map(|()| Some(Held { _file: file }))
                .map_err(io::Error::from),
        }
    }
}
