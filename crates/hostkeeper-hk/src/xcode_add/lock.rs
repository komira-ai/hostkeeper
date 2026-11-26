//! The lock that keeps one `hk xcode add` at a time for a user: `~/.hostkeeper/xcode/.lock`, a file
//! of mode 0600 that a run holds with an exclusive `flock(2)` from the first step to the last,
//! since two runs would share the expansion directory beside an archive (on a Mac) and the
//! records (and, off macOS, one archive would be checked on the farm Mac twice). The
//! system gives the lock up when the run's process ends, however it ends, so there is no stale
//! lock to tell from a live one and no process id to be mistaken for another program's. [`Lock`]
//! holds the open file; dropping it closes the file, which lets the lock go.
//!
//! A run that cannot take the lock because another holds it fails naming that run's process id,
//! which the holder writes into the file once it has the lock (the number is for the message
//! alone: the lock is the `flock`); it never waits and never removes the file, which stays for
//! the next run to lock. An existing file is checked as the files of `~/.hostkeeper` are (the
//! user's own, not a link, not writable by group or others).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;

use crate::home::Home;
use crate::{Flow, PROGRAM, Stop};

/// The lock file in the `xcode` directory.
const NAME: &str = ".lock";

/// The mode of the lock file.
const MODE: u32 = 0o600;

/// A held lock: the open lock file, whose `flock` ends when this value is dropped.
#[derive(Debug)]
pub(crate) struct Lock {
    _file: File,
}

/// The process id the lock file at `path` names, when it names one.
fn holder(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Takes the lock of `dir` (the `xcode` directory) for the run of `hk <word>`, or fails naming the
/// process that holds it.
pub(crate) fn take(home: &Home, dir: &Path, word: &str) -> Flow<Lock> {
    let path = dir.join(NAME);
    let shown = path.display();
    home.check_file(&path)?;
    let cannot = |error: std::io::Error| Stop::fail(format!("cannot use {shown}: {error}"));
    // Not truncated here: the file may hold the id of the run that has the lock.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(MODE)
        .open(&path)
        .map_err(cannot)?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(Errno::WOULDBLOCK) => {
            let who = holder(&path).map_or_else(String::new, |pid| format!("process id {pid}; "));
            return Err(Stop::fail(format!(
                "another {PROGRAM} {word} is running ({who}it holds {shown}); \
                 wait for it to end, then run {PROGRAM} {word} again"
            )));
        }
        Err(error) => return Err(cannot(error.into())),
    }
    // The umask only takes bits away; the mode is what it is said to be.
    file.set_permissions(std::fs::Permissions::from_mode(MODE))
        .and_then(|()| file.set_len(0))
        .and_then(|()| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush())
        .map_err(cannot)?;
    Ok(Lock { _file: file })
}
