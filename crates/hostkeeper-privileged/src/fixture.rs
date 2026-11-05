//! The workload fixture: `hostkeeper-fixture-hook`, a program built by cargo with this crate's
//! tests (`CARGO_BIN_EXE_hostkeeper-fixture-hook`), that a privileged test runs as a hook through
//! the production launcher, or as the main process of a unit.
//!
//! Its interface is documented here and kept stable, so that tests outside this crate can drive
//! it as the privileged tests and `tests/fixture_hook.rs` do:
//!
//! - `hostkeeper-fixture-hook serve`: the main process of a unit (`Type=exec`). It does
//!   nothing and ends only by a signal (`SIGTERM` from systemd's stop). `/proc/<pid>/exe` of it is
//!   the fixture binary.
//! - `hostkeeper-fixture-hook <step> [<artifact>] --params <file>`: one step of the hook contract
//!   of `hostkeeper-hook`, as the launcher runs it (the arguments [`Invocation::parse`] reads).
//!   The params file holds the fixture's own keys besides the updater's:
//!   - [`MODE_KEY`] (`"fixture.mode"`, required): what the step does, one of [`Mode`]:
//!     - `ok`: exits `0` and prints a valid last line ([`ok_report`]);
//!     - `sleep`: runs until a signal ends it: the unit's bound, a stop, the dead-man. With
//!       [`PID_FILE_KEY`] it first starts one child (`serve`, in the same control group) and
//!       writes its own pid and the child's to that file, so a test knows the step is running
//!       and can check that the whole group is gone after;
//!     - `fail-at=<step>`: exits `1` with the last line of a failure ([`failed_report`],
//!       reason [`FAILED_REASON`]) at that step, and is `ok` at every other step;
//!     - `print-pid`: writes its pid and the SHA-256 of its own executable to the file
//!       [`PID_FILE_KEY`] names (required), then is `ok`;
//!     - `flood=<bytes>`: writes that many bytes of filler lines to standard output, then is
//!       `ok`: what the launcher keeps of a long output.
//!   - [`PID_FILE_KEY`] (`"fixture.pid_file"`, optional): an absolute path, written whole or not
//!     at all ([`write_pid_file`]; read back with [`read_pid_file`]).
//!
//! Every step writes one line to standard error, the hook's log. The fixture reads no
//! environment variable and opens no socket. A refused command line or params file ends the
//! program with exit `1` and the reason on standard error.
//!
//! The decisions are here, tested without root, and so is the start of `sleep`'s child
//! ([`start_child`]); the program (`tests/bin/fixture_hook.rs`) only forwards to them and blocks.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use hostkeeper_hook::{ArgsError, Invocation, Outcome, Params, ParamsError, Report, Scalar, Step};
use sha2::{Digest, Sha256};

/// The first argument that makes the fixture a unit's main process.
pub const SERVE: &str = "serve";
/// The params key of the mode.
pub const MODE_KEY: &str = "fixture.mode";
/// The params key of the pid file.
pub const PID_FILE_KEY: &str = "fixture.pid_file";
/// The reason of the last line of a step that `fail-at` fails.
pub const FAILED_REASON: &str = "fixture_failed";
/// One filler line of `flood`: 64 bytes, the newline included.
pub const FLOOD_LINE: &[u8; 64] =
    b"fixture flood: filler that only fills the output of the step...\n";

/// What one step of the fixture does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Ok,
    Sleep,
    FailAt(Step),
    PrintPid,
    Flood(u64),
}

impl Mode {
    /// Reads the value of [`MODE_KEY`].
    pub fn parse(text: &str) -> Result<Mode, FixtureError> {
        let unknown = || FixtureError::UnknownMode(text.to_owned());
        match text {
            "ok" => Ok(Mode::Ok),
            "sleep" => Ok(Mode::Sleep),
            "print-pid" => Ok(Mode::PrintPid),
            _ => match text.split_once('=') {
                Some(("fail-at", step)) => step.parse().map(Mode::FailAt).map_err(|_| unknown()),
                Some(("flood", bytes)) => bytes.parse().map(Mode::Flood).map_err(|_| unknown()),
                _ => Err(unknown()),
            },
        }
    }
}

impl fmt::Display for Mode {
    /// The value of [`MODE_KEY`] that [`Mode::parse`] reads back as this mode.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Ok => f.write_str("ok"),
            Mode::Sleep => f.write_str("sleep"),
            Mode::FailAt(step) => write!(f, "fail-at={step}"),
            Mode::PrintPid => f.write_str("print-pid"),
            Mode::Flood(bytes) => write!(f, "flood={bytes}"),
        }
    }
}

/// Why the fixture refused to run, or could not.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("unknown fixture mode {0:?} (ok, sleep, fail-at=<step>, print-pid, flood=<bytes>)")]
    UnknownMode(String),
    #[error("serve takes no argument, not {0:?}")]
    ServeArgument(OsString),
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error("the step was given no --params file")]
    NoParams,
    #[error(transparent)]
    Params(#[from] ParamsError),
    #[error("the params file has no {MODE_KEY}")]
    NoMode,
    #[error("{0} must be text")]
    NotText(&'static str),
    #[error("{PID_FILE_KEY} must be an absolute path, not {0:?}")]
    RelativePidFile(String),
    #[error("print-pid needs {PID_FILE_KEY}")]
    NoPidFile,
    #[error("the pid file holds a line that is not `pid <n>` or `exe-sha256 <hex>`: {0:?}")]
    PidLine(String),
    #[error("the pid file holds no pid")]
    NoPid,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One step, as the fixture was asked to run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRun {
    pub invocation: Invocation,
    pub mode: Mode,
    pub pid_file: Option<PathBuf>,
}

/// What the fixture was asked to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// A unit's main process.
    Serve,
    /// One step of the hook contract.
    Step(StepRun),
}

/// A text value of `params`, if the key is there.
fn text<'a>(params: &'a Params, key: &'static str) -> Result<Option<&'a str>, FixtureError> {
    match params.get(key) {
        None => Ok(None),
        Some(Scalar::Text(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(FixtureError::NotText(key)),
    }
}

/// Reads the fixture's command line, without the program name; for a step, reads its params file.
pub fn parse_args<I>(args: I) -> Result<Run, FixtureError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    if args.next_if(|first| first == SERVE).is_some() {
        return match args.next() {
            None => Ok(Run::Serve),
            Some(extra) => Err(FixtureError::ServeArgument(extra)),
        };
    }
    let invocation = Invocation::parse(args)?;
    let file = invocation.params.as_deref().ok_or(FixtureError::NoParams)?;
    let params = Params::read(file)?;
    let mode = Mode::parse(text(&params, MODE_KEY)?.ok_or(FixtureError::NoMode)?)?;
    let pid_file = match text(&params, PID_FILE_KEY)? {
        None => None,
        Some(path) if Path::new(path).is_absolute() => Some(PathBuf::from(path)),
        Some(path) => return Err(FixtureError::RelativePidFile(path.to_owned())),
    };
    Ok(Run::Step(StepRun {
        invocation,
        mode,
        pid_file,
    }))
}

/// The process the fixture runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Me {
    pub pid: u32,
    /// Its executable (`std::env::current_exe`, that is `/proc/self/exe`).
    pub exe: PathBuf,
}

/// How a step of the fixture ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Exits with the outcome's code.
    Exit(Outcome),
    /// Runs until a signal ends it; with a pid file, after it started its child and wrote both
    /// pids there.
    Block { pid_file: Option<PathBuf> },
}

/// The last line of a step that ends `ok`.
pub fn ok_report(step: Step) -> Report {
    Report {
        detail: Some(format!("fixture {step} ok")),
        ..Report::default()
    }
}

/// The last line of the step `fail-at` names.
pub fn failed_report(step: Step) -> Report {
    Report {
        reason: Some(FAILED_REASON.to_owned()),
        detail: Some(format!("fixture failed {step}")),
        ..Report::default()
    }
}

/// Writes `bytes` bytes of [`FLOOD_LINE`]s to `out`; the last one cut at its start, so that the
/// output still ends with a newline.
fn flood(out: &mut dyn Write, bytes: u64) -> io::Result<()> {
    let line = FLOOD_LINE.len() as u64;
    let mut left = bytes;
    while left > 0 {
        let n = left.min(line);
        out.write_all(&FLOOD_LINE[(line - n) as usize..])?;
        left -= n;
    }
    Ok(())
}

/// Runs one step: writes its log line to `err` and its output to `out`, and says how it ends.
pub fn run_step(
    run: &StepRun,
    me: &Me,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Ending, FixtureError> {
    let step = run.invocation.step;
    match &run.mode {
        Mode::Sleep => {
            writeln!(err, "fixture: {step} runs until it is stopped")?;
            return Ok(Ending::Block {
                pid_file: run.pid_file.clone(),
            });
        }
        Mode::FailAt(at) if *at == step => {
            writeln!(err, "fixture: {step} fails")?;
            failed_report(step).write_to(out)?;
            return Ok(Ending::Exit(Outcome::Failure));
        }
        Mode::PrintPid => {
            let file = run.pid_file.as_deref().ok_or(FixtureError::NoPidFile)?;
            write_pid_file(file, &[me.pid], Some(&file_sha256(&me.exe)?))?;
        }
        Mode::Flood(bytes) => flood(out, *bytes)?,
        Mode::Ok | Mode::FailAt(_) => {}
    }
    writeln!(err, "fixture: {step} ok")?;
    ok_report(step).write_to(out)?;
    Ok(Ending::Exit(Outcome::Success))
}

/// Starts the child of a `sleep` step: the fixture's own `serve`, with no input and no output (so
/// that the step's output ends when the step and its child have ended), in the step's control
/// group; then writes the step's pid and the child's to `pid_file`.
pub fn start_child(me: &Me, pid_file: &Path) -> Result<Child, FixtureError> {
    let child = Command::new(&me.exe)
        .arg(SERVE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    write_pid_file(pid_file, &[me.pid, child.id()], None)?;
    Ok(child)
}

/// The SHA-256 of the file at `path`, in lowercase hex.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}

/// What a pid file holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PidFile {
    /// The pids, the step's own first.
    pub pids: Vec<u32>,
    /// The SHA-256 of the step's executable, in lowercase hex (`print-pid`).
    pub exe_sha256: Option<String>,
}

/// The text of a pid file: a line `pid <n>` for each pid, then `exe-sha256 <hex>` if given.
pub fn pid_file_text(pids: &[u32], exe_sha256: Option<&str>) -> String {
    let mut lines: Vec<String> = pids.iter().map(|pid| format!("pid {pid}\n")).collect();
    lines.extend(exe_sha256.map(|digest| format!("exe-sha256 {digest}\n")));
    lines.concat()
}

/// Writes the pid file at `path` whole: to `<path>-part` first, renamed over `path` after, so a
/// reader never sees part of it.
pub fn write_pid_file(path: &Path, pids: &[u32], exe_sha256: Option<&str>) -> io::Result<()> {
    let mut part = path.as_os_str().to_owned();
    part.push("-part");
    fs::write(&part, pid_file_text(pids, exe_sha256))?;
    fs::rename(&part, path)
}

/// Reads the text of a pid file.
pub fn parse_pid_file(text: &str) -> Result<PidFile, FixtureError> {
    let mut found = PidFile {
        pids: Vec::new(),
        exe_sha256: None,
    };
    for line in text.lines() {
        let bad = || FixtureError::PidLine(line.to_owned());
        match line.split_once(' ') {
            Some(("pid", pid)) => found.pids.push(pid.parse().map_err(|_| bad())?),
            Some(("exe-sha256", digest)) => found.exe_sha256 = Some(digest.to_owned()),
            _ => return Err(bad()),
        }
    }
    if found.pids.is_empty() {
        return Err(FixtureError::NoPid);
    }
    Ok(found)
}

/// Reads the pid file at `path`.
pub fn read_pid_file(path: &Path) -> Result<PidFile, FixtureError> {
    parse_pid_file(&fs::read_to_string(path)?)
}
