//! The force record (docs/design/hostkeeper-design.md, section 5.6, steps 8 to 16): the journal
//! of a hookless `force`, kept in the service's step record as its optional `force` field.
//!
//! A force writes it before it changes anything (phase `started`) and again as each part of the
//! switch is done, then with its verdict (phase `verdicted`). Every part is safe to run twice, so
//! a force whose record has no verdict is run again from its top: at the daemon's start, and on a
//! resend of the same order. The on-disk strings of [`ForcePhase`], [`ForceVerdict`] and
//! [`ForceFailure`] are fixed (a golden test holds them).

use serde::{Deserialize, Serialize};

/// How far a force got: the last part of the switch it finished, in the order the parts run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForcePhase {
    /// The record is written; nothing has changed yet.
    Started,
    /// The `run` units of the version left are stopped (step 9).
    UnitsStopped,
    /// The units and files the target does not list are stopped, disabled and unlinked (step 10).
    Unlinked,
    /// `current` names the target, `previous` the version left (step 11).
    Switched,
    /// The target's units and files are linked (step 12).
    Linked,
    /// `daemon-reload` ran (step 13).
    Reloaded,
    /// The target's `run` units are started and were each seen active (steps 14 and 15).
    UnitsStarted,
    /// The verdict is recorded (step 16).
    Verdicted,
}

impl ForcePhase {
    /// Every phase, in the order a force passes them.
    pub const ALL: [ForcePhase; 8] = [
        ForcePhase::Started,
        ForcePhase::UnitsStopped,
        ForcePhase::Unlinked,
        ForcePhase::Switched,
        ForcePhase::Linked,
        ForcePhase::Reloaded,
        ForcePhase::UnitsStarted,
        ForcePhase::Verdicted,
    ];
}

/// Why a force failed once it began to change the host. The host stays where the failure left
/// it: nothing is rolled back, and the repair is a second force.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForceFailure {
    /// A unit or file could not be stopped, unlinked, switched or linked (steps 9 to 12).
    LinkFailed,
    /// `systemctl daemon-reload` failed (step 13).
    DaemonReloadFailed,
    /// A `run` unit of the target did not start, or was not active within `force_start_secs`
    /// (steps 14 and 15).
    StartFailed,
}

impl ForceFailure {
    /// The reason token, as the record and the reply spell it.
    pub const fn token(self) -> &'static str {
        match self {
            ForceFailure::LinkFailed => "link_failed",
            ForceFailure::DaemonReloadFailed => "daemon_reload_failed",
            ForceFailure::StartFailed => "start_failed",
        }
    }
}

/// How a force ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ForceVerdict {
    /// Every `run` unit of the target was seen active.
    Succeeded,
    /// It stopped at a part of the switch.
    Failed { reason: ForceFailure },
}

/// The journal of one force.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceRecord {
    pub target_version: String,
    /// The package digest the order named: 64 lower-case hex digits.
    pub target_digest: String,
    /// The version `current` named when the force began; `None` when it named none.
    pub from_version: Option<String>,
    pub phase: ForcePhase,
    /// The coordinator's try of the order.
    pub order_try: u64,
    /// The halt generation the order carried.
    pub halt_generation: u64,
    /// The coordinator's id of the order, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `None` until the force has ended.
    pub verdict: Option<ForceVerdict>,
}

impl ForceRecord {
    /// Whether the switch of this force moves `current`: it began on another version.
    pub fn moves_current(&self) -> bool {
        self.from_version.as_deref() != Some(self.target_version.as_str())
    }
}
