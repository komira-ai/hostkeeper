//! The `launchctl` port: one `launchctl` command, its real adapter, and the reading of
//! `launchctl print system/<label>`.
//!
//! The real adapter is [`LaunchctlProcess`], the only code of the crate that starts a process.
//! The text `launchctl print` writes is Apple's, not a stable interface; [`parse_print`] reads the
//! few lines of it this daemon needs and ignores the rest.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::label::Label;

/// The program the real adapter runs.
pub const LAUNCHCTL: &str = "/bin/launchctl";
/// The bytes kept of the standard output, and of the standard error, of one command.
pub const OUTPUT_MAX: usize = 64 * 1024;
/// The exit code of `launchctl print` for a label the domain does not hold.
pub const EXIT_NOT_LOADED: i32 = 113;
/// The text `launchctl` writes for a label the domain does not hold.
pub const NOT_LOADED_TEXT: &str = "Could not find service";

/// What one `launchctl` command ended with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchctlOutput {
    /// The exit code; `None` when the command was ended by a signal.
    pub code: Option<i32>,
    /// The standard output, to [`OUTPUT_MAX`] bytes.
    pub stdout: Vec<u8>,
    /// The standard error, to [`OUTPUT_MAX`] bytes.
    pub stderr: Vec<u8>,
}

impl LaunchctlOutput {
    /// Exit code 0 with `stdout`.
    pub fn success(stdout: &str) -> LaunchctlOutput {
        LaunchctlOutput {
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    /// Exit code `code` with `stderr` and no standard output.
    pub fn exit(code: i32, stderr: &str) -> LaunchctlOutput {
        LaunchctlOutput {
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// What `launchctl print` ends with for a label that is not loaded.
    pub fn not_loaded() -> LaunchctlOutput {
        LaunchctlOutput::exit(EXIT_NOT_LOADED, &format!("{NOT_LOADED_TEXT}: system/x"))
    }

    /// The standard error as text, invalid UTF-8 replaced.
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// A `launchctl` command that did not end with an exit code.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchctlError {
    /// The command could not be started.
    #[error("cannot run launchctl {args}: {why}")]
    Spawn {
        /// The arguments, joined by spaces.
        args: String,
        /// The system's reason.
        why: String,
    },
    /// The command's output had not ended by its deadline.
    #[error("launchctl {args} did not finish within its deadline and was killed")]
    TimedOut {
        /// The arguments, joined by spaces.
        args: String,
    },
}

/// One `launchctl` command.
pub trait Launchctl: Send + Sync {
    /// Runs `launchctl` with `args` and returns how it ended.
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError>;
}

/// Runs a `launchctl` program as a child process with an empty environment, no standard input,
/// and its standard output and standard error each kept to [`OUTPUT_MAX`] bytes. The deadline
/// bounds the wait for both output streams to end, which for `launchctl` is its exit: a child
/// whose output has not ended by then is killed and reaped. Once both streams have ended the
/// child is waited for without a bound, as `hostkeeper-observe`'s `ProcessRunner` does, so a
/// program that closes its output and keeps running would hold the call.
#[derive(Clone, Debug)]
pub struct LaunchctlProcess {
    program: PathBuf,
    deadline: Duration,
}

impl LaunchctlProcess {
    /// The system's `launchctl` ([`LAUNCHCTL`]), each command's output waited for at most
    /// `deadline`.
    pub fn system(deadline: Duration) -> LaunchctlProcess {
        LaunchctlProcess::new(Path::new(LAUNCHCTL), deadline)
    }

    /// `program` in the place of `launchctl`, each command's output waited for at most
    /// `deadline`.
    pub fn new(program: &Path, deadline: Duration) -> LaunchctlProcess {
        LaunchctlProcess {
            program: program.to_owned(),
            deadline,
        }
    }

    /// The program that is run.
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// Reads `pipe` to [`OUTPUT_MAX`] bytes on a thread of its own, then drops it and sends the bytes
/// with `which` (0 for standard output, 1 for standard error). What was read before a read
/// error is kept.
fn read_capped<R: Read + Send + 'static>(
    pipe: Option<R>,
    which: usize,
    sent: Sender<(usize, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.map(|pipe| pipe.take(OUTPUT_MAX as u64).read_to_end(&mut bytes));
        // The receiver is gone when the deadline passed first: nothing waits for this.
        let _ = sent.send((which, bytes));
    });
}

impl Launchctl for LaunchctlProcess {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let joined = args.join(" ");
        let mut child = Command::new(&self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e: io::Error| LaunchctlError::Spawn {
                args: joined.clone(),
                why: e.to_string(),
            })?;
        let end = Instant::now() + self.deadline;
        let (sent, read) = mpsc::channel();
        read_capped(child.stdout.take(), 0, sent.clone());
        read_capped(child.stderr.take(), 1, sent);
        let mut streams: [Vec<u8>; 2] = Default::default();
        for _ in 0..2 {
            let left = end.saturating_duration_since(Instant::now());
            match read.recv_timeout(left) {
                Ok((which, bytes)) => streams[which] = bytes,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(LaunchctlError::TimedOut { args: joined });
                }
            }
        }
        let [stdout, stderr] = streams;
        // A wait that fails leaves the exit code unknown, as a signal does.
        let code = child.wait().ok().and_then(|status| status.code());
        Ok(LaunchctlOutput {
            code,
            stdout,
            stderr,
        })
    }
}

/// The state `launchctl print` gives a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// `running`: the job has a process.
    Running,
    /// `waiting`: loaded, with no process, waiting for a trigger.
    Waiting,
    /// `spawn scheduled`: launchd is about to start the job.
    SpawnScheduled,
    /// `not running` or `exited`: loaded, with no process.
    NotRunning,
    /// Any other word, as printed.
    Other(String),
}

impl JobState {
    fn from_word(word: &str) -> JobState {
        match word {
            "running" => JobState::Running,
            "waiting" => JobState::Waiting,
            "spawn scheduled" => JobState::SpawnScheduled,
            "not running" | "exited" => JobState::NotRunning,
            other => JobState::Other(other.to_owned()),
        }
    }

    /// The word launchd prints for this state.
    pub fn word(&self) -> &str {
        match self {
            JobState::Running => "running",
            JobState::Waiting => "waiting",
            JobState::SpawnScheduled => "spawn scheduled",
            JobState::NotRunning => "not running",
            JobState::Other(word) => word,
        }
    }
}

/// What `launchctl print` says of a loaded job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    /// The job's state.
    pub state: JobState,
    /// The process id, while the job has a process.
    pub pid: Option<u32>,
    /// The exit code of the last run; `None` when it never exited or a signal ended it.
    pub last_exit_status: Option<i32>,
    /// The signal that ended the last run, when one did.
    pub last_terminating_signal: Option<i32>,
}

/// Why `launchctl print` output could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// There was no text.
    #[error("launchctl print wrote nothing")]
    Empty,
    /// The first line is not `<domain>/<label> = {`.
    #[error("launchctl print did not start with a job header")]
    NoHeader,
    /// The text ended before the job's closing brace.
    #[error("launchctl print ended before the job's closing brace")]
    Truncated,
    /// The job has no `state` line.
    #[error("launchctl print has no state line")]
    NoState,
    /// A line this daemon reads has a value it cannot read.
    #[error("launchctl print has {value:?} for {key:?}, which is not a number")]
    BadValue {
        /// The line's key.
        key: String,
        /// The line's value.
        value: String,
    },
    /// A line this daemon reads is there more than once.
    #[error("launchctl print has {key:?} more than once")]
    Duplicate {
        /// The line's key.
        key: String,
    },
}

/// The one line of `found` whose key is among `keys`, as (key, value).
fn once<'a>(
    found: &[(&'a str, &'a str)],
    keys: &[&str],
) -> Result<Option<(&'a str, &'a str)>, ParseError> {
    let hits: Vec<(&str, &str)> = found
        .iter()
        .filter(|(key, _)| keys.contains(key))
        .copied()
        .collect();
    match hits.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        [_, (key, _), ..] => Err(ParseError::Duplicate {
            key: (*key).to_owned(),
        }),
    }
}

/// The integer in `digits` read for the line `key = value`.
fn number<T: std::str::FromStr>(key: &str, value: &str, digits: &str) -> Result<T, ParseError> {
    digits.trim().parse().map_err(|_| ParseError::BadValue {
        key: key.to_owned(),
        value: value.to_owned(),
    })
}

/// Reads the output of `launchctl print system/<label>`: the job's state and, when printed, its
/// process id, last exit code and last terminating signal. Only the lines directly inside the
/// job's braces are read; nested blocks are skipped whole, and lines this function does not know
/// are ignored.
pub fn parse_print(stdout: &str) -> Result<JobInfo, ParseError> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let header = lines.next().ok_or(ParseError::Empty)?;
    if !header.ends_with(" = {") {
        return Err(ParseError::NoHeader);
    }
    let mut depth = 1_usize;
    let mut found = Vec::new();
    for line in lines {
        let inside = depth == 1;
        if line == "}" {
            depth -= 1;
        } else if line.ends_with('{') {
            depth += 1;
        } else if let Some((key, value)) = line.split_once(" = ").filter(|_| inside) {
            found.push((key.trim(), value.trim()));
        }
        if depth == 0 {
            break;
        }
    }
    if depth != 0 {
        return Err(ParseError::Truncated);
    }
    let (_, state) = once(&found, &["state"])?.ok_or(ParseError::NoState)?;
    let pid: Option<u32> = once(&found, &["pid"])?
        .map(|(key, value)| number(key, value, value))
        .transpose()?;
    let last_exit_status: Option<i32> = once(&found, &["last exit code", "last exit status"])?
        .filter(|(_, value)| *value != "(never exited)")
        .map(|(key, value)| number(key, value, value.split_once(':').map_or(value, |(n, _)| n)))
        .transpose()?;
    let last_terminating_signal: Option<i32> = once(&found, &["last terminating signal"])?
        .map(|(key, value)| number(key, value, value.rsplit_once(':').map_or(value, |(_, n)| n)))
        .transpose()?;
    Ok(JobInfo {
        state: JobState::from_word(state),
        pid,
        last_exit_status,
        last_terminating_signal,
    })
}

/// Whether a label is loaded in the system domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStatus {
    /// The domain does not hold the label.
    NotLoaded,
    /// The domain holds the label, as printed.
    Loaded(JobInfo),
}

/// Why `launchctl print` gave no answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrintError {
    /// The command did not run to an exit.
    #[error("{0}")]
    Launchctl(#[from] LaunchctlError),
    /// The command exited with a code that is neither success nor not-loaded.
    #[error("launchctl print exited with {code:?}: {stderr}")]
    Exit {
        /// The exit code; `None` for a signal.
        code: Option<i32>,
        /// The standard error, as text.
        stderr: String,
    },
    /// The standard output is not UTF-8.
    #[error("launchctl print wrote output that is not UTF-8")]
    NotUtf8,
    /// The output could not be read.
    #[error("{0}")]
    Parse(#[from] ParseError),
}

/// The arguments of `launchctl print system/<label>`.
pub fn print_args(label: &Label) -> Vec<String> {
    vec!["print".to_owned(), format!("system/{label}")]
}

/// The arguments of `launchctl bootstrap system <plist>`.
pub fn bootstrap_args(plist: &Path) -> Vec<String> {
    vec![
        "bootstrap".to_owned(),
        "system".to_owned(),
        plist.to_string_lossy().into_owned(),
    ]
}

/// The arguments of `launchctl bootout system/<label>`.
pub fn bootout_args(label: &Label) -> Vec<String> {
    vec!["bootout".to_owned(), format!("system/{label}")]
}

/// The arguments of `launchctl kickstart system/<label>`.
pub fn kickstart_args(label: &Label) -> Vec<String> {
    vec!["kickstart".to_owned(), format!("system/{label}")]
}

/// Runs `launchctl print system/<label>`. Exit code [`EXIT_NOT_LOADED`], or [`NOT_LOADED_TEXT`]
/// in the standard output or standard error, is [`JobStatus::NotLoaded`]; exit code 0 is parsed;
/// any other end is [`PrintError::Exit`].
pub fn print(launchctl: &impl Launchctl, label: &Label) -> Result<JobStatus, PrintError> {
    let out = launchctl.run(&print_args(label))?;
    let says = |bytes: &[u8]| String::from_utf8_lossy(bytes).contains(NOT_LOADED_TEXT);
    if (out.code == Some(EXIT_NOT_LOADED)) | says(&out.stdout) | says(&out.stderr) {
        return Ok(JobStatus::NotLoaded);
    }
    if out.code != Some(0) {
        return Err(PrintError::Exit {
            code: out.code,
            stderr: out.stderr_text(),
        });
    }
    let text = String::from_utf8(out.stdout).map_err(|_| PrintError::NotUtf8)?;
    Ok(JobStatus::Loaded(parse_print(&text)?))
}

/// Why a label could not be stopped.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StopError {
    /// Asking whether the label is loaded failed.
    #[error("{0}")]
    Print(#[from] PrintError),
    /// `launchctl bootout` failed and the label is still loaded.
    #[error("launchctl bootout exited with {code:?}: {stderr}")]
    Bootout {
        /// The exit code; `None` for a signal.
        code: Option<i32>,
        /// The standard error, as text.
        stderr: String,
    },
    /// `launchctl bootout` did not run to an exit.
    #[error("{0}")]
    Launchctl(LaunchctlError),
}

/// Stops the job `label`: prints it, and a label that is not loaded is done. Otherwise runs
/// `launchctl bootout`; exit code 0 is done. A bootout that fails is followed by one more print:
/// a label no longer loaded is done, one still loaded is [`StopError::Bootout`].
pub fn bootout(launchctl: &impl Launchctl, label: &Label) -> Result<(), StopError> {
    if print(launchctl, label)? == JobStatus::NotLoaded {
        return Ok(());
    }
    let out = launchctl
        .run(&bootout_args(label))
        .map_err(StopError::Launchctl)?;
    if out.code == Some(0) {
        return Ok(());
    }
    match print(launchctl, label)? {
        JobStatus::NotLoaded => Ok(()),
        JobStatus::Loaded(_) => Err(StopError::Bootout {
            code: out.code,
            stderr: out.stderr_text(),
        }),
    }
}
