//! What `softwareupdate` lists on a Mac, read for the maintenance look's `macos` collector
//! ([`crate::maintenance_macos`], docs/design/maintenance-updates.md section 1), and which of it
//! is due.
//!
//! `softwareupdate` has no machine-readable output, so its two listings are read by one function,
//! [`parse_softwareupdate_list`], pinned by fixtures of each macOS release the farm runs
//! (`tests/fixtures/maintenance/softwareupdate`):
//!
//! - `softwareupdate --list` names each update in a `* Label: <label>` line, followed by a line of
//!   its details: `Title: <title>, Version: <version>, Size: <size>, Recommended: YES|NO,` and,
//!   for an update that restarts the Mac, `Action: restart,` (or `Action: shut down,`).
//! - `softwareupdate --list-full-installers` names each full installer of macOS in one line:
//!   `* Title: <title>, Version: <version>, Size: <size>, Build: <build>, Deferred: YES|NO`.
//!
//! A line the parser does not know is counted ([`SoftwareUpdateListing::unparsed`]) and never
//! guessed at: a detail line with no `* Label:` line before it, a `* Label:` line with no detail
//! line after it, a details line with a field it does not know (its known fields are still read),
//! and any other line that is not one of the tool's headings.
//!
//! **What is due** ([`macos_due`]): an update whose title starts with `macOS ` and whose major
//! version (the number before the first `.`) is the running one's is a minor update, always due;
//! one of a higher major version is a major upgrade, as is a full installer of a higher major
//! version. A major upgrade is due only when the host's setting allows it ([`MacosMajor::Due`],
//! the group setting `macos_major` of the design's section 3), and then only the newest one.
//! Nothing else `softwareupdate` lists is due (the Command Line Tools, Safari), and neither is a
//! macOS of a lower major version, nor one whose version has no major number.

use std::cmp::Ordering;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::{MAX_DUE_LABEL, MAX_DUE_VERSION};

use crate::maintenance_collect::{clip, compare_versions};

/// The headings `softwareupdate` writes around its listings, which carry nothing.
const HEADINGS: [&str; 5] = [
    "Software Update Tool",
    "Finding available software",
    "Software Update found the following new or updated software:",
    "Software Update found the following full installers:",
    "No new software available.",
];

/// What starts the line that names an update of `--list`.
const LABEL_LINE: &str = "* Label:";

/// What starts the line that names a full installer of `--list-full-installers`.
const INSTALLER_LINE: &str = "* Title:";

/// What starts the line of an update's details in `--list`.
const DETAILS_LINE: &str = "Title:";

/// The fields a details line or a full installer's line may hold.
const FIELDS: [&str; 7] = [
    "Title",
    "Version",
    "Size",
    "Recommended",
    "Action",
    "Build",
    "Deferred",
];

/// The `Action` values that restart the Mac.
const RESTART_ACTIONS: [&str; 2] = ["restart", "shut down"];

/// The `YES` or `NO` of the fields `Recommended` and `Deferred`.
const YES_NO: [&str; 2] = ["YES", "NO"];

/// What the title of a macOS update or installer starts with.
const MACOS_TITLE: &str = "macOS ";

/// Whether a major upgrade of macOS is due on the host (`hostkeeper daemon
/// --maintenance-macos-major`, the group setting `macos_major` of the design's section 3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacosMajor {
    /// A major upgrade is never due, though `softwareupdate` lists it: the default.
    #[default]
    NotDue,
    /// The newest major upgrade is due.
    Due,
}

impl MacosMajor {
    /// [`MacosMajor::Due`] when the flag is given, else [`MacosMajor::NotDue`].
    #[must_use]
    pub fn from_flag(due: bool) -> MacosMajor {
        if due {
            MacosMajor::Due
        } else {
            MacosMajor::NotDue
        }
    }
}

/// One update `softwareupdate --list` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedUpdate {
    /// Its label, which names it to `softwareupdate --install`.
    pub label: String,
    /// Its title (`macOS Tahoe 26.2`).
    pub title: String,
    /// Its version (`26.2`).
    pub version: String,
    /// Whether it restarts the Mac (`Action: restart` or `Action: shut down`).
    pub restart: bool,
}

/// One full installer `softwareupdate --list-full-installers` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullInstaller {
    /// Its title (`macOS Tahoe`).
    pub title: String,
    /// Its version (`26.2`), which names it to `softwareupdate --fetch-full-installer`.
    pub version: String,
    /// Its build (`25C56`).
    pub build: String,
}

/// What [`parse_softwareupdate_list`] read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoftwareUpdateListing {
    /// The updates `--list` names, in its order.
    pub updates: Vec<ListedUpdate>,
    /// The full installers `--list-full-installers` names, in its order.
    pub installers: Vec<FullInstaller>,
    /// How many lines it did not know.
    pub unparsed: u32,
}

/// The fields of one line: the value of each known field, and whether the line held anything
/// else (a piece that is no `<field>: <value>`, a field it does not know, a field twice, or a
/// known field with a value it does not know).
#[derive(Default)]
struct Fields<'a> {
    title: Option<&'a str>,
    version: Option<&'a str>,
    action: Option<&'a str>,
    build: Option<&'a str>,
    strange: bool,
}

/// Reads what `softwareupdate --list` or `softwareupdate --list-full-installers` wrote to its
/// standard output, as the module says.
#[must_use]
pub fn parse_softwareupdate_list(text: &str) -> SoftwareUpdateListing {
    let mut listing = SoftwareUpdateListing::default();
    let mut label: Option<&str> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix(LABEL_LINE) {
            if label.is_some() {
                listing.count_unparsed();
            }
            label = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix(INSTALLER_LINE) {
            listing.count_pending(label.take());
            listing.installer(&format!("{DETAILS_LINE}{rest}"));
        } else if line.starts_with(DETAILS_LINE) {
            match label.take() {
                Some(name) => listing.update(name, line),
                None => listing.count_unparsed(),
            }
        } else {
            listing.count_pending(label.take());
            if !(line.is_empty() || HEADINGS.contains(&line)) {
                listing.count_unparsed();
            }
        }
    }
    listing.count_pending(label);
    listing
}

impl SoftwareUpdateListing {
    fn count_unparsed(&mut self) {
        self.unparsed = self.unparsed.saturating_add(1);
    }

    /// Counts a `* Label:` line no details line followed.
    fn count_pending(&mut self, label: Option<&str>) {
        if label.is_some() {
            self.count_unparsed();
        }
    }

    /// The update `label` of the details `line`: kept when it names a title and a version; the
    /// line counted when it holds anything else or lacks either.
    fn update(&mut self, label: &str, line: &str) {
        let fields = fields(line);
        match (fields.title, fields.version) {
            (Some(title), Some(version)) if !label.is_empty() => {
                self.updates.push(ListedUpdate {
                    label: label.to_owned(),
                    title: title.to_owned(),
                    version: version.to_owned(),
                    restart: fields
                        .action
                        .is_some_and(|action| RESTART_ACTIONS.contains(&action)),
                });
                if fields.strange {
                    self.count_unparsed();
                }
            }
            _ => self.count_unparsed(),
        }
    }

    /// The full installer of `line` (its `* ` taken off): kept when it names a title, a version
    /// and a build; the line counted when it holds anything else or lacks one of them.
    fn installer(&mut self, line: &str) {
        let fields = fields(line);
        match (fields.title, fields.version, fields.build) {
            (Some(title), Some(version), Some(build)) => {
                self.installers.push(FullInstaller {
                    title: title.to_owned(),
                    version: version.to_owned(),
                    build: build.to_owned(),
                });
                if fields.strange || fields.action.is_some() {
                    self.count_unparsed();
                }
            }
            _ => self.count_unparsed(),
        }
    }
}

/// The fields of `line`: its pieces between `, `, the last ending with `,` or not; each piece a
/// field of [`FIELDS`], `: `, and a value that is not empty.
fn fields(line: &str) -> Fields<'_> {
    let mut fields = Fields::default();
    let mut seen: Vec<&str> = Vec::new();
    let body = line.trim_end().trim_end_matches(',');
    for piece in body.split(", ") {
        let Some((name, value)) = piece.split_once(": ") else {
            fields.strange = true;
            continue;
        };
        let value = value.trim();
        if !FIELDS.contains(&name) || seen.contains(&name) || value.is_empty() {
            fields.strange = true;
            continue;
        }
        seen.push(name);
        match name {
            "Title" => fields.title = Some(value),
            "Version" => fields.version = Some(value),
            "Build" => fields.build = Some(value),
            "Action" => {
                fields.strange |= !RESTART_ACTIONS.contains(&value);
                fields.action = Some(value);
            }
            "Recommended" | "Deferred" => fields.strange |= !YES_NO.contains(&value),
            _ => {}
        }
    }
    fields
}

/// The major version of a macOS version: the number before its first `.` (`26` of `26.2`, and of
/// `26`); `None` when that is not a number.
#[must_use]
pub fn major_of(version: &str) -> Option<u64> {
    version.split('.').next()?.trim().parse().ok()
}

/// What of `listing` is due on a Mac running macOS `running`, under `major`, as the module says:
/// the minor updates in the listing's order, then the newest major upgrade when `major` allows it
/// (of equal versions, a listed update before a full installer, and the first of each).
#[must_use]
pub fn macos_due(
    running: &str,
    listing: &SoftwareUpdateListing,
    major: MacosMajor,
) -> Vec<pb::DueItem> {
    let Some(running_major) = major_of(running) else {
        return Vec::new();
    };
    let mut due = Vec::new();
    let mut newest: Option<pb::DueItem> = None;
    let mut consider = |item: pb::DueItem| {
        let newer = newest
            .as_ref()
            .is_none_or(|held| compare_versions(&item.to, &held.to) == Ordering::Greater);
        if newer {
            newest = Some(item);
        }
    };
    for update in &listing.updates {
        if !update.title.starts_with(MACOS_TITLE) {
            continue;
        }
        match major_of(&update.version).map(|to| to.cmp(&running_major)) {
            Some(Ordering::Equal) => due.push(item(
                running,
                &update.version,
                update.restart,
                &update.label,
            )),
            Some(Ordering::Greater) => {
                consider(item(running, &update.version, true, &update.label))
            }
            Some(Ordering::Less) | None => {}
        }
    }
    for installer in &listing.installers {
        let higher = major_of(&installer.version).is_some_and(|to| to > running_major);
        if installer.title.starts_with(MACOS_TITLE) && higher {
            consider(item(running, &installer.version, true, &installer.title));
        }
    }
    if major == MacosMajor::Due {
        due.extend(newest);
    }
    due
}

/// A due `macos` item from `from` to `to`, named `label`, each text cut to its bound.
fn item(from: &str, to: &str, needs_reboot: bool, label: &str) -> pb::DueItem {
    pb::DueItem {
        kind: pb::MaintenanceKind::Macos.into(),
        from: clip(from, MAX_DUE_VERSION),
        to: clip(to, MAX_DUE_VERSION),
        needs_reboot,
        label: clip(label, MAX_DUE_LABEL),
        security: false,
    }
}

#[cfg(test)]
#[path = "maintenance_softwareupdate_tests.rs"]
mod tests;
