//! The coordinator's side of the `ipmitool` port ([`hostkeeper_console::ipmi::Tool`]): the tool
//! run as a child process on pipes (`docs/design/console-broker.md`, decision D6 and section
//! 17.3), its environment emptied but for the password, its standard output read up to each
//! prompt, its standard error kept only to name a failure.
//!
//! Every wait for a prompt has a deadline, past which the child is killed and the call is
//! answered as a device that did not answer. A session ends when it is dropped: its standard input
//! is closed after `exit`, which ends the tool and its session with the BMC, and a child still
//! running at the deadline after that is killed.
//!
//! An activation (`sol activate`, [`Tool::activate`]) is a run of its own with no prompt and no
//! deadline, handed over at once as its standard input and output; its end, or the drop of its
//! end, kills the child at once (an `exit` would be typed at the host) and answers its standard
//! error. The coordinator then deactivates the payload in a session of its own.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hostkeeper_console::ipmi::{
    Activated, Invocation, PASSWORD_VARIABLE, PROMPT, Settings, Shell, ShellError, Tool, ToolText,
};

/// The most bytes of the tool's standard error kept.
const MAX_ERROR_BYTES: u64 = 64 * 1024;

/// The real tool: `program`, each wait for its prompt bounded by `deadline`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipmitool {
    program: PathBuf,
    deadline: Duration,
}

impl Ipmitool {
    /// The tool at `program`, waited for as [`deadline`] says of `settings`.
    #[must_use]
    pub fn new(program: &Path, settings: &Settings) -> Self {
        Self {
            program: program.to_path_buf(),
            deadline: deadline(settings),
        }
    }

    /// The command of `invocation`: the program with its escape byte (`-e`, one raw byte) when it
    /// has one, then its arguments, its environment holding the password alone, and its three
    /// streams on pipes.
    pub(crate) fn command(&self, invocation: &Invocation<'_>) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(escape) = invocation.escape() {
            command.arg("-e").arg(OsStr::from_bytes(&[escape]));
        }
        command
            .args(invocation.args())
            .env_clear()
            .env(PASSWORD_VARIABLE, invocation.password())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

/// How long one wait for the tool's prompt may take: a session's setup and one command are about
/// a dozen exchanges, each of which the tool sends at most `retries + 1` times and waits
/// `timeout` for (`-N`, `-R`), so eight such waits and ten seconds more bound a tool that still
/// works; the default settings give 34 seconds.
#[must_use]
pub fn deadline(settings: &Settings) -> Duration {
    let tries = u64::from(settings.retries) + 1;
    Duration::from_secs(u64::from(settings.timeout_secs) * tries * 8 + 10)
}

impl Tool for Ipmitool {
    fn open(&self, invocation: &Invocation<'_>) -> Result<Box<dyn Shell>, ShellError> {
        Process::start(self.command(invocation), self.deadline).map(|p| Box::new(p) as _)
    }

    fn activate(&self, invocation: &Invocation<'_>) -> Result<Activated, ShellError> {
        activated(self.command(invocation))
    }
}

/// Starts `command` and hands it over as its streams, as the module says.
///
/// # Errors
/// [`ShellError::Start`]: it could not be started.
pub(crate) fn activated(mut command: Command) -> Result<Activated, ShellError> {
    let mut child = command.spawn().map_err(|_| ShellError::Start)?;
    let (Some(input), Some(output), stderr) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ShellError::Start);
    };
    let mut running = Running {
        child,
        errors: Some(errors_of(stderr)),
    };
    Ok(Activated {
        input: Box::new(input),
        output: Box::new(output),
        end: Box::new(move || running.stop()),
    })
}

/// A child handed over as streams: killed when it is stopped or dropped.
struct Running {
    child: Child,
    errors: Option<JoinHandle<String>>,
}

impl Running {
    /// Kills the child, if it still runs, and answers its standard error.
    fn stop(&mut self) -> ToolText {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let text = self
            .errors
            .take()
            .map(JoinHandle::join)
            .and_then(Result::ok);
        ToolText::new(text.unwrap_or_default())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A thread that keeps the first [`MAX_ERROR_BYTES`] of `stderr`, until it ends.
fn errors_of(stderr: Option<std::process::ChildStderr>) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.into_iter().for_each(|stderr| {
            let _ = stderr.take(MAX_ERROR_BYTES).read_to_end(&mut bytes);
        });
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// A running tool.
pub(crate) struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<Vec<u8>>,
    errors: Option<JoinHandle<String>>,
    pending: Vec<u8>,
    deadline: Duration,
}

impl Process {
    /// Starts `command` and waits for its first prompt; what it printed before is dropped.
    ///
    /// # Errors
    /// [`ShellError`]: it could not be started, gave no prompt by `deadline`, or ended first.
    pub(crate) fn start(mut command: Command, deadline: Duration) -> Result<Self, ShellError> {
        let mut child = command.spawn().map_err(|_| ShellError::Start)?;
        let (sender, output) = mpsc::channel();
        let stdout = child.stdout.take();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            stdout.into_iter().for_each(|mut stdout| {
                loop {
                    let read = stdout.read(&mut buffer).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    let _ = sender.send(buffer[..read].to_vec());
                }
            });
        });
        let errors = errors_of(child.stderr.take());
        let mut process = Self {
            stdin: child.stdin.take(),
            child,
            output,
            errors: Some(errors),
            pending: Vec::new(),
            deadline,
        };
        process.until_prompt()?;
        Ok(process)
    }

    /// The same session waiting `deadline` from now on: a test's way to reach the deadline once
    /// the first prompt has come.
    #[cfg(test)]
    pub(crate) fn set_deadline(&mut self, deadline: Duration) {
        self.deadline = deadline;
    }

    /// What the tool printed up to its next prompt, which is dropped.
    fn until_prompt(&mut self) -> Result<String, ShellError> {
        let until = Instant::now() + self.deadline;
        loop {
            if let Some(at) = find(&self.pending, PROMPT.as_bytes()) {
                let answer: Vec<u8> = self.pending.drain(..at + PROMPT.len()).collect();
                return Ok(String::from_utf8_lossy(&answer[..at]).into_owned());
            }
            match self
                .output
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(bytes) => self.pending.extend(bytes),
                Err(RecvTimeoutError::Timeout) => {
                    let _ = self.child.kill();
                    return Err(ShellError::Deadline);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.child.wait();
                    let errors = self.errors.take().map(JoinHandle::join);
                    let text = errors.and_then(Result::ok).unwrap_or_default();
                    return Err(ShellError::Ended(ToolText::new(text)));
                }
            }
        }
    }
}

impl Shell for Process {
    fn ask(&mut self, command: &str) -> Result<String, ShellError> {
        self.stdin.iter_mut().for_each(|stdin| {
            let _ = stdin.write_all(format!("{command}\n").as_bytes());
            let _ = stdin.flush();
        });
        self.until_prompt()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stdin.take().into_iter().for_each(|mut stdin| {
            let _ = stdin.write_all(b"exit\n");
        });
        let until = Instant::now() + self.deadline;
        while self
            .output
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .is_ok()
        {}
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where `needle` first starts in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
#[path = "ipmitool_tests.rs"]
mod tests;
