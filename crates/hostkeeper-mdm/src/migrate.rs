//! `hostkeeper-mdm migrate --from <old settings file>`: the live stack of the old kit moved to
//! this kit's names while no Mac is enrolled (docs/design/hostkeeper-mdm.md section 8.3).

use std::io::Write;

use hostkeeper_mdm_core::migrate::{Old, Target};
use hostkeeper_mdm_core::pins::Pins;
use hostkeeper_mdm_step::Mode;

use crate::{Env, MdmError};

/// What `migrate` was asked for.
pub struct Migrate {
    /// The old kit's settings file.
    pub from: String,
    /// The old kit's settings, read from it.
    pub old: Old,
    /// Where the new settings file goes.
    pub settings: String,
    /// Where `fetch` put the archives.
    pub bin_dir: String,
    /// What the new settings take that the old file does not settle.
    pub target: Target,
    /// The largest archive read, in bytes.
    pub max_archive: u64,
}

/// Why `migrate` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {}

impl MoveError {
    /// The exit code: 1 when a step failed, 2 when the input or the host was refused.
    pub fn exit_code(&self) -> u8 {
        match *self {}
    }
}

/// Check the old and the new state, then print or carry out the move.
///
/// # Errors
/// [`MdmError`].
pub fn run(
    env: &Env<'_>,
    a: &Migrate,
    pins: &Pins,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let _ = (env, a, pins, mode, out);
    Ok(())
}
