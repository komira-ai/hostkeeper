//! Halt: a failure no version can repair (`docs/design/host-updates.md`, section 3.8).
//!
//! A host whose last update to a service `Failed` in the `halt` phase has found something a
//! new release cannot fix (a tainted disk, a state no hook can read). The coordinator raises a
//! halt for the service: every host and role of it is held ([`Gate::Halted`]), a new desired
//! version does not lift it, the convergence says `halted`, and nothing retries or rolls back.
//! The coordinator has no pager: a standing halt is an alert ([`crate::alerts`]), logged at
//! error with the field `alert = "halt"` once when it appears, which is what an operator alert
//! watches.
//!
//! In group mode ([`crate::group_scope`]) a halt is a (group, package)'s: a host's report halts the
//! package in the group of the host alone, a clear lifts it there, and the generation the agents are
//! told is the group's.
//!
//! The halt is kept in the ops store ([`crate::ops`]). Raising it is written *after* it is in
//! memory, so a halt that could not be written still stops the service, and under a marker
//! file ([`crate::ops::Write::Halt`]): a write that fails is tried again at every heartbeat
//! until it lands, and a coordinator started over the marker refuses to start unless the ops
//! file holds the halt. Clearing it is written *first*, so a clear that could not be written
//! leaves it standing.
//!
//! Who raised a halt says what lifts it ([`HaltOrigin`], design section 5.4). A halt the
//! updater raised itself (a report whose reason is one of [`UPDATER_HALT_REASONS`]) is lifted
//! by the operator's `clear_halt` and also by CI's set of a desired version other than its
//! target ([`lift_by_ci`]): a fix-forward release is the remedy. A halt a hook raised (its exit
//! 76; any other reason) is lifted only by `clear_halt`, and so is a halt an operator raised
//! (`Releases.Halt`, [`raise_by_operator`]). A force lifts any of them ([`crate::force`]).
//!
//! The halted host keeps reporting the same `last_update` until it updates again, so a clear
//! also records `(node, target)` as acknowledged, and a report for an acknowledged pair is
//! not raised. A clear clears the service: every other halt report received while the halt
//! stood (`halt_seen`, kept with the halt) is acknowledged with it, so the coordinator and the
//! agents, which lift on the generation, agree. A `last_update` has no attempt number or
//! finish stamp, so a report is identified by the host and its whole `last_update` for as long as
//! the host keeps reporting it: a different `last_update` (another target, reason or class, or no
//! halt at all) ends the host's acknowledgement ([`end_ack`]), and a halt reported after that is
//! new. Section 3.8 of the design also ends it when the host is ordered the acknowledged
//! target, and leaves it standing for an order of another target. That order cannot be given
//! while the acknowledgement stands: the only place an order is produced is
//! `Coordinator::reply_to` (`service.rs`, the call of `Fleet::order`), after [`decide`], and
//! `decide_marked` holds a host whose beat's `last_update` is a failure of the desired version
//! (`Skip::FailedHere`, `decide.rs`) and one that is updating (`Gate::Updating`). The
//! acknowledgement stands only while the host's beat carries the acknowledged report, a failed
//! update of its target, and any other beat ends it in [`note`], before [`decide`] and
//! durably, with no order for the service if the write fails. No operator verb lifts
//! `FailedHere`, and a change of the desired state does not change what the host reports.
//! A service the host is updating is not read: its `last_update` is the one before.
//!
//! Each clear raises the service's generation by one. A reply to an agent that announced
//! [`FEATURE`] carries the generation of every service the coordinator defines that has one
//! (`halts`): the agent lifts its hold when it sees a number newer than the one it halted
//! under, so a lost heartbeat cannot make it miss a clear, and a restart of the coordinator,
//! which reads the number from the halts file beside the ops file ([`crate::ops_file`]), does
//! not forget it.
//!
//! A report is ignored when its host is not counted by the service (the service is not
//! defined, the host's role or selector leaves it out, it is unmanaged, or an operator excluded
//! it: [`crate::membership::standing`], the test recording uses), when an operator cleared
//! exactly that report, and when the service is halted already: the first halt stands until it
//! is cleared (the convergence names it).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Heartbeat, Outcome, Phase, Token, UpdateResult};

use crate::decide::{Decision, Gate, decide_marked};
use crate::desired::Desired;
use crate::fleet::{Fleet, Node};
use crate::host_groups::GroupName;
use crate::identity::Identity;
use crate::membership::{self, Standing};
use crate::ops::{AuditEntry, Halt, HaltAck, HaltOrigin, OpsError, OpsState, OpsStore, Write};
use crate::services::{ServiceDef, Services};

/// The feature an agent announces to be told the clear generations.
pub const FEATURE: &str = "halt-v1";

/// Who a halt is logged as: the coordinator, on a host's report.
pub const COORDINATOR: &str = "coordinator";

/// The check of a halt whose report named none.
pub const UNSPECIFIED: &str = "unspecified";

/// The reasons of the halts the updater raises itself, rather than a hook's check: a staged
/// release whose digest changed after it was verified. A halt of any other reason, or of none,
/// is a hook's.
pub const UPDATER_HALT_REASONS: &[&str] = &["artifact_changed"];

/// Who raised the halt `report` reports: the updater for one of [`UPDATER_HALT_REASONS`], a hook
/// for any other, and for none.
#[must_use]
pub fn origin_of(report: Option<&UpdateResult>) -> HaltOrigin {
    let reason = report.and_then(|r| r.reason.as_ref()).map(Token::as_str);
    if reason.is_some_and(|reason| UPDATER_HALT_REASONS.contains(&reason)) {
        HaltOrigin::Updater
    } else {
        HaltOrigin::Hook
    }
}

/// The halt `last` reports for the node `id`, if it reports one: a failed update in the
/// `halt` phase. The check is the report's reason.
fn reported(last: Option<&UpdateResult>, id: &HostId) -> Option<Halt> {
    let last = last?;
    matches!(
        (last.outcome, last.phase),
        (Outcome::Failed, Some(Phase::Halt))
    )
    .then(|| Halt {
        node_id: id.to_string(),
        check: last
            .reason
            .as_ref()
            .map_or(UNSPECIFIED, Token::as_str)
            .to_owned(),
        target: last.target.clone(),
        report: Some(last.clone()),
        origin: origin_of(Some(last)),
    })
}

/// Why a halt report changed nothing.
enum Unchanged {
    /// An operator cleared this report.
    Acked,
    /// The service is halted already and the report is the halt's own or kept already.
    Known,
    /// The host has no acknowledgement that the report ends.
    NoAck,
    /// The ops file could not be written.
    Store(OpsError),
}

impl From<OpsError> for Unchanged {
    fn from(why: OpsError) -> Self {
        Self::Store(why)
    }
}

/// Raises the halts the heartbeat `beat` of the host `who` reports, for the `legacy` service
/// (its top-level facts, when one is set) and for each service it lists. Before the heartbeat is decided, so the host that
/// reports a halt is ordered nothing in the same reply, and under the fleet's lock, which every
/// decision and order is made under (`Coordinator::reply_to`): a halt is never raised between
/// another heartbeat's decision to order and its order.
///
/// A service whose update the host is running is skipped: its `last_update` is the one before,
/// not a report. Otherwise a `last_update` that is not the halt an operator cleared for the
/// host ends that acknowledgement ([`end_ack`]), and one that is a halt is raised unless it is
/// ignored. Answers the services for which an acknowledgement should have ended and could not
/// be written: the host is ordered nothing of them in this reply.
pub(crate) fn note(
    ops: &OpsStore,
    services: &Services,
    legacy: Option<&str>,
    who: &Identity,
    beat: &Heartbeat,
    node: &Node,
    now: Duration,
) -> Vec<String> {
    let state = ops.state();
    let listed = beat.services.as_deref().unwrap_or_default();
    let top = legacy
        .into_iter()
        .map(|name| (name, beat.last_update.as_ref(), beat.updating.is_some()));
    let reports = top.chain(listed.iter().map(|one| {
        (
            one.name.as_str(),
            one.last_update.as_ref(),
            one.updating.is_some(),
        )
    }));
    let mut held = Vec::new();
    for (service, last, updating) in reports {
        let counted = services.get(service).filter(|def| {
            membership::standing(def, &who.host_id, node, &state, now) == Standing::Counted
        });
        let Some(group) = counted.filter(|_| !updating).map(|def| def.group()) else {
            continue;
        };
        let report = reported(last, &who.host_id);
        if !end_ack(ops, group, service, &who.host_id, report.as_ref()) {
            held.push(service.to_owned());
        }
        if let Some(halt) = report {
            raise(ops, group, service, halt);
        }
    }
    held
}

/// Ends the acknowledgement the host `id` has for `service`, unless `report`, the halt the
/// host reports now, is the very one acknowledged: a different `last_update` (another target,
/// reason or class, or no halt at all) is a different report, so a halt the host reports from
/// then on is a new failure, even of the same target. Written before it is in memory: it
/// removes protection, and one that stays on disk would swallow the new failure after a
/// restart. Decided under the writers' lock, on the state it changes. Answers whether it
/// holds: `false` when the write failed and the acknowledgement stands, so no order may go out
/// before it is gone.
fn end_ack(
    ops: &OpsStore,
    group: Option<&GroupName>,
    service: &str,
    id: &HostId,
    report: Option<&Halt>,
) -> bool {
    let node = id.to_string();
    let report = report.and_then(|halt| halt.report.clone());
    let differs = |ack: &HaltAck| {
        ack.node_id == node && !report.as_ref().is_some_and(|r| ack.matches(&node, r))
    };
    let held = ops.read(|state| state.halt_acked_in(group, service).iter().any(differs));
    if !held {
        return true;
    }
    let ended = ops.decide_change(Write::BeforeMemory, |state| {
        let ended = state.edit_marks_in(group, service, |marks| {
            let before = marks.halt_acked.len();
            marks.halt_acked.retain(|ack| !differs(ack));
            marks.halt_acked.len() != before
        });
        if !ended {
            return Err(Unchanged::NoAck);
        }
        let mut entry = AuditEntry::new(COORDINATOR, "halt_ack_ended");
        entry.service = Some(service.to_owned());
        entry.node_id = Some(node.clone());
        entry.host_group = group.map(ToString::to_string);
        Ok(entry)
    });
    match ended {
        Ok(_) | Err(Unchanged::NoAck) => true,
        Err(Unchanged::Store(why)) => {
            tracing::error!(service, %why, "the end of an acknowledgement could not be written: no order for the host");
            false
        }
        Err(Unchanged::Acked | Unchanged::Known) => unreachable!("not answered by this change"),
    }
}

/// Raises `halt` for `service`, or keeps it for the clear when the service is halted already,
/// unless an operator cleared exactly this report: in memory first, then committed under the
/// marker of changes not yet written ([`Write::Halt`]). Which of these it is is decided under the
/// writers' lock, on the state the change is applied to, so a clear or another host's halt that
/// got there first is seen. A failed commit is logged and the change stands in memory, pending;
/// it is committed again at every heartbeat until it lands ([`OpsStore::retry_unwritten`]), and
/// the marker stops a restart before then.
///
/// A report made while the halt stands is kept once per host, with the halt (`halt_seen`), for
/// the clear to acknowledge: a report the halt itself holds, or one kept already, is not kept
/// again.
fn raise(ops: &OpsStore, group: Option<&GroupName>, service: &str, halt: Halt) {
    let node_id = halt.node_id.clone();
    let changed = ops.decide_change(Write::Halt, |state| {
        state.edit_marks_in(group, service, |marks| {
            let acked = halt.report.as_ref().is_some_and(|report| {
                marks
                    .halt_acked
                    .iter()
                    .any(|ack| ack.matches(&halt.node_id, report))
            });
            if acked {
                return Err(Unchanged::Acked);
            }
            let mut entry = AuditEntry::new(COORDINATOR, "halt");
            entry.service = Some(service.to_owned());
            entry.node_id = Some(halt.node_id.clone());
            entry.host_group = group.map(ToString::to_string);
            entry.reason = Some(halt.check.clone());
            let Some(standing) = &marks.halt else {
                marks.halt = Some(halt);
                return Ok(entry);
            };
            let ack = HaltAck::from(halt);
            let own = (&standing.node_id, &standing.report) == (&ack.node_id, &ack.report);
            if own || marks.halt_seen.contains(&ack) {
                return Err(Unchanged::Known);
            }
            marks.halt_seen.retain(|seen| seen.node_id != ack.node_id);
            marks.halt_seen.push(ack);
            entry.action = "halt_report".to_owned();
            Ok(entry)
        })
    });
    match changed {
        Ok(_) | Err(Unchanged::Known | Unchanged::NoAck) => {}
        Err(Unchanged::Acked) => {
            tracing::debug!(service, %node_id, "ignored a halt report: cleared");
        }
        Err(Unchanged::Store(why)) => tracing::error!(
            alert = "halt",
            service,
            %node_id,
            %why,
            "a halt or a halt report could not be written to the ops file: it stands in memory, and the write is tried again at every heartbeat"
        ),
    }
}

/// The check of a halt an operator raised (`Releases.Halt`).
pub const OPERATOR_CHECK: &str = "operator";

/// Raises a halt of `service` for the operator `by`, with `reason`, as a host's halt report
/// raises one ([`raise`]): in memory first, then committed under the marker of changes not yet
/// written ([`Write::Halt`]): an alert ([`crate::alerts`]). The halt names no host, the check
/// [`OPERATOR_CHECK`] and the desired version `target`; its origin is [`HaltOrigin::Operator`],
/// so only `clear_halt`, or a force, lifts it. Decided under the writers' lock: `already` is
/// answered, and nothing changes, when a halt stands.
///
/// # Errors
/// `already`; or `E` from [`OpsError`] when the write failed, the halt then standing in memory
/// and its write tried again at every heartbeat.
pub(crate) fn raise_by_operator<E: From<OpsError>>(
    ops: &OpsStore,
    service: &str,
    target: &Version,
    by: &str,
    reason: &str,
    already: E,
) -> Result<AuditEntry, E> {
    ops.decide_change(Write::Halt, |state| {
        let marks = state.services.entry(service.to_owned()).or_default();
        if marks.halt.is_some() {
            return Err(already);
        }
        marks.halt = Some(Halt {
            node_id: String::new(),
            check: OPERATOR_CHECK.to_owned(),
            target: target.clone(),
            report: None,
            origin: HaltOrigin::Operator,
        });
        let mut entry = AuditEntry::new(by, "halt");
        entry.service = Some(service.to_owned());
        entry.reason = Some(reason.to_owned());
        Ok(entry)
    })
}

/// What CI's set of `version` for `service` did to its halt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifted {
    /// There is no halt, or it stays: a hook raised it, or it is of `version` itself.
    Nothing,
    /// The halt the updater raised was lifted, and its generation counted.
    Lifted,
}

/// Lifts the halt of `service` when the updater raised it and CI has set a version other than
/// its target, `version`, in the run of `workflow`: written first, like the operator's clear,
/// and audited as CI's (`lift_halt`). As a clear does, it acknowledges the halt's report and
/// every report kept while it stood, and counts the generation, so the agents lift their hold.
/// A halt with a report kept while it stood that a hook raised is not lifted: that report is a
/// hook's halt of the service, which only `clear_halt` lifts.
///
/// # Errors
/// [`OpsError`]: the ops file could not be written; the halt stands.
pub fn lift_by_ci(
    ops: &OpsStore,
    service: &str,
    version: &Version,
    workflow: &str,
) -> Result<Lifted, OpsError> {
    lift_by_setter(ops, None, service, version, crate::reconcile::CI, workflow)
}

/// [`lift_by_ci`] for any setter of the desired version: the audit entry `lift_halt` is by
/// `actor`, and its reason says the version was set by `set_by`.
///
/// # Errors
/// [`OpsError`]: the ops file could not be written; the halt stands.
pub(crate) fn lift_by_setter(
    ops: &OpsStore,
    group: Option<&GroupName>,
    service: &str,
    version: &Version,
    actor: &str,
    set_by: &str,
) -> Result<Lifted, OpsError> {
    let lifted = ops.decide_change(Write::BeforeMemory, |state| {
        lift(state, group, service, version, actor, set_by).ok_or(Unchanged::Known)
    });
    match lifted {
        Ok(_) => Ok(Lifted::Lifted),
        Err(Unchanged::Store(why)) => Err(why),
        Err(Unchanged::Known | Unchanged::Acked | Unchanged::NoAck) => Ok(Lifted::Nothing),
    }
}

/// Lifts the halt of `service` in `state` as [`lift_by_setter`] does, and answers the audit entry
/// of the lift; `None` when there is no halt to lift.
pub(crate) fn lift(
    state: &mut OpsState,
    group: Option<&GroupName>,
    service: &str,
    version: &Version,
    actor: &str,
    set_by: &str,
) -> Option<AuditEntry> {
    let mut marks = state.marks_in(group, service);
    let liftable = marks.halt.as_ref().is_some_and(|halt| {
        halt.origin == HaltOrigin::Updater
            && halt.target != *version
            && marks
                .halt_seen
                .iter()
                .all(|seen| origin_of(seen.report.as_ref()) == HaltOrigin::Updater)
    });
    let halt = marks.halt.take().filter(|_| liftable)?;
    let mut entry = AuditEntry::new(actor, "lift_halt");
    entry.service = Some(service.to_owned());
    entry.node_id = Some(halt.node_id.clone());
    entry.host_group = group.map(ToString::to_string);
    entry.reason = Some(format!("{version} set by {set_by}"));
    let seen = std::mem::take(&mut marks.halt_seen);
    for report in std::iter::once(HaltAck::from(halt)).chain(seen) {
        marks.halt_acked.retain(|ack| ack.node_id != report.node_id);
        marks.halt_acked.push(report);
    }
    marks.halt_generation = marks.halt_generation.saturating_add(1);
    state.edit_marks_in(group, service, |kept| *kept = marks);
    Some(entry)
}

/// The standing halt of `service` in `state`, if any: the halt in `group`, when it is a service
/// made for one.
pub(crate) fn standing(state: &OpsState, group: Option<&GroupName>, service: &str) -> Option<Halt> {
    state.halt_in(group, service).cloned()
}

/// [`Gate::Halted`] for a service that is halted, and [`decide_marked`] (which holds the
/// versions marked bad) for one that is not.
///
/// A halt is checked first, so that a halted service is told nothing: not an order, and not
/// the reason a version is bad. The halt and the bad versions are read under one lock.
pub(crate) fn decide(
    ops: &OpsStore,
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Decision {
    let group = services.get(service).and_then(ServiceDef::group);
    let (halted, bad) = ops.read(|state| {
        (
            standing(state, group, service).is_some(),
            state
                .services
                .get(service)
                .map(|marks| marks.bad.clone())
                .unwrap_or_default(),
        )
    });
    if halted {
        return Decision::Gate(Gate::Halted);
    }
    decide_marked(services, desired, &bad, service, fleet, id, now)
}

/// The generation of every service an operator has cleared a halt of, by name, of the
/// services `services` defines (in a group, the generations of the group's packages). The ops file keeps the generation of a service that is no longer
/// defined (it counts on if the service comes back), but an agent reads at most a services
/// file's worth of them and refuses an answer with more, so only the defined ones are told.
pub(crate) fn generations(ops: &OpsStore, services: &Services) -> BTreeMap<String, u64> {
    let group = services.group();
    ops.read(|state| {
        services
            .names()
            .map(|name| (name, state.halt_generation_in(group, name)))
            .filter(|(_, generation)| *generation > 0)
            .map(|(name, generation)| (name.to_owned(), generation))
            .collect()
    })
}
