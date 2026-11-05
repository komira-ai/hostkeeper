//! Rolling a service back by itself: what the coordinator records about a version and what
//! it does when a release fails on enough hosts (`docs/design/host-updates.md`, sections 3.8
//! and 5.3).
//!
//! [`plan`] is a pure function of a service's definition, its desired state, the marks the
//! ops store holds for it ([`crate::ops::ServiceOps`]), the fleet and the time; it says what
//! to change and changes nothing. The coordinator applies the answer after every heartbeat
//! (`apply`).

use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Class, Heartbeat, Outcome, Phase};
use serde::Deserialize;

use crate::acks::{drop_ack, stale_acks};
use crate::converge::State;
use crate::desired::{Desired, DesiredStore, StoreError};
use crate::fleet::Fleet;
use crate::membership::{Standing, standing};
use crate::ops::{AuditEntry, OpsError, OpsState, OpsStore, ServiceOps, Write};
use crate::service::convergence_of;
use crate::services::ServiceDef;

/// Who the coordinator's own changes are logged as.
pub const COORDINATOR: &str = "coordinator";

/// Who CI's changes to the ops file (the lift of a halt the updater raised) are logged as.
pub const CI: &str = "ci";

/// Whether the coordinator may roll a service back by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rollback {
    /// Only an operator or CI sets an older desired version.
    #[default]
    Manual,
    /// The coordinator sets the last good version when a release fails on enough hosts.
    Automatic,
}

/// How many hosts must fail an update to the desired version before an `automatic` service
/// rolls back: the larger of `min_hosts` and `percent` of the live hosts, rounded up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FailThreshold {
    /// The share of live hosts, 1 to 100.
    pub percent: u32,
    /// The fewest hosts, at least 1.
    pub min_hosts: u32,
}

impl Default for FailThreshold {
    /// A quarter of the hosts and at least two: two failed hosts is the smallest signal that
    /// is not one bad host.
    fn default() -> Self {
        Self {
            percent: 25,
            min_hosts: 2,
        }
    }
}

impl FailThreshold {
    /// How many failed hosts it takes when `live` hosts are live.
    #[must_use]
    pub fn hosts(self, live: usize) -> usize {
        (self.percent as usize * live)
            .div_ceil(100)
            .max(self.min_hosts as usize)
    }
}

/// One thing [`plan`] says to do for a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Records `version` as the last good one.
    LastGood(Version),
    /// Forgets the last good version.
    ClearLastGood,
    /// Marks `version` bad: it is never ordered again, and CI cannot set it again; only the
    /// operator's unmark or a force lifts the mark.
    MarkBad(Version),
    /// Sets the desired version to `version`, the other desired fields as they are.
    RollBackTo(Version),
    /// Drops the failure ack of the host with this node id.
    DropAck(String),
    /// The canary host with this node id failed the release of `version`: logged as an alert
    /// (`alert = "canary_failed"`) with the rollback it starts.
    CanaryFailed {
        /// The canary's node id.
        node_id: String,
        /// The version it failed.
        version: Version,
    },
}

/// Whether the last update the heartbeat `beat` reports was a failure of `version` that is the
/// release's fault: it failed after the commit or at the release gate, and the hook did not
/// blame the environment. A failure that says nothing about where it happened (an updater
/// without phases) is not counted, nor is one before the commit, at the environment gate, or a
/// halt.
pub(crate) fn is_release_failure_beat(beat: &Heartbeat, version: &Version) -> bool {
    let Some(last) = beat.last_update.as_ref() else {
        return false;
    };
    if last.outcome != Outcome::Failed || last.target != *version || beat.version == *version {
        return false;
    }
    matches!(last.phase, Some(Phase::AfterCommit | Phase::GateRelease))
        && last.class != Some(Class::Environment)
}

/// What to change for `def` given its desired state `want`, the marks `marks` the ops store
/// holds for it, and the fleet at `now`.
///
/// Nothing for a `manual` service (a farm that never asks for a rollback keeps its files as
/// they were), nothing in the quiet start (the coordinator has not heard the hosts that would
/// outvote a few failures), nothing while the service is halted and nothing while a force of it
/// stands. A pair an operator holds is not counted, as a failure or as a live host. Otherwise,
/// the first that applies:
///
/// 1. the desired version has converged and baked and nothing holds a slot: record it as
///    the last good version (unless it is, or is marked bad);
/// 2. the desired version is `min_version` and an older last good version is recorded: forget
///    it, since the version before the first release with hooks is no rollback target;
/// 3. an older last good version is recorded, and the
///    desired version is marked bad or failed its release on [`FailThreshold::hosts`] live
///    hosts, or on a canary: mark it bad (unless it already is), then set the desired version
///    to the last good one (only if that is `min_version` or later, else it is only marked).
///    The mark comes first, so a stop between the two leaves a marked desired version, which
///    this finishes on the next beat. The failures are the verdicts the fleet has counted, each
///    once per host, order id and try ([`crate::verdicts`]); failure reports acked by an
///    override do not count ([`crate::acks`]). A canary's release failure meets the threshold
///    on its own, whatever `min_hosts` says, and is logged as an alert: a canary is a live host
///    of a service with a canary that failed while the canary gate was still closed (fewer
///    than its `count` hosts had baked at the desired version), when only canaries are
///    ordered.
///
/// Acks that no longer match their host's report are dropped whatever else is planned.
#[must_use]
pub fn plan(
    def: &ServiceDef,
    want: &Desired,
    marks: &ServiceOps,
    fleet: &Fleet,
    state: &OpsState,
    now: Duration,
) -> Vec<Change> {
    let mut changes = stale_acks(&def.name, marks, fleet);
    changes.extend(decide(def, want, marks, fleet, state, now));
    changes
}

fn decide(
    def: &ServiceDef,
    want: &Desired,
    marks: &ServiceOps,
    fleet: &Fleet,
    state: &OpsState,
    now: Duration,
) -> Vec<Change> {
    // A force that stands suspends the automatic rollback of its service: the operator chose
    // the version, and only CI's next version or another force ends it ([`crate::force`]).
    if def.rollback != Rollback::Automatic
        || fleet.is_quiet(now)
        || marks.halt.is_some()
        || marks.force.is_some()
    {
        return Vec::new();
    }
    let wanted = &want.version;
    let service = def.name.as_str();
    let live = || {
        fleet
            .hosts_of(service)
            .filter(|(_, h)| def.roles.contains(h.role_name()))
            .filter(|(_, h)| h.is_live(now))
            // The hosts the service counts: not an excluded one, nor one its selector leaves out,
            // nor a pair an operator holds, which is never counted towards a rollback.
            .filter(|(id, _)| !fleet.is_held(service, id))
            .filter(|(id, _)| {
                fleet
                    .node(id)
                    .is_none_or(|node| standing(def, id, node, state, now) != Standing::Dropped)
            })
    };
    let older = marks.last_good.as_ref().filter(|good| *good != wanted);
    let marked = marks.bad.contains(wanted);
    let settled = convergence_of(def, Some(want), fleet, state, now).state == State::Converged
        && !live().any(|(id, h)| fleet.holds_slot(service, id, h, now));
    let recorded = marks.last_good.as_ref() == Some(wanted);
    if settled && !recorded && !marked {
        return vec![Change::LastGood(wanted.clone())];
    }
    let floored = def
        .min_version
        .as_ref()
        .is_some_and(|floor| floor.same_release(wanted));
    if floored {
        return older.map(|_| Change::ClearLastGood).into_iter().collect();
    }
    let Some(back) = older else {
        return Vec::new();
    };
    let counted: Vec<&hostkeeper_wire::HostId> = fleet
        .verdicts_of(service, wanted)
        .filter(|(id, _)| live().any(|(member, _)| member == *id))
        .filter(|(id, report)| {
            let node = id.to_string();
            !marks
                .failure_acked
                .iter()
                .any(|ack| ack.node_id == node && ack.report == **report)
        })
        .map(|(id, _)| id)
        .collect();
    let canary = counted
        .first()
        .filter(|_| canary_open(def, want, fleet, now))
        .map(|id| id.to_string());
    if !marked
        && canary.is_none()
        && counted.len() < def.rollback_after_failed.hosts(live().count())
    {
        return Vec::new();
    }
    let mut changes = Vec::new();
    if !marked {
        if let Some(node_id) = canary {
            changes.push(Change::CanaryFailed {
                node_id,
                version: wanted.clone(),
            });
        }
        changes.push(Change::MarkBad(wanted.clone()));
    }
    // The last good version is a target only at or above the floor.
    if def
        .min_version
        .as_ref()
        .is_none_or(|floor| back.at_least(floor))
    {
        changes.push(Change::RollBackTo(back.clone()));
    }
    changes
}

/// Whether the canary gate of `def` is still closed for `want`: the service has a canary and
/// fewer than its `count` member hosts have baked at the desired version. While it is, only
/// canaries are ordered the desired version ([`crate::canary`]).
fn canary_open(def: &ServiceDef, want: &Desired, fleet: &Fleet, now: Duration) -> bool {
    let Some(canary) = def.canary else {
        return false;
    };
    let baked = fleet
        .hosts_of(&def.name)
        .filter(|(_, h)| def.roles.contains(h.role_name()))
        .filter(|(id, _)| !fleet.is_held(&def.name, id))
        .filter(|(id, h)| {
            h.beat.version == want.version
                && fleet.is_up(&def.name, id, h, now)
                && fleet.bake_beats(&def.name, id, &want.version) >= def.bake_beats
        })
        .count();
    baked < canary.count as usize
}

/// Why a change [`plan`] asked for was not made.
#[derive(Debug, thiserror::Error)]
enum Failed {
    /// The ops file could not be written.
    #[error("the ops file could not be written: {0}")]
    Ops(#[from] OpsError),
    /// The desired state could not be written.
    #[error("the desired state could not be written: {0}")]
    Desired(#[from] StoreError),
}

pub(crate) fn entry(by: &str, action: &str, service: &str, reason: String) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(service.to_owned());
    entry.reason = Some(reason);
    entry
}

fn apply(
    service: &str,
    want: &Desired,
    change: Change,
    ops: &OpsStore,
    store: &DesiredStore,
) -> Result<(), Failed> {
    match change {
        Change::LastGood(version) => {
            let audit = entry(COORDINATOR, "last_good", service, version.to_string());
            ops.change(Write::BeforeMemory, audit, |state| {
                state
                    .services
                    .entry(service.to_owned())
                    .or_default()
                    .last_good = Some(version);
            })?;
        }
        Change::ClearLastGood => {
            let why = "the desired version is the first with update hooks".to_owned();
            let audit = entry(COORDINATOR, "clear_last_good", service, why);
            ops.change(Write::BeforeMemory, audit, |state| {
                state
                    .services
                    .entry(service.to_owned())
                    .or_default()
                    .last_good = None;
            })?;
        }
        Change::MarkBad(version) => {
            let why = format!("{version} failed its release on enough hosts");
            let audit = entry(COORDINATOR, "mark_bad", service, why);
            // Raising: in memory first, so that a failed write still stops the orders.
            ops.change(Write::AfterMemory, audit, |state| {
                state
                    .services
                    .entry(service.to_owned())
                    .or_default()
                    .bad
                    .push(version);
            })?;
        }
        Change::RollBackTo(version) => {
            tracing::warn!(%service, from = %want.version, to = %version, "rolling back");
            let mut back = want.clone();
            // The digest of the version gone back to, as CI registered it, if it did.
            back.release_digest = store.digests(service).get(&version).cloned();
            back.version = version;
            store.set(service, back)?;
        }
        Change::CanaryFailed { node_id, version } => {
            tracing::error!(
                alert = "canary_failed",
                %service,
                %node_id,
                target = %version,
                "the canary failed the release: it is marked bad and rolled back"
            );
        }
        Change::DropAck(node) => {
            let why = format!("{node} reports something else now");
            drop_ack(ops, service, &node, why, Write::AfterMemory)?;
        }
    }
    Ok(())
}

/// Plans and makes the changes for `def` at `now`, after a heartbeat. A change that fails is
/// logged and the rest are left for the next beat: the heartbeat is answered either way.
/// A service with no desired state has nothing to plan.
pub fn reconcile(
    def: &ServiceDef,
    ops: &OpsStore,
    store: &DesiredStore,
    fleet: &Fleet,
    now: Duration,
) {
    let Some(want) = store.get(&def.name) else {
        return;
    };
    let state = ops.state();
    for change in plan(def, &want, &ops.marks(&def.name), fleet, &state, now) {
        if let Err(why) = apply(&def.name, &want, change, ops, store) {
            tracing::error!(service = %def.name, %why, "a rollback change was not made");
            return;
        }
    }
}
