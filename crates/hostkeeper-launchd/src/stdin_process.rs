//! The runner of a program whose standard input is a pipe: the [`StdinRun`] port (one run of a
//! program with bytes on its standard input), its real adapter [`StdinProcess`], and [`run_ok`].
//!
//! [`LaunchctlProcess`](crate::LaunchctlProcess) gives its child `/dev/null` as its standard input.
//! A program that reads a password there (`softwareupdate --stdinpass`, the maintenance-updates
//! design, section 5.5, proposed in komira-ai/hostkeeper#447) is run by this one instead: the bytes
//! it is given are written to the child's standard input, which is then closed. They are never in
//! the child's arguments or its environment, which is empty, and no error of this module holds
//! them, the arguments or what the child wrote: an error names the step its caller gave and, for a
//! run that ended, the exit code alone (the pattern of the daemon's `run_ok_named`).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::launchctl::{LaunchctlOutput, collect};

/// Why a run gave no output, or, from [`run_ok`], did not succeed. Each value names the step and
/// nothing the step was given or wrote.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StdinError {
    /// The program could not be started.
    #[error("{step}: the program could not be started: {why}")]
    Spawn {
        /// The step the caller named.
        step: String,
        /// The system's reason.
        why: String,
    },
    /// The program's output had not ended by its deadline; it was killed.
    #[error("{step}: the program did not finish within its deadline and was killed")]
    TimedOut {
        /// The step the caller named.
        step: String,
    },
    /// The program exited with a code other than 0.
    #[error("{step}: the program exited with code {code}")]
    Exit {
        /// The step the caller named.
        step: String,
        /// The exit code.
        code: i32,
    },
    /// The program ended with no exit code: a signal ended it, or its end could not be read.
    #[error("{step}: the program ended without an exit code")]
    NoExitCode {
        /// The step the caller named.
        step: String,
    },
}

/// One run of a program with bytes on its standard input.
pub trait StdinRun: Send + Sync {
    /// Runs the program with `args`, writes `input` to its standard input and closes it, and
    /// returns how the program ended. `step` names the run in an error.
    ///
    /// # Errors
    /// [`StdinError::Spawn`] and [`StdinError::TimedOut`]; a run that ended is `Ok`, whatever
    /// its exit code.
    fn run(&self, step: &str, args: &[String], input: &[u8])
    -> Result<LaunchctlOutput, StdinError>;
}

/// [`StdinRun::run`], with an exit code other than 0 an error: [`StdinError::Exit`] with the
/// code, or [`StdinError::NoExitCode`]. The standard error of a failed run is not in the error.
///
/// # Errors
/// Those of [`StdinRun::run`], and those two.
pub fn run_ok(
    runner: &dyn StdinRun,
    step: &str,
    args: &[String],
    input: &[u8],
) -> Result<LaunchctlOutput, StdinError> {
    let output = runner.run(step, args, input)?;
    match output.code {
        Some(0) => Ok(output),
        Some(code) => Err(StdinError::Exit {
            step: step.to_owned(),
            code,
        }),
        None => Err(StdinError::NoExitCode {
            step: step.to_owned(),
        }),
    }
}

/// Runs a program as a child process with an empty environment, its standard input a pipe that
/// is given the run's input and then closed, and its standard output and standard error each
/// kept to [`OUTPUT_MAX`](crate::launchctl::OUTPUT_MAX) bytes. The input is written on a thread
/// of its own, so a child that does not read it holds nothing but that thread, which ends when
/// the child does. The deadline bounds the wait for both output streams to end, as for
/// [`LaunchctlProcess`](crate::LaunchctlProcess): a child whose output has not ended by then is
/// killed and reaped; once both have ended the child is waited for without a bound.
#[derive(Clone, Debug)]
pub struct StdinProcess {
    program: PathBuf,
    deadline: Duration,
}

impl StdinProcess {
    /// Runs `program`, each run's output waited for at most `deadline`.
    pub fn new(program: &Path, deadline: Duration) -> StdinProcess {
        StdinProcess {
            program: program.to_owned(),
            deadline,
        }
    }

    /// The program that is run.
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// Writes `input` to `pipe` on a thread of its own, then drops the pipe, which closes it. A
/// child that ends, or closes its standard input, before it has read all of it makes the write
/// fail; how the child ended says what it did, so the failure is not kept.
fn give_input(pipe: Option<ChildStdin>, input: Vec<u8>) {
    thread::spawn(move || {
        let _ = pipe.map(|mut pipe| pipe.write_all(&input));
    });
}

impl StdinRun for StdinProcess {
    fn run(
        &self,
        step: &str,
        args: &[String],
        input: &[u8],
    ) -> Result<LaunchctlOutput, StdinError> {
        let mut child = Command::new(&self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e: io::Error| StdinError::Spawn {
                step: step.to_owned(),
                why: e.to_string(),
            })?;
        give_input(child.stdin.take(), input.to_vec());
        collect(&mut child, self.deadline).ok_or_else(|| StdinError::TimedOut {
            step: step.to_owned(),
        })
    }
}
