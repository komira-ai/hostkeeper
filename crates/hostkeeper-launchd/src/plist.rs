//! The XML property lists of the two kinds of launchd job: the daemon's own job and a step job.
//!
//! The writer is small and pure. It knows the four value kinds these two files use (string,
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
        ("KeepAlive", true),
        &job.stdout,
        &job.stderr,
    )
}

/// A step job's plist: `Label`, `ProgramArguments`, `RunAtLoad` true, `AbandonProcessGroup`
/// false, and the two output paths. It has no `KeepAlive`.
pub fn step_plist(job: &StepJob) -> Result<String, PlistError> {
    let argv = std::iter::once(job.program.as_os_str())
        .chain(job.args.iter().map(OsString::as_os_str))
        .map(utf8)
        .collect::<Result<Vec<_>, _>>()?;
    job_plist(
        &job.label,
        argv,
        ("AbandonProcessGroup", false),
        &job.stdout,
        &job.stderr,
    )
}

/// One value of a property list.
enum Value {
    Str(String),
    Bool(bool),
    Array(Vec<Value>),
    Dict(Vec<(&'static str, Value)>),
}

fn job_plist(
    label: &Label,
    argv: Vec<String>,
    flag: (&'static str, bool),
    stdout: &Path,
    stderr: &Path,
) -> Result<String, PlistError> {
    let dict = Value::Dict(vec![
        ("Label", Value::Str(label.as_str().to_owned())),
        (
            "ProgramArguments",
            Value::Array(argv.into_iter().map(Value::Str).collect()),
        ),
        ("RunAtLoad", Value::Bool(true)),
        (flag.0, Value::Bool(flag.1)),
        ("StandardOutPath", Value::Str(utf8(stdout.as_os_str())?)),
        ("StandardErrorPath", Value::Str(utf8(stderr.as_os_str())?)),
    ]);
    let mut out = String::from(HEADER);
    render(&dict, 0, &mut out)?;
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
                out.push_str(&format!("{indent}\t<key>{key}</key>\n"));
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
