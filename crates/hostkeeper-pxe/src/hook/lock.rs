//! The lock a step holds on the boot directory: an exclusive, non-blocking `flock` on the open
//! directory, released when the step ends (the descriptor is closed, also when the step dies).

use std::fs::{DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use rustix::fs::{FlockOperation, flock};

use super::Failure;

/// The held lock.
#[derive(Debug)]
pub(super) struct Lock {
    _directory: File,
}

impl Lock {
    /// Makes `dir` (mode 0755, its parents too) if it is not there, and locks it.
    pub(super) fn create(dir: &Path) -> Result<Lock, Failure> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(dir)
            .map_err(|e| Failure::io(dir, &e))?;
        Lock::open(dir)
    }

    /// Locks the existing directory `dir`; a lock held by another step is the reason `locked`.
    pub(super) fn open(dir: &Path) -> Result<Lock, Failure> {
        let directory = File::open(dir).map_err(|e| Failure::io(dir, &e))?;
        flock(&directory, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            Failure::release(
                "locked",
                format!("{} is locked by another step: {e}", dir.display()),
            )
        })?;
        Ok(Lock {
            _directory: directory,
        })
    }
}
