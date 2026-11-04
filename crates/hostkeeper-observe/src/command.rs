//! The real [`CommandRunner`]: a child process with its standard output read to a cap, ended at
//! its deadline.
//!
//! The child's standard input is a pipe written nothing and held open until the child's output
//! has ended or the deadline has passed; its standard error is discarded. Standard output is read
//! on a thread of its own, to at most the cap plus one byte (the extra byte only tells that there
//! was more); the pipe is then closed, so a child that keeps printing is stopped by the closed
//! pipe rather than read without end. If the reading has not ended by the deadline the child is killed and reaped, and the run
//! is [`RunError::TimedOut`]. The deadline bounds the wait for the child's output to end, which
//! for `systemctl show` is its exit.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::error::RunError;
use crate::ports::{CommandOutput, CommandRunner};

/// The standard output kept unless told otherwise: far above what `systemctl show` prints for
/// four properties.
pub const MAX_STDOUT: usize = 64 * 1024;

/// Runs programs as child processes.
#[derive(Clone, Copy, Debug)]
pub struct ProcessRunner {
    max_stdout: usize,
}

impl ProcessRunner {
    /// A runner that keeps at most `max_stdout` bytes of a child's standard output.
    pub fn new(max_stdout: usize) -> ProcessRunner {
        ProcessRunner { max_stdout }
    }
}

/// The error of `program` for an I/O failure.
fn because(program: &str) -> impl Fn(io::Error) -> RunError + '_ {
    move |e| RunError::Failed {
        program: program.to_owned(),
        why: e.to_string(),
    }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, argv: &[String], deadline: Duration) -> Result<CommandOutput, RunError> {
        let Some((program, args)) = argv.split_first() else {
            return Err(RunError::Empty);
        };
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(because(program))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let cap = self.max_stdout;
        let (sent, read) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.map_or(Ok(0), |out| {
                out.take((cap as u64).saturating_add(1))
                    .read_to_end(&mut bytes)
            });
            // The receiver is gone when the deadline passed first: nothing waits for this.
            let _ = sent.send(result.map(|_| bytes));
        });
        let Ok(result) = read.recv_timeout(deadline) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::TimedOut {
                program: program.clone(),
            });
        };
        drop(stdin);
        child
            .wait()
            .and_then(|status| result.map(|stdout| (status, stdout)))
            .map(|(status, mut stdout)| {
                let truncated = stdout.len() > cap;
                stdout.truncate(cap);
                CommandOutput {
                    exit_code: status.code(),
                    stdout,
                    truncated,
                }
            })
            .map_err(because(program))
    }
}
