//! The plan of a Mac's macOS install: what the `macos` kind's `cutover` chose from the Mac's own
//! listings, kept for the steps that run after it in other processes: the restart, which installs
//! it (in the same boot, or again when the reboot did not come) and, on a Mac with FileVault on,
//! then restarts it authenticated, and `verify`, which checks the target build and removes a major
//! upgrade's installer app.
//!
//! **The file.** [`PLAN_FILE`] in the run's directory (the directory of the `--params` file, where
//! the daemon keeps the run record), written durably (a temporary file, fsync, rename, directory
//! fsync; mode 0600) before the hook records the reboot under way. It is JSON:
//! `{"runId": ..., "build": ..., "label": ...}` for a minor update whose install restarts the Mac,
//! the same with `"authrestart": true` for one installed without `--restart` and followed by
//! `fdesetup authrestart` (FileVault on), `{"runId": ..., "build": ..., "installer": ...}` for a
//! major upgrade. It holds no password: the update account's is read from its own file by the step
//! that uses it.
//!
//! **Reading it.** A plan of another run, or one that is not that JSON, is refused, so a plan an
//! earlier run left is never installed or checked. An installer that is not a directory
//! `Install macOS *.app` directly in `/Applications` is refused too: `verify` removes it. So is an
//! `authrestart` that is not `true`, or one beside an installer: a major upgrade is never followed
//! by an authenticated restart.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::files::write_atomic;
use crate::maintenance_collect::APPLICATIONS;

/// The plan's file in the run's directory.
pub const PLAN_FILE: &str = "macos-install.json";

/// The most bytes of a plan read: far more than one at every bound.
const MAX_PLAN_BYTES: u64 = 16 * 1024;

/// What the name of a macOS installer app starts with.
pub(super) const INSTALLER_PREFIX: &str = "Install macOS ";

/// What the name of an app ends with.
pub(super) const APP_SUFFIX: &str = ".app";

/// How the update is installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Install {
    /// A minor update, by the label `softwareupdate --list` gave it; when `authrestart`, installed
    /// without `--restart` and followed by `fdesetup authrestart` (FileVault on).
    Update { label: String, authrestart: bool },
    /// A major upgrade, by the installer app the stage fetched: its path on the Mac
    /// (`/Applications/Install macOS <name>.app`).
    Upgrade { app: String },
}

/// What the `cutover` chose: how to install, and the build `sw_vers` reports once it is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub build: String,
    pub install: Install,
}

/// Whether `name` is the name of a macOS installer app: `Install macOS <name>.app`, with no `/`.
pub(super) fn installer_name(name: &str) -> bool {
    name.len() > INSTALLER_PREFIX.len() + APP_SUFFIX.len()
        && name.starts_with(INSTALLER_PREFIX)
        && name.ends_with(APP_SUFFIX)
        && !name.contains('/')
}

/// Whether `app` is the path of a macOS installer app directly in [`APPLICATIONS`].
fn installer_path(app: &str) -> bool {
    app.strip_prefix(APPLICATIONS)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(installer_name)
}

/// The text of `value`'s field `key`, when it is text.
fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

/// The plan file of a run's directory; none for a hook whose parameters came on standard input.
pub(super) struct PlanFile {
    dir: Option<PathBuf>,
}

impl PlanFile {
    /// The plan file in `dir`.
    pub(super) fn new(dir: Option<&Path>) -> PlanFile {
        PlanFile {
            dir: dir.map(Path::to_owned),
        }
    }

    /// The run's directory, or why there is none.
    fn dir(&self) -> Result<&Path, String> {
        self.dir.as_deref().ok_or_else(|| {
            "the hook has no run directory: its parameters came on standard input".to_owned()
        })
    }

    /// Keeps `plan` as the plan of the run `run_id`, durably.
    ///
    /// # Errors
    /// Why it could not be written, naming the file.
    pub(super) fn save(&self, run_id: &str, plan: &Plan) -> Result<(), String> {
        let dir = self.dir()?;
        let mut fields = Map::new();
        fields.insert("runId".to_owned(), Value::from(run_id));
        fields.insert("build".to_owned(), Value::from(plan.build.as_str()));
        match &plan.install {
            Install::Update { label, authrestart } => {
                if *authrestart {
                    fields.insert("authrestart".to_owned(), Value::Bool(true));
                }
                fields.insert("label".to_owned(), Value::from(&**label))
            }
            Install::Upgrade { app } => fields.insert("installer".to_owned(), Value::from(&**app)),
        };
        let bytes = Value::Object(fields).to_string();
        write_atomic(dir, PLAN_FILE, bytes.as_bytes(), &|_| false).map_err(|error| {
            format!(
                "the install plan {} could not be written: {error}",
                dir.join(PLAN_FILE).display()
            )
        })
    }

    /// The plan of the run `run_id`.
    ///
    /// # Errors
    /// Why there is none: no file, one that does not read, one of another run, or one that is
    /// not a plan; the text names the file and never quotes it.
    pub(super) fn load(&self, run_id: &str) -> Result<Plan, String> {
        let path = self.dir()?.join(PLAN_FILE);
        let named = |why: &str| format!("the install plan {} {why}", path.display());
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|file| file.take(MAX_PLAN_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|error| named(&format!("could not be read: {error}")))?;
        if bytes.len() as u64 > MAX_PLAN_BYTES {
            return Err(named(&format!("is larger than {MAX_PLAN_BYTES} bytes")));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| named("is not a plan's JSON"))?;
        if text(&value, "runId") != Some(run_id) {
            return Err(named(&format!("is not of the run {run_id}")));
        }
        let build = text(&value, "build")
            .filter(|build| !build.is_empty())
            .ok_or_else(|| named("names no build"))?;
        let authrestart = match value.get("authrestart") {
            None => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(named("has an authrestart that is not true")),
        };
        let install = match (text(&value, "label"), text(&value, "installer")) {
            (Some(label), None) if !label.is_empty() => Install::Update {
                label: label.to_owned(),
                authrestart,
            },
            (None, Some(_)) if authrestart => {
                return Err(named("names an installer app and an authrestart"));
            }
            (None, Some(app)) if installer_path(app) => Install::Upgrade {
                app: app.to_owned(),
            },
            _ => {
                return Err(named(
                    "names neither one update label nor one installer app in /Applications",
                ));
            }
        };
        Ok(Plan {
            build: build.to_owned(),
            install,
        })
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
