//! The lock that keeps one `hk xcode add` at a time on a Mac: `~/.hostkeeper/xcode/.lock`, a file
//! made with `O_EXCL` and mode 0600 that holds the process id of the run that made it, held from the
//! first step to the last, since two runs would share the expansion directory beside an archive
//! and the records. [`Lock`] removes the file when it is dropped.
//!
//! A run that finds the file reads its process id: when that process is alive the run fails naming
//! it and never waits; when it is not (a run that was killed, a file with no process id in it)
//! the lock is stale, and the run removes it and makes its own. An existing file is checked as the
//! files of `~/.hostkeeper` are (the user's own, not a link, not writable by group or others).
//! A process id the system has given to another program since is taken for the first run's: the
//! lock then stays until its file is removed by hand, the safe way to be wrong.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};

use crate::home::Home;
use crate::{Flow, PROGRAM, Stop};

/// The lock file in the `xcode` directory.
const NAME: &str = ".lock";

/// The mode of the lock file.
const MODE: u32 = 0o600;

/// A held lock: the file, removed when this value is dropped.
#[derive(Debug)]
pub(crate) struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        // A file that is gone already leaves nothing to let go.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Makes the lock file `path`, which must not exist, holding this process's id. A file whose
/// write fails is left with no id in it, which is a stale lock to the next run.
fn create(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(MODE)
        .open(path)?;
    // The umask only takes bits away; the mode is what it is said to be.
    file.set_permissions(std::fs::Permissions::from_mode(MODE))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

/// The process id the lock file at `path` names, when it names one that is alive.
fn live_holder(path: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    let alive = match test_kill_process(Pid::from_raw(i32::try_from(pid).ok()?)?) {
        Ok(()) => true,
        // The process exists, and is not this user's to signal.
        Err(error) => error == Errno::PERM,
    };
    alive.then_some(pid)
}

/// Takes the lock of `dir` (the `xcode` directory) for the run of `hk <word>`, or fails naming the
/// process that holds it.
pub(crate) fn take(home: &Home, dir: &Path, word: &str) -> Flow<Lock> {
    let path = dir.join(NAME);
    let shown = path.display();
    home.check_file(&path)?;
    let cannot = |error: std::io::Error| Stop::fail(format!("cannot use {shown}: {error}"));
    match create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(pid) = live_holder(&path) {
                return Err(Stop::fail(format!(
                    "another {PROGRAM} {word} is running (process id {pid}; it holds {shown}); \
                     wait for it to end, then run {PROGRAM} {word} again"
                )));
            }
            // Stale: no process of that id, or no id. Removed, and made again.
            std::fs::remove_file(&path)
                .and_then(|()| create(&path))
                .map_err(cannot)?;
        }
        Err(error) => return Err(cannot(error)),
    }
    Ok(Lock { path })
}
