//! The start log of the updater on macOS (design section 6.6, the row Self-update of 5.9): each
//! start of the daemon, with the version that started, so that the fallback verb run by the
//! timer job can tell a binary that keeps failing to start ([`crate::selfupdate::should_fall_back`])
//! from one that started once and runs. On Linux systemd counts the starts itself
//! (`StartLimitBurst=`), and nothing writes this file.
//!
//! The file is `<state-dir>/hostkeeper/starts`, beside the step record of the service
//! `hostkeeper`, a JSON object, mode 0600, written durably (temporary file, fsync, rename,
//! directory fsync), the oldest start first:
//!
//! ```text
//! {"schema":1,"starts":[{"version":"0.2.0","at":{"boot_id":"...","mono_secs":812}}]}
//! ```
//!
//! The format is additive (AGENTS.md B8): a top-level key this reader does not know is kept when
//! the file is written again, and a newer `schema` is read for its `starts` all the same. A file
//! that is not such an object is an error naming the file, never read as no starts. The log keeps
//! the last [`MAX_STARTS`] starts, the oldest dropped first.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::bad::write_private;
use crate::clock::{BootId, Stamp};

/// The file's name in the state directory of the service `hostkeeper`.
pub const STARTS_FILE: &str = "starts";
/// The schema this binary writes.
pub const STARTS_SCHEMA: u32 = 1;
/// The most starts the log keeps: more than any burst it is read for.
pub const MAX_STARTS: usize = 16;

/// One start of the daemon: the version that started, and when.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DaemonStart {
    /// The daemon's own version.
    pub version: String,
    /// When it started, on the monotonic clock.
    pub at: Stamp,
}

/// Why the log could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StartLogError {
    #[error("cannot use the start log {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file is there and is not a start log.
    #[error("{} is not a start log: {why}", path.display())]
    Garbage { path: PathBuf, why: String },
}

/// The file as it is read: its starts, and every other key kept for the next write.
#[derive(Debug, Default, Deserialize)]
struct Shape {
    #[serde(default)]
    starts: Vec<DaemonStart>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// The start log in one state directory.
#[derive(Debug)]
pub struct StartLog {
    path: PathBuf,
    shape: Shape,
}

impl StartLog {
    /// The log in the state directory `dir`; empty when the file is not there.
    pub fn load(dir: &Path) -> Result<StartLog, StartLogError> {
        let path = dir.join(STARTS_FILE);
        let shape = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| StartLogError::Garbage {
                path: path.clone(),
                why: e.to_string(),
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Shape::default(),
            Err(source) => return Err(StartLogError::Io { path, source }),
        };
        Ok(StartLog { path, shape })
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The starts, oldest first.
    pub fn starts(&self) -> &[DaemonStart] {
        &self.shape.starts
    }

    /// When `version` started, oldest first.
    pub fn of(&self, version: &str) -> Vec<Stamp> {
        self.shape
            .starts
            .iter()
            .filter(|start| start.version == version)
            .map(|start| start.at.clone())
            .collect()
    }

    /// Adds `start` and writes the log, durably. Past [`MAX_STARTS`], the oldest starts are
    /// dropped.
    pub fn record(&mut self, start: DaemonStart) -> Result<(), StartLogError> {
        let starts = &mut self.shape.starts;
        starts.push(start);
        let excess = starts.len().saturating_sub(MAX_STARTS);
        starts.drain(..excess);
        let mut object = self.shape.rest.clone();
        // A newer file keeps its schema: this binary changed only `starts` in it.
        object.entry("schema").or_insert(Value::from(STARTS_SCHEMA));
        let listed: Vec<Value> = starts
            .iter()
            .map(|start| {
                serde_json::json!({
                    "version": start.version,
                    // An unknown boot is written as `""`, the form of `Stamp`'s own encoding.
                    "at": {
                        "boot_id": start.at.boot_id.as_ref().map_or("", BootId::as_str),
                        "mono_secs": start.at.mono_secs,
                    },
                })
            })
            .collect();
        object.insert("starts".to_owned(), Value::from(listed));
        let text = Value::Object(object).to_string();
        write_private(&self.path, text.as_bytes()).map_err(|source| StartLogError::Io {
            path: self.path.clone(),
            source,
        })
    }
}
