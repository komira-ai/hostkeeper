//! The coordinator as the runner sees it: `hkctl`, one call per command, behind a port.
//!
//! [`Hkctl`] runs the program `--hkctl` names with the coordinator's endpoint, its CA file and
//! the operator's token file as flags, so the token is never an argument or an environment
//! variable of the runner. A request (a message `hkctl` takes only as JSON) is written to
//! `requests/<n>.json` in the run directory first, which keeps what was sent.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use thiserror::Error;

/// What `hkctl` answered: its exit code (0 ok, 1 no answer or another code, 2 refused or a usage
/// error, 3 the caller refused; docs/cli.md) and its two outputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Answer {
    /// The reason of a refusal, from the line `refused: <reason>` of stderr.
    pub fn refused(&self) -> Option<&str> {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix("refused: "))
            .map(str::trim)
    }
}

/// The coordinator, one `hkctl` call at a time: `args` are the command and its flags, after the
/// global flags; `request` goes in as `--request <file>`.
pub trait Coordinator {
    fn call(&mut self, args: &[String], request: Option<&Value>) -> Result<Answer, CallError>;
}

/// Why `hkctl` could not be run at all.
#[derive(Debug, Error)]
pub enum CallError {
    #[error("running {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("writing the request {path}: {source}")]
    Request {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{program} was ended by a signal")]
    Signal { program: PathBuf },
}

/// The real coordinator: the `hkctl` program.
#[derive(Debug)]
pub struct Hkctl {
    program: PathBuf,
    globals: Vec<String>,
    requests: PathBuf,
    sent: u32,
}

impl Hkctl {
    /// `program` called with `--endpoint endpoint --ca-file ca_file --token-file token_file`;
    /// requests are written under `requests`, which must exist.
    pub fn new(
        program: &Path,
        endpoint: &str,
        ca_file: &Path,
        token_file: &Path,
        requests: &Path,
    ) -> Self {
        Self {
            program: program.to_owned(),
            globals: vec![
                "--endpoint".to_owned(),
                endpoint.to_owned(),
                "--ca-file".to_owned(),
                ca_file.display().to_string(),
                "--token-file".to_owned(),
                token_file.display().to_string(),
            ],
            requests: requests.to_owned(),
            sent: 0,
        }
    }
}

impl Coordinator for Hkctl {
    fn call(&mut self, args: &[String], request: Option<&Value>) -> Result<Answer, CallError> {
        let mut command = Command::new(&self.program);
        command.args(&self.globals).args(args);
        if let Some(request) = request {
            self.sent += 1;
            let path = self.requests.join(format!("{}.json", self.sent));
            std::fs::write(&path, format!("{request:#}\n")).map_err(|source| {
                CallError::Request {
                    path: path.clone(),
                    source,
                }
            })?;
            command.arg("--request").arg(&path);
        }
        let out = command
            .stdin(Stdio::null())
            .output()
            .map_err(|source| CallError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        let code = out.status.code().ok_or_else(|| CallError::Signal {
            program: self.program.clone(),
        })?;
        Ok(Answer {
            code,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
