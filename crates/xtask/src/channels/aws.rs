//! The `aws` program, behind a trait so a test answers in place of it. The credentials are the
//! job's own (`AWS_*` of the role the workflow assumed); nothing here reads the environment.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/aws.rs`); the program runs through the crate's
//! [`Exec`](crate::release::ports::Exec) port, which is the only code that starts a process.

use std::path::{Path, PathBuf};

use super::Error;
use crate::release::ports::Exec;

/// What one run of a program came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    /// Whether it exited with status 0.
    pub(crate) ok: bool,
    /// What it printed on standard output.
    pub(crate) stdout: String,
    /// What it printed on standard error.
    pub(crate) stderr: String,
}

/// `program <args>` through `exec`. A program that exits with a failure is an [`Answer`] that is
/// not `ok`; one that could not be started is an error.
pub(crate) fn run_program(
    exec: &dyn Exec,
    program: &Path,
    args: &[String],
) -> Result<Answer, Error> {
    let ran = exec
        .spawn(&program.to_string_lossy(), args)
        .map_err(Error::spawn(program.display()))?;
    Ok(Answer {
        ok: ran.success,
        stdout: String::from_utf8_lossy(&ran.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&ran.stderr).into_owned(),
    })
}

/// Is `name` a bucket name (lower case letters, digits, `-` and `.`)?
pub(crate) fn is_bucket(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// Is `id` a CloudFront distribution id (upper case letters and digits)?
pub(crate) fn is_distribution_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// What the channel verbs ask of AWS.
pub(crate) trait Aws {
    /// `aws <args>`. A refusal by AWS is an [`Answer`] that is not `ok`, not an error: the verbs
    /// read it (a `412` is an answer to the question "is this file there already").
    ///
    /// # Errors
    /// The program could not be started.
    fn run(&self, args: &[String]) -> Result<Answer, Error>;
}

/// The `aws` program at a path, run through an [`Exec`].
pub(crate) struct ExecAws<'a> {
    exec: &'a dyn Exec,
    program: PathBuf,
}

impl<'a> ExecAws<'a> {
    pub(crate) fn new(exec: &'a dyn Exec, program: PathBuf) -> ExecAws<'a> {
        ExecAws { exec, program }
    }
}

impl Aws for ExecAws<'_> {
    fn run(&self, args: &[String]) -> Result<Answer, Error> {
        run_program(self.exec, &self.program, args)
    }
}

#[cfg(test)]
mod tests;
