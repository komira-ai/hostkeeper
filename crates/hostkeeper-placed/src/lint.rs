//! The load rules that belong to linking (design section 4.7, "Units, enable and boot"),
//! reported as a refusal of the manifest: class release, reason `manifest_mismatch`, as
//! `hostkeeper_config::manifest::ManifestRefusal` reports its own.
//!
//! - Every `[units] run` entry is a linked unit file or an instance (`name@instance.service`) of
//!   a linked template (`name@.service`).
//! - A unit's command lines (`ExecStart=` and the other keys of [`EXEC_KEYS`]) name files of a
//!   package only through `/var/lib/hostkeeper/packages/<service>/current/...`: any other path
//!   under `/var/lib/hostkeeper/packages/` (a version directory, another service's) would pin the
//!   unit to a version or to another package. The prefixes systemd reads before the command
//!   (`-`, `@`, `+`, `!`, `:`) are stripped first, and every word is judged, `--flag=<path>`
//!   included. A launchd plist's first `ProgramArguments` element is held to the same rule.
//! - Every `run` unit's file has an `[Install] WantedBy=` naming at least one unit, the daemon's
//!   way to enable it (see [`wanted_by`]).

use std::collections::BTreeMap;

use hostkeeper_config::manifest::{Class, Manifest};
use hostkeeper_config::names::UnitName;

use crate::fs::Fs;
use crate::roots::{PACKAGES_DIR, Roots};

/// The keys of a systemd unit whose value is a command line.
pub const EXEC_KEYS: [&str; 7] = [
    "ExecStart",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStop",
    "ExecStopPost",
    "ExecReload",
    "ExecCondition",
];
/// The characters systemd reads as prefixes of a command, before its path.
pub const COMMAND_PREFIXES: [char; 5] = ['-', '@', '+', '!', ':'];

/// Why a manifest's units are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LintError {
    #[error(
        "the [units] run entry {unit:?} is neither a file of units/ nor an instance of a template there"
    )]
    RunUnit { unit: String },
    #[error(
        "units/{file} {key} names {path:?}, which is under {PACKAGES_DIR}/ but not {PACKAGES_DIR}/{service}/current/"
    )]
    PackagePath {
        file: String,
        key: String,
        path: String,
        service: String,
    },
    #[error(
        "the run unit {unit} has no [Install] WantedBy= in units/{file}, so the daemon cannot enable it"
    )]
    NoWantedBy { unit: String, file: String },
    #[error("units/{file} has WantedBy={target:?}, which is not the name of a unit")]
    BadTarget { file: String, target: String },
    #[error("units/{file} cannot be read as text: {why}")]
    Unreadable { file: String, why: String },
}

impl LintError {
    /// Always `Release`: the package is wrong.
    pub fn class(&self) -> Class {
        Class::Release
    }

    /// Always `manifest_mismatch`, the reason of every refusal of the package (design 5.5).
    pub fn reason(&self) -> &'static str {
        "manifest_mismatch"
    }
}

/// The lines of a systemd unit file with their continuations (a trailing `\`) joined, and with
/// comment lines (`#`, `;`) and blank lines left out.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut open = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() | line.starts_with(['#', ';']) {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                open.push_str(head);
                open.push(' ');
            }
            None => {
                open.push_str(line);
                lines.push(std::mem::take(&mut open));
            }
        }
    }
    lines.push(open);
    lines.retain(|line| !line.is_empty());
    lines
}

/// The `(section, key, value)` of each assignment of a systemd unit file.
fn assignments(text: &str) -> Vec<(String, String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in logical_lines(text) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.push((
                section.clone(),
                key.trim().to_owned(),
                value.trim().to_owned(),
            ));
        }
    }
    out
}

/// Whether `path` is outside `<packages>/`, or is under `<packages>/<service>/current/` with no
/// `.` or `..` component after it.
fn through_current(service: &str, path: &str) -> bool {
    let Some(rest) = path.strip_prefix(PACKAGES_DIR) else {
        return true;
    };
    let Some(below) = rest.strip_prefix('/') else {
        // `/var/lib/hostkeeper/packagesx`, or the directory itself.
        return true;
    };
    below
        .strip_prefix(service)
        .and_then(|rest| rest.strip_prefix("/current/"))
        .is_some_and(|tail| !tail.split('/').any(|part| (part == ".") | (part == "..")))
}

/// The first path of a command line that names a package file other than through `current`.
fn bad_command_path(service: &str, command: &str) -> Option<String> {
    command
        .trim_start_matches(COMMAND_PREFIXES)
        .split_whitespace()
        .map(|word| word.trim_matches(['"', '\'']))
        .flat_map(|word| [word, word.split_once('=').map_or("", |(_, value)| value)])
        .find(|path| !through_current(service, path))
        .map(str::to_owned)
}

/// The first `<string>` of the `ProgramArguments` array of a launchd plist, if it has one.
fn plist_program(text: &str) -> Option<&str> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    let array = after.trim_start().strip_prefix("<array>")?;
    let first = array.trim_start().strip_prefix("<string>")?;
    first.split_once("</string>").map(|(value, _)| value.trim())
}

/// Checks the command lines of the unit file `file` of `service`, whose text is `text`: a
/// launchd plist (`.plist`) by its first `ProgramArguments` element, a systemd unit by every key
/// of [`EXEC_KEYS`].
pub fn check_unit_text(service: &str, file: &str, text: &str) -> Result<(), LintError> {
    let refuse = |key: &str, path: String| LintError::PackagePath {
        file: file.to_owned(),
        key: key.to_owned(),
        path,
        service: service.to_owned(),
    };
    if file.ends_with(".plist") {
        let program = plist_program(text).unwrap_or_default();
        return match through_current(service, program) {
            true => Ok(()),
            false => Err(refuse("ProgramArguments", program.to_owned())),
        };
    }
    for (_, key, value) in assignments(text) {
        let bad = EXEC_KEYS
            .contains(&key.as_str())
            .then(|| bad_command_path(service, &value))
            .flatten();
        if let Some(path) = bad {
            return Err(refuse(&format!("{key}="), path));
        }
    }
    Ok(())
}

/// The units named by the `WantedBy=` of the `[Install]` section of a systemd unit, in order;
/// an empty `WantedBy=` clears the list, as systemd reads it.
pub fn wanted_by(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for (section, key, value) in assignments(text) {
        if (section == "[Install]") & (key == "WantedBy") {
            if value.is_empty() {
                targets.clear();
            }
            targets.extend(value.split_whitespace().map(str::to_owned));
        }
    }
    targets
}

/// Whether `target` can be the name of a unit: `A-Za-z0-9`, `_ . @ : -`, not starting with `.`.
pub fn is_unit_name(target: &str) -> bool {
    !target.is_empty()
        & !target.starts_with('.')
        & target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b"_.@:-".contains(&b))
}

/// The unit file a `run` entry is started from: the template it is an instance of, or the entry
/// itself when it is a plain unit; either must be a file of `units/`. A template is not a unit
/// that can run, and is refused.
pub(crate) fn unit_file_of(manifest: &Manifest, unit: &str) -> Result<String, LintError> {
    let refuse = || LintError::RunUnit {
        unit: unit.to_owned(),
    };
    let name = UnitName::of(manifest.name.as_str(), unit, false).map_err(|_| refuse())?;
    let file = name.template().unwrap_or_else(|| unit.to_owned());
    manifest
        .unit_files
        .contains(&file)
        .then_some(file)
        .ok_or_else(refuse)
}

/// The text of each file of `units/` of the version directory of `manifest`.
pub(crate) fn unit_texts(
    fs: &dyn Fs,
    roots: &Roots,
    manifest: &Manifest,
) -> Result<BTreeMap<String, String>, LintError> {
    let units = roots
        .packages()
        .version(manifest.name.as_str(), manifest.version.as_str())
        .join("units");
    let mut texts = BTreeMap::new();
    for file in &manifest.unit_files {
        let unreadable = |why: String| LintError::Unreadable {
            file: file.clone(),
            why,
        };
        let bytes = fs
            .read(&units.join(file))
            .map_err(|e| unreadable(e.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| unreadable("not UTF-8".to_owned()))?;
        texts.insert(file.clone(), text);
    }
    Ok(texts)
}

/// The `WantedBy=` units of the `run` unit `unit`, read from `texts` (by [`unit_texts`]): at
/// least one, each a unit name.
pub(crate) fn run_targets(
    manifest: &Manifest,
    texts: &BTreeMap<String, String>,
    unit: &str,
) -> Result<(String, Vec<String>), LintError> {
    let file = unit_file_of(manifest, unit)?;
    let targets = texts
        .get(&file)
        .map(String::as_str)
        .map(wanted_by)
        .unwrap_or_default();
    if targets.is_empty() {
        return Err(LintError::NoWantedBy {
            unit: unit.to_owned(),
            file,
        });
    }
    match targets.iter().find(|target| !is_unit_name(target)) {
        Some(target) => Err(LintError::BadTarget {
            file,
            target: target.clone(),
        }),
        None => Ok((file, targets)),
    }
}

/// Applies every rule of this module to `manifest`, whose version directory is
/// `<packages>/<service>/<version>`, read through `fs`.
pub fn lint(fs: &dyn Fs, roots: &Roots, manifest: &Manifest) -> Result<(), LintError> {
    let texts = unit_texts(fs, roots, manifest)?;
    for (file, text) in &texts {
        check_unit_text(manifest.name.as_str(), file, text)?;
    }
    for unit in &manifest.run {
        run_targets(manifest, &texts, unit)?;
    }
    Ok(())
}
