//! The programs outside the repository's own tools other than `aws` and `gpg` (`curl`), behind a
//! trait so a test answers in place of them.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/check.rs`: `Host`), run through the crate's
//! [`Exec`](crate::release::ports::Exec) port.

use std::path::Path;

use super::Error;
use super::aws::{Answer, run_program};
use crate::release::ports::Exec;

/// The programs outside, behind a trait so a test answers in place of them.
pub(crate) trait Host {
    /// `program <args>`. A refusal is an [`Answer`] that is not `ok`.
    ///
    /// # Errors
    /// The program could not be started.
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error>;
}

/// The real programs, run through an [`Exec`].
pub(crate) struct ExecHost<'a>(pub(crate) &'a dyn Exec);

impl Host for ExecHost<'_> {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        run_program(self.0, program, args)
    }
}

#[cfg(test)]
mod tests;
