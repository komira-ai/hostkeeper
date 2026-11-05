//! The XML property lists of the three kinds of launchd job: the daemon's own job, a step job and
//! a service package's job.
//!
//! The writer is small and pure. It knows the four value kinds these files use (string,
//! boolean, array, dictionary), writes Apple's XML layout with one tab per level, and refuses any
//! text that holds a control character or is not UTF-8, so a path can never inject markup or
//! lines into a plist that runs as root.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::label::Label;

/// The installed daemon binary the daemon job runs.
pub const DAEMON_PROGRAM: &str = "/usr/local/bin/hostkeeper";
/// The directory the daemon job's output files are in.
pub const DAEMON_LOG_DIR: &str = "/var/log/hostkeeper";

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n";

/// The daemon's launchd job: runs as root, starts at load, and is kept alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonJob {
    /// The job's label.
    pub label: Label,
    /// The daemon binary; the job runs it with the argument `daemon`.
    pub program: PathBuf,
    /// Where launchd sends the job's standard output.
    pub stdout: PathBuf,
    /// Where launchd sends the job's standard error.
    pub stderr: PathBuf,
}

impl DaemonJob {
    /// The job of an installed host: the daemon label, [`DAEMON_PROGRAM`], and two files in
    /// [`DAEMON_LOG_DIR`].
    pub fn standard() -> DaemonJob {
        let logs = Path::new(DAEMON_LOG_DIR);
        DaemonJob {
            label: Label::daemon(),
            program: PathBuf::from(DAEMON_PROGRAM),
            stdout: logs.join("daemon.out.log"),
            stderr: logs.join("daemon.err.log"),
        }
    }
}

/// A step's launchd job: runs once at load, is not restarted, and its process group is not
/// abandoned when the job ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepJob {
    /// The job's label.
    pub label: Label,
    /// The program to run.
    pub program: PathBuf,
    /// The program's arguments.
    pub args: Vec<OsString>,
    /// Where launchd sends the job's standard output.
    pub stdout: PathBuf,
    /// Where launchd sends the job's standard error.
    pub stderr: PathBuf,
}

/// A service package's launchd job: a long-running program that launchd keeps alive while the
/// file `run_while` exists, and that does not start at load otherwise (`RunAtLoad` is false).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceJob {
    /// The job's label.
    pub label: Label,
    /// The program to run.
    pub program: PathBuf,
    /// The program's arguments.
    pub args: Vec<OsString>,
    /// The path whose existence keeps the job alive (`KeepAlive` / `PathState`).
    pub run_while: PathBuf,
    /// Where launchd sends the job's standard output.
    pub stdout: PathBuf,
    /// Where launchd sends the job's standard error.
    pub stderr: PathBuf,
}

/// Why a plist was not written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlistError {
    /// A string holds a control character.
    #[error("plist text {value:?} holds a control character")]
    ControlCharacter {
        /// The text, as given.
        value: String,
    },
    /// A path or argument is not UTF-8.
    #[error("plist text {value:?} is not valid UTF-8")]
    NotUtf8 {
        /// The text, with each invalid byte shown as U+FFFD.
        value: String,
    },
}

/// The daemon job's plist: `Label`, `ProgramArguments`, `RunAtLoad` and `KeepAlive` true, and
/// the two output paths.
pub fn daemon_plist(job: &DaemonJob) -> Result<String, PlistError> {
    let argv = vec![utf8(job.program.as_os_str())?, "daemon".to_owned()];
    job_plist(
        &job.label,
        argv,
        [
            ("RunAtLoad", Value::Bool(true)),
            ("KeepAlive", Value::Bool(true)),
        ],
        &job.stdout,
        &job.stderr,
    )
}

/// A step job's plist: `Label`, `ProgramArguments`, `RunAtLoad` true, `AbandonProcessGroup`
/// false, and the two output paths. It has no `KeepAlive`.
pub fn step_plist(job: &StepJob) -> Result<String, PlistError> {
    job_plist(
        &job.label,
        argv(&job.program, &job.args)?,
        [
            ("RunAtLoad", Value::Bool(true)),
            ("AbandonProcessGroup", Value::Bool(false)),
        ],
        &job.stdout,
        &job.stderr,
    )
}

/// A service job's plist: `Label`, `ProgramArguments`, `RunAtLoad` false, `KeepAlive` as a
/// dictionary whose `PathState` holds `run_while` true, and the two output paths.
pub fn service_plist(job: &ServiceJob) -> Result<String, PlistError> {
    let keep_alive = Value::Dict(vec![(
        "PathState".to_owned(),
        Value::Dict(vec![(utf8(job.run_while.as_os_str())?, Value::Bool(true))]),
    )]);
    job_plist(
        &job.label,
        argv(&job.program, &job.args)?,
        [("RunAtLoad", Value::Bool(false)), ("KeepAlive", keep_alive)],
        &job.stdout,
        &job.stderr,
    )
}

/// One value of a property list.
enum Value {
    Str(String),
    Bool(bool),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

/// The program followed by its arguments, each as UTF-8 text.
fn argv(program: &Path, args: &[OsString]) -> Result<Vec<String>, PlistError> {
    std::iter::once(program.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(utf8)
        .collect()
}

/// The plist of one job: the label, the arguments, the job's own keys (`own`, in order), and the
/// two output paths.
fn job_plist(
    label: &Label,
    argv: Vec<String>,
    own: [(&'static str, Value); 2],
    stdout: &Path,
    stderr: &Path,
) -> Result<String, PlistError> {
    let mut entries = vec![
        ("Label".to_owned(), Value::Str(label.as_str().to_owned())),
        (
            "ProgramArguments".to_owned(),
            Value::Array(argv.into_iter().map(Value::Str).collect()),
        ),
    ];
    entries.extend(own.map(|(key, value)| (key.to_owned(), value)));
    entries.push((
        "StandardOutPath".to_owned(),
        Value::Str(utf8(stdout.as_os_str())?),
    ));
    entries.push((
        "StandardErrorPath".to_owned(),
        Value::Str(utf8(stderr.as_os_str())?),
    ));
    let mut out = String::from(HEADER);
    render(&Value::Dict(entries), 0, &mut out)?;
    out.push_str("</plist>\n");
    Ok(out)
}

fn utf8(text: &OsStr) -> Result<String, PlistError> {
    text.to_str()
        .map(str::to_owned)
        .ok_or_else(|| PlistError::NotUtf8 {
            value: text.to_string_lossy().into_owned(),
        })
}

fn render(value: &Value, depth: usize, out: &mut String) -> Result<(), PlistError> {
    let indent = "\t".repeat(depth);
    match value {
        Value::Str(text) => out.push_str(&format!("{indent}<string>{}</string>\n", escape(text)?)),
        Value::Bool(flag) => out.push_str(&format!("{indent}<{flag}/>\n")),
        Value::Array(items) => {
            out.push_str(&format!("{indent}<array>\n"));
            for item in items {
                render(item, depth + 1, out)?;
            }
            out.push_str(&format!("{indent}</array>\n"));
        }
        Value::Dict(entries) => {
            out.push_str(&format!("{indent}<dict>\n"));
            for (key, entry) in entries {
                out.push_str(&format!("{indent}\t<key>{}</key>\n", escape(key)?));
                render(entry, depth + 1, out)?;
            }
            out.push_str(&format!("{indent}</dict>\n"));
        }
    }
    Ok(())
}

fn escape(text: &str) -> Result<String, PlistError> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => {
                return Err(PlistError::ControlCharacter {
                    value: text.to_owned(),
                });
            }
            c => out.push(c),
        }
    }
    Ok(out)
}
