//! What the hook needs from the params file: the keys of the manifest's `[params]` table and
//! the target version the daemon adds.

use std::path::PathBuf;

use crate::params::{Table, Value};
use crate::report::Failure;

/// The key of the version being installed, written by the daemon.
pub const TARGET_VERSION_KEY: &str = "updater.target_version";

/// How the slots are run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    /// `hello@<slot>.service`, run with `systemctl`.
    Systemd,
    /// The job `hello.<slot>`, run with `launchctl`.
    Launchd,
}

/// The params the hook reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub units: Units,
    /// The port the serving slot listens on.
    pub port: u16,
    /// The hook record directory.
    pub record_dir: PathBuf,
    /// The version being installed; only `verify` needs it.
    pub target_version: Option<String>,
}

impl Settings {
    /// Reads the keys; one that is missing or of the wrong kind is a failure of class
    /// `environment` naming it.
    pub fn from_table(table: &Table) -> Result<Settings, Failure> {
        let units = match text(table, "units")? {
            "systemd" => Units::Systemd,
            "launchd" => Units::Launchd,
            _ => return Err(bad("units", "must be \"systemd\" or \"launchd\"")),
        };
        let port = match table.get("port") {
            Some(Value::Integer(n)) => u16::try_from(*n).ok().filter(|port| *port > 0),
            Some(_) => None,
            None => return Err(missing("port")),
        }
        .ok_or_else(|| bad("port", "must be an integer from 1 to 65535"))?;
        let record_dir = PathBuf::from(text(table, "record_dir")?);
        if !record_dir.is_absolute() {
            return Err(bad("record_dir", "must be an absolute path"));
        }
        let target_version = match table.get(TARGET_VERSION_KEY) {
            Some(value) => Some(
                value
                    .as_text()
                    .ok_or_else(|| bad(TARGET_VERSION_KEY, "must be text"))?
                    .to_owned(),
            ),
            None => None,
        };
        Ok(Settings {
            units,
            port,
            record_dir,
            target_version,
        })
    }
}

fn text<'t>(table: &'t Table, key: &str) -> Result<&'t str, Failure> {
    match table.get(key) {
        Some(value) => value.as_text().ok_or_else(|| bad(key, "must be text")),
        None => Err(missing(key)),
    }
}

fn missing(key: &str) -> Failure {
    Failure::environment(format!("params key {key:?} is missing"))
}

fn bad(key: &str, rule: &str) -> Failure {
    Failure::environment(format!("params key {key:?} {rule}"))
}
