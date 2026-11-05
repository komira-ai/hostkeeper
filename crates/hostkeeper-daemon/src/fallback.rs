//! `hostkeeper self-update-fallback --dir <dir>`: what the fallback unit of hostkeeper's own
//! update runs, with the *previous* binary, once the new one has failed `StartLimitBurst` starts
//! (design section 6.6; the unit is `hostkeeper_steps::selfupdate::fallback_unit`, unchanged).
//!
//! In this order: `current` of the binary directory `<dir>` is pointed at `previous` again
//! ([`Layout::flip_back`], which refuses a `previous` that is not an executable file); then the
//! update that waits for the restart is settled with the version `current` now names as the
//! binary that runs ([`hostkeeper_steps::selfupdate::settle`]): its version goes on the local
//! list of bad versions and its record says `Failed` after the commit, of class release,
//! `self_update_fallback`. The flip comes first, so that a crash between the two leaves a host
//! that runs the previous binary, whose start settles the same verdict. Run again, it changes
//! nothing. The step record and the list are in the parent of `<dir>`, the state directory of the
//! service `hostkeeper` ([`crate::selfupdate::binary_dir`]).
//!
//! It exits 0 when the previous binary is back, and [`EXIT_FAILED`] with a message naming the
//! cause when there is no `previous`, when it is not an executable file, or when the record or
//! the list cannot be written; with no `previous`, nothing is written.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_steps::selfupdate::settle;
use hostkeeper_steps::{Layout, RecordStore, SelfUpdateError, Settled};

use crate::start::EXIT_FAILED;

/// Why the fallback could not put the previous binary back, or record it.
#[derive(Debug, thiserror::Error)]
pub enum FallbackError {
    /// The binary directory has no `previous`: there is nothing to fall back to.
    #[error("{} has no previous binary to fall back to", .0.display())]
    NoPrevious(PathBuf),
    /// `previous` could not be read, or names what is not an executable file, or `current`
    /// could not be moved.
    #[error("the previous binary cannot be put back: {0}")]
    Flip(#[from] io::Error),
    /// The previous binary is back, and the update could not be recorded failed.
    #[error("the previous binary is back, but the update could not be recorded: {0}")]
    Settle(#[from] SelfUpdateError),
}

/// The fallback for the binary directory `dir`, the step record and the list of bad versions in
/// its parent.
pub fn fallback(dir: &Path) -> Result<Settled, FallbackError> {
    let state = dir.parent().unwrap_or(dir);
    fallback_with(&Layout::new(dir), &RecordStore::new(state))
}

/// The fallback for the binary directory `layout`, the update in the record of `store`: the seam
/// of the tests that stop it at a write of the record.
pub fn fallback_with(layout: &Layout, store: &RecordStore) -> Result<Settled, FallbackError> {
    if layout.previous()?.is_none() {
        return Err(FallbackError::NoPrevious(layout.dir().to_path_buf()));
    }
    layout.flip_back()?;
    let running = layout.current()?.unwrap_or_default();
    Ok(settle(layout, store, &running)?)
}

/// The verb: [`fallback`] for `dir`, its error printed.
pub fn main(dir: &Path) -> ExitCode {
    match fallback(dir) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hostkeeper self-update-fallback: {error}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}
