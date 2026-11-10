//! The host's facts file (docs/design/hostgroups.md section 6.4): the host's own values, which
//! the coordinator sends in the answer to a heartbeat (`facts-v1`), written for the services
//! that read them.
//!
//! The file is the daemon's `--facts-file`, by default [`crate::cli::DEFAULT_FACTS_FILE`] on Linux
//! and on a Mac. It is one JSON object of schema [`SCHEMA`], with the host id and host group the
//! coordinator sent, the platform the daemon reports in its heartbeat, and the labels by key:
//!
//! ```json
//! {"schema":1,"host_id":"web-04","host_group":"web","platform":"linux-x86_64","labels":{"rack":"r12"}}
//! ```
//!
//! followed by a newline. A reader ignores fields it does not know; `schema` is raised only for
//! a change a reader of the schema before would misread.
//!
//! **The write.** The file is written whole, owned by the daemon's user (root on a host) with
//! mode 0644, so a service that runs as its own user reads it: a temporary file beside it, synced,
//! renamed over it, and its directory synced, the directory made (0755) when it is missing. It is
//! written only when its content changes: a file that holds the same bytes, with that owner and
//! mode, is left as it is, so facts sent again (after a restart of the coordinator or of the
//! daemon) write nothing. Writing them runs nothing: no service is restarted or reloaded for a
//! change of a fact.
//!
//! **The generation.** The sink reports the generation of the facts the file holds, as it last
//! wrote them or found them already written ([`FactsSink::generation`]); none after the daemon
//! starts, until the first facts come, so the coordinator sends them again and the file, already
//! holding them, is not written. The generation is not kept in the file. A write that fails is
//! logged and changes nothing the sink reports: the coordinator sends the facts again, and the
//! heartbeat loop holds the order of that answer ([`hostkeeper_link::Heartbeater`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_link::{FactsSink, Log, Platform, PushedFacts};
use serde_json::{Map, Value};

use crate::mac_files::{Rendered, Replace, replace};

/// The schema of the file this build writes.
pub const SCHEMA: u32 = 1;

/// The name the temporary file of a write starts with.
const TEMPORARY: &str = "hostkeeper-facts";

/// The bytes of the file that holds `facts` on a host of `platform`: its fields in the order of
/// the module's example, each string escaped as JSON, the labels by key.
#[must_use]
pub fn render(facts: &PushedFacts, platform: Platform) -> Vec<u8> {
    let text = |text: &str| Value::from(text).to_string();
    format!(
        "{{\"schema\":{SCHEMA},\"host_id\":{},\"host_group\":{},\"platform\":{},\"labels\":{}}}\n",
        text(&facts.host_id),
        text(&facts.host_group),
        text(platform.as_str()),
        labels(&facts.labels),
    )
    .into_bytes()
}

/// The labels as a JSON object, by key.
fn labels(labels: &BTreeMap<String, String>) -> Value {
    let object: Map<String, Value> = labels
        .iter()
        .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
        .collect();
    Value::Object(object)
}

/// The daemon's facts file: the [`FactsSink`] of its heartbeat loop.
pub struct FactsFile {
    path: PathBuf,
    platform: Platform,
    owner: u32,
    held: Mutex<Option<u64>>,
    log: Arc<dyn Log>,
}

impl FactsFile {
    /// The facts file at `path` of a host of `platform`, written by the daemon's own user, which
    /// says what it wrote and what failed in `log`.
    #[must_use]
    pub fn new(path: PathBuf, platform: Platform, log: Arc<dyn Log>) -> FactsFile {
        FactsFile::owned_by(path, platform, rustix::process::geteuid().as_raw(), log)
    }

    /// [`FactsFile::new`], with `owner` as the user a file it finds must be owned by to be left
    /// as it is.
    #[must_use]
    pub fn owned_by(path: PathBuf, platform: Platform, owner: u32, log: Arc<dyn Log>) -> FactsFile {
        FactsFile {
            path,
            platform,
            owner,
            held: Mutex::new(None),
            log,
        }
    }
}

impl FactsSink for FactsFile {
    fn generation(&self) -> Option<u64> {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn apply(&self, facts: &PushedFacts) -> bool {
        let bytes = render(facts, self.platform);
        let path = self.path.display();
        let generation = facts.generation;
        match replace(&self.path, &bytes, self.owner, TEMPORARY) {
            Ok(written) => {
                if written == Rendered::Written {
                    self.log
                        .line(&format!("facts generation {generation}: written to {path}"));
                }
                *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(generation);
                true
            }
            Err(error) => {
                let why = match error {
                    Replace::NotAFile => "something other than a file is at its path".to_owned(),
                    Replace::Io(error) => error.to_string(),
                };
                self.log.line(&format!(
                    "error: facts generation {generation} is not written to {path}: {why}"
                ));
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "facts_file_tests.rs"]
mod tests;
