//! Whether a Mac will install a release, and when: the coordinator's own rule
//! (`maintenance_xcode.rs`, `plan`: the newest `xcode_keep` releases the store holds are kept, and
//! a Mac is due when `missing.or(unselected)` names one), asked of one release; and which run
//! takes the Mac's Xcode (`service_maintain.rs` reads the store when a turn starts;
//! `maintenance_service.rs`, `turn_of`, fixed the turn's kinds when the run was scheduled).

use std::cmp::Ordering;

use super::read::{Installed, Mac, Open, Policy, Turn};
use crate::xcode_add::find::parts;
use crate::xcode_add::store::Held;

/// What a Mac does with the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum State {
    /// The release is the newest it lacks.
    Missing,
    /// It lacks a newer release too, which the Mac's `xcode` due item names; both come in one turn.
    Behind(String),
    /// It has the release but `xcode-select` names another while the release is the group's
    /// default.
    Unselected,
    /// It has the release installed and selected as the group wants.
    Installed,
    /// The release will not come to it; why.
    LeftOut(String),
}

impl State {
    /// Whether the Mac is due the release.
    pub(super) fn is_due(&self) -> bool {
        matches!(self, State::Missing | State::Behind(_) | State::Unselected)
    }
}

/// Which run takes the Mac's Xcode, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum When {
    /// No open run holds it (or its turn there is over): an operator stores a run.
    Free,
    /// Its turn has not started and takes `xcode`: the run's window, or now.
    Takes {
        run: String,
        window: Option<(u64, u64)>,
    },
    /// The run does not take `xcode` for it.
    NoXcode(String),
    /// Its turn of the run has started: it reads the store no more.
    InTurn(String),
    /// The run is halted in its group, so no further host of it starts.
    Halted { run: String, group: String },
}

/// The newest first: by the numbers of the version (trailing zeros dropped), then its text, then
/// the build, as the coordinator orders releases, then the SHA-256.
pub(super) fn newest_first(a: &Held, b: &Held) -> Ordering {
    let key = |held: &Held| {
        (
            parts(&held.version),
            held.version.clone(),
            held.build.clone(),
        )
    };
    key(b).cmp(&key(a)).then_with(|| a.sha256.cmp(&b.sha256))
}

/// Whether `held` is installed in its own directory, with its build.
fn is_installed(installed: &[Installed], held: &Held) -> bool {
    installed
        .iter()
        .any(|x| !x.foreign && x.version == held.version && x.build == held.build)
}

/// What the Mac, which reports `installed` in a group with `policy`, does with `release`, by the
/// store's `archives` (which hold `release`).
pub(super) fn state(
    group: &str,
    policy: &Policy,
    archives: &[Held],
    release: &Held,
    installed: &[Installed],
) -> State {
    if group.is_empty() {
        return State::LeftOut("it is in no host group".to_owned());
    }
    if policy.keep == 0 {
        return State::LeftOut(format!("group {group} keeps no Xcode"));
    }
    let mut kept: Vec<&Held> = archives.iter().collect();
    kept.sort_by(|a, b| newest_first(a, b));
    kept.truncate(policy.keep);
    if !kept.iter().any(|held| held.sha256 == release.sha256) {
        let newer: Vec<&str> = kept.iter().map(|held| held.version.as_str()).collect();
        return State::LeftOut(format!(
            "group {group} keeps only newer releases ({})",
            newer.join(", ")
        ));
    }
    if is_installed(installed, release) {
        let default = match &policy.default {
            None => kept.first().copied(),
            Some(version) => kept.iter().copied().find(|held| &held.version == version),
        };
        let unselected = default.is_some_and(|wanted| {
            wanted.sha256 == release.sha256
                && !installed.iter().any(|x| {
                    x.selected
                        && !x.foreign
                        && x.version == wanted.version
                        && x.build == wanted.build
                })
        });
        return if unselected {
            State::Unselected
        } else {
            State::Installed
        };
    }
    match kept.iter().find(|held| !is_installed(installed, held)) {
        Some(newest) if newest.sha256 != release.sha256 => State::Behind(newest.version.clone()),
        _ => State::Missing,
    }
}

/// Which run takes `mac`'s Xcode: the first of the open runs, newest first, whose turn of the Mac
/// is not over.
pub(super) fn when(mac: &Mac, open: &[Open]) -> When {
    for entry in open {
        let Some(turn) = entry.turns.iter().find(|turn| turn.host_id == mac.id) else {
            continue;
        };
        let run = entry.run.id.clone();
        match turn_of(turn) {
            Phase::Waiting if entry.run.halted_groups.contains(&mac.group) => {
                return When::Halted {
                    run,
                    group: mac.group.clone(),
                };
            }
            Phase::Waiting
                if turn
                    .kinds
                    .iter()
                    .any(|kind| kind == "MAINTENANCE_KIND_XCODE") =>
            {
                let window = (entry.run.state == "MAINTENANCE_STATE_SCHEDULED")
                    .then_some((entry.run.window_start_unix, entry.run.window_end_unix));
                return When::Takes { run, window };
            }
            Phase::Waiting => return When::NoXcode(run),
            Phase::Started => return When::InTurn(run),
            Phase::Over => {}
        }
    }
    When::Free
}

/// Where a host's turn of a run stands, for the one question of the Xcode.
enum Phase {
    /// Not started, or deferred and offered again: the store is read when it starts.
    Waiting,
    /// In its turn, halted or not back.
    Started,
    /// Done, not reached or skipped: the run does not come back to it.
    Over,
}

fn turn_of(turn: &Turn) -> Phase {
    match turn.step.as_str() {
        "MAINTENANCE_HOST_STEP_WAITING" | "MAINTENANCE_HOST_STEP_DEFERRED" => Phase::Waiting,
        "MAINTENANCE_HOST_STEP_STAGING"
        | "MAINTENANCE_HOST_STEP_DRAINING"
        | "MAINTENANCE_HOST_STEP_APPLYING"
        | "MAINTENANCE_HOST_STEP_REBOOTING"
        | "MAINTENANCE_HOST_STEP_CHECKING"
        | "MAINTENANCE_HOST_STEP_PUTTING_BACK"
        | "MAINTENANCE_HOST_STEP_HALTED"
        | "MAINTENANCE_HOST_STEP_NOT_BACK" => Phase::Started,
        _ => Phase::Over,
    }
}
