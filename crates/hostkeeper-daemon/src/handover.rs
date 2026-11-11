//! The hand-over at the start of `hostkeeper daemon` (docs/design/two-programs.md, section 4.3):
//! the first thing the daemon does, before it reads its configuration, is read the record of the
//! service `hostkeeper` for a hand-over and act on it ([`hostkeeper_steps::handover`]).
//!
//! When the record waits for a hand-over into another version than this binary's, the state
//! directory's lock is taken ([`lock_state_dir`]) and steps 1 to 3 run under it: this binary
//! raises the start count and execs `versions/<N>` with its own arguments, the process id kept
//! ([`Exec`]); or, when it cannot, settles the update `Failed`, marks `N` bad, removes `next`
//! and goes on to start as the daemon. The lock is a file opened close-on-exec, so the exec
//! releases it, and the new binary takes it again. A daemon that holds the lock is left alone
//! ([`Handed::Busy`]): this start then stops at the lock, as it always did. When the record
//! waits for this binary, or for no hand-over, nothing is written and no lock is taken here: the
//! settlement of the start (`crate::start`) confirms a hand-over into this binary.
//!
//! What cannot be read or written is said on standard error, and the start goes on: the
//! settlement then reads the same record under the daemon's lock.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_steps::handover::{AtStart, at_start, exec_failed, pending};
use hostkeeper_steps::{Layout, RecordStore, SelfUpdateError};

use crate::selfupdate::binary_dir;
use crate::start::{StartError, lock_state_dir};
use crate::verbs::force::SELF_SERVICE;

/// Replaces this process by another binary, as `execve` does: the process id stays.
pub trait Exec {
    /// Runs `binary` in place of this process. It returns only when that could not be done.
    fn exec(&self, binary: &Path) -> io::Error;
}

/// The exec of the host: `binary` with the arguments this process was started with, after the
/// program name, and its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExec {
    args: Vec<OsString>,
}

impl ProcessExec {
    /// The exec that gives the new binary `args`, the arguments after the program name.
    pub fn new(args: Vec<OsString>) -> ProcessExec {
        ProcessExec { args }
    }
}

impl Exec for ProcessExec {
    fn exec(&self, binary: &Path) -> io::Error {
        Command::new(binary).args(&self.args).exec()
    }
}

/// What the start did about a hand-over ([`hand_over`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Handed {
    /// No hand-over waits: nothing was written.
    NotPending,
    /// The hand-over waits for this binary: its settlement confirms it. Nothing was written.
    Settles,
    /// A daemon holds the state directory: nothing was written.
    Busy,
    /// The new binary could not be started (step 3): the update is settled `Failed`.
    FellBack,
    /// The exec of `binary` returned `error`, and the update is settled `Failed`.
    ExecFailed { binary: PathBuf, error: String },
}

/// Why the hand-over could not be read or acted on.
#[derive(Debug, thiserror::Error)]
pub enum HandOverError {
    /// The record, the list of bad versions or a link could not be read or written.
    #[error(transparent)]
    SelfUpdate(#[from] SelfUpdateError),
    /// The state directory's lock could not be taken for another reason than a daemon holding it.
    #[error(transparent)]
    Lock(#[from] StartError),
}

/// The hand-over at the start of a binary of version `running` whose state directory is
/// `state_dir` (the module's rule), the exec made by `exec`.
pub fn hand_over(
    state_dir: &Path,
    running: &str,
    exec: &dyn Exec,
) -> Result<Handed, HandOverError> {
    let store = RecordStore::new(&state_dir.join(SELF_SERVICE));
    let layout = Layout::new(&binary_dir(state_dir));
    // Held until the exec, which closes it, or until the exec's failure is settled.
    let mut held = None;
    let decided = match pending(&store)? {
        None => AtStart::NotPending,
        Some(awaiting) if awaiting.version == running => AtStart::Settles,
        Some(_) => match lock_state_dir(state_dir) {
            Ok(lock) => {
                held = Some(lock);
                at_start(&layout, &store, running)?
            }
            Err(StartError::Locked { .. }) => return Ok(Handed::Busy),
            Err(error) => return Err(error.into()),
        },
    };
    let binary = match decided {
        AtStart::Exec { binary, .. } => binary,
        AtStart::FellBack => return Ok(Handed::FellBack),
        AtStart::NotPending => return Ok(Handed::NotPending),
        AtStart::Settles => return Ok(Handed::Settles),
    };
    let error = exec.exec(&binary);
    exec_failed(&layout, &store)?;
    drop(held);
    Ok(Handed::ExecFailed {
        binary,
        error: error.to_string(),
    })
}

/// [`hand_over`] for `hostkeeper daemon`: what went wrong is said on standard error, and the start
/// goes on. A hand-over into another version that was left for an error, not for a daemon holding
/// the lock, is then settled `Failed` by the start's settlement (step 3: this binary could not
/// hand over), so no wait is left standing.
pub fn at_daemon_start(state_dir: &Path, running: &str, exec: &dyn Exec) -> Handed {
    match hand_over(state_dir, running, exec) {
        Ok(Handed::ExecFailed { binary, error }) => {
            eprintln!(
                "hostkeeper daemon: {} could not be started, and the update is recorded failed: {error}",
                binary.display()
            );
            Handed::ExecFailed { binary, error }
        }
        Ok(handed) => handed,
        Err(error) => {
            eprintln!("hostkeeper daemon: the hand-over of the update was not read: {error}");
            Handed::NotPending
        }
    }
}
