//! The lock that keeps a second coordinator off the same state files (design section 8.1: two
//! coordinators are not supported, and a second `serve` on the same files is refused).
//!
//! `serve` takes an exclusive advisory lock on `<state-file>.lock` before it opens any state
//! file (the state file, the ops file, the halts file and the spent token ids beside them) and
//! holds it for as long as the process runs ([`crate::config::Parts::lock`]). Taking it is a port
//! ([`StateLock`]): [`FileLock`] is the real one, `flock` through [`std::fs::File::try_lock`];
//! a test gives a fake, so no test needs a second process.

use std::any::Any;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Why the lock was not taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another holder has it: another coordinator runs on these files.
    #[error("held by another process")]
    Held,
    /// The lock file could not be opened or locked.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What keeps a lock held: dropping it lets the lock go.
pub struct LockGuard {
    _held: Box<dyn Any + Send + Sync>,
}

impl LockGuard {
    /// A guard that holds the lock as long as `held` lives.
    #[must_use]
    pub fn new(held: impl Any + Send + Sync) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

/// Takes the exclusive lock of a path, without waiting.
pub trait StateLock {
    /// The lock of `path`, held until the guard is dropped.
    ///
    /// # Errors
    /// [`LockError::Held`]: another holder has it. [`LockError::Io`]: it could not be taken.
    fn acquire(&self, path: &Path) -> Result<LockGuard, LockError>;
}

/// The real lock: `flock(LOCK_EX | LOCK_NB)` on the file at the path, created owner-only if it
/// is not there. The lock goes with the open file, so it ends when the process does, however it
/// ends.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileLock;

impl StateLock for FileLock {
    fn acquire(&self, path: &Path) -> Result<LockGuard, LockError> {
        let file: File = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let tried = file.try_lock();
        locked(tried, file)
    }
}

/// The guard of `file` when `tried`, its try to lock it, took the lock; the error otherwise.
pub(crate) fn locked(tried: Result<(), TryLockError>, file: File) -> Result<LockGuard, LockError> {
    match tried {
        Ok(()) => Ok(LockGuard::new(file)),
        Err(TryLockError::WouldBlock) => Err(LockError::Held),
        Err(TryLockError::Error(why)) => Err(LockError::Io(why)),
    }
}

/// The lock file of the state file at `state`: its name with `.lock` added.
#[must_use]
pub fn lock_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".lock");
    PathBuf::from(path)
}
