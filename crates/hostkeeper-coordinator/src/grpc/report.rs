//! The status and the convergence as `hostkeeper.operator.v1` says them: the coordinator's report
//! types ([`crate::status::Status`], [`crate::converge::Convergence`]) mapped field by field to
//! the messages of `Releases.Status` and `Releases.Converge`.
//!
//! The mapping decides nothing. A fact with no field in the proto is left out (the `hold` of a
//! host that the rollout gate has no spelling for, the legacy service's top-level status, the
//! flattened heartbeat's platform); a proto field with no source here stays at its default (the
//! write record of a service, the pinned count of a role). A standing force is in the marks of
//! the ops state, so [`status`] puts it into the convergence of its service; [`convergence`]
//! alone has no force to name.

use std::collections::HashMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{self, Heartbeat};

use crate::converge::{Convergence, HaltNote, RoleCounts, State};
use crate::decide::{Gate, Skip, Why};
use crate::desired::{Desired, RoleLimits};
use crate::force::ForceRecord;
use crate::holds::holding;
use crate::membership::{HostCounts, Member, Membership};
use crate::operator::{entry_proto, hold_proto};
use crate::ops::{FailureAck, Halt, HaltAck, OpsState, ServiceOps};
use crate::status::{ServiceHostStatus, ServiceStatus, Status};

/// A count as the proto's `uint32`: the number itself, or the largest `uint32` when it does not
/// fit.
fn count<T: TryInto<u32>>(n: T) -> u32 {
    n.try_into().unwrap_or(u32::MAX)
}

/// The status as `Releases.Status` answers it: every service in name order, or only `only` when
/// it is not empty (none when the coordinator manages no such service).
pub(crate) fn status(status: &Status, only: &str) -> proto::StatusResponse {
    proto::StatusResponse {
        services: status
            .per_service
            .iter()
            .filter(|(name, _)| only.is_empty() || name.as_str() == only)
            .map(|(name, one)| service_status(name, one, &status.ops))
            .collect(),
        quiet: status.quiet,
        services_dropped: status.services_dropped,
        facts_dropped: status
            .extras_dropped
            .iter()
            .map(|(fact, n)| ((*fact).to_owned(), *n))
            .collect(),
        audit: status.ops.audit.iter().map(entry_proto).collect(),
    }
}

/// The convergence as `Releases.Converge` answers it. It names no write record and no force: the
/// convergence report holds neither.
pub(crate) fn convergence(convergence: &Convergence) -> proto::Convergence {
    proto::Convergence {
        desired: convergence.desired.as_ref().map(|v| v.as_str().to_owned()),
        quiet: convergence.quiet,
        state: convergence_state(convergence.state).into(),
        roles: convergence.roles.iter().map(role_counts).collect(),
        halt: convergence.halt.as_ref().map(halt_note),
        hosts: convergence.hosts.as_ref().map(host_counts),
        coordinator_commit: convergence.commit.to_owned(),
        record: None,
        force: None,
    }
}

/// One service of the status, with the ops state's marks and the force that stands.
fn service_status(name: &str, one: &ServiceStatus, ops: &OpsState) -> proto::ServiceStatus {
    let marks = ops.services.get(name);
    let mut converged = convergence(&one.convergence);
    converged.force = marks.and_then(|m| m.force.as_ref()).map(force_record);
    proto::ServiceStatus {
        service: name.to_owned(),
        desired: one.desired.as_ref().map(desired_state),
        convergence: Some(converged),
        hosts: one
            .hosts
            .iter()
            .map(|host| service_host(name, host, &one.members, ops))
            .collect(),
        up: count(one.up),
        slots_held: count(one.slots_held),
        marks: marks.map(service_marks),
    }
}

/// A desired state with its limits per role and its package digest.
fn desired_state(desired: &Desired) -> proto::DesiredState {
    let per_role = |limits: &RoleLimits| -> HashMap<String, u32> {
        limits
            .iter()
            .map(|(role, n)| (role.to_owned(), n))
            .collect()
    };
    proto::DesiredState {
        version: desired.version.as_str().to_owned(),
        max_updating: per_role(&desired.max_updating),
        min_up: per_role(&desired.min_up),
        package_digest: desired
            .release_digest
            .as_ref()
            .map(|digest| digest.as_str().to_owned()),
    }
}

/// A convergence state by its name.
fn convergence_state(state: State) -> proto::ConvergenceState {
    match state {
        State::Converged => proto::ConvergenceState::Converged,
        State::Blocked => proto::ConvergenceState::Blocked,
        State::Halted => proto::ConvergenceState::Halted,
        State::InProgress => proto::ConvergenceState::InProgress,
    }
}

/// The hosts of one role by where they stand. The proto's `pinned` count has no source.
fn role_counts(counts: &RoleCounts) -> proto::RoleCounts {
    proto::RoleCounts {
        role: counts.role.clone(),
        at_desired: count(counts.at_desired),
        updating: count(counts.updating),
        failed: count(counts.failed),
        down: count(counts.down),
        pinned: 0,
    }
}

/// The hosts a service does not count, by why.
fn host_counts(counts: &HostCounts) -> proto::HostCounts {
    proto::HostCounts {
        missing: count(counts.missing),
        absent: count(counts.absent),
        excluded: count(counts.excluded),
        unmanaged: count(counts.unmanaged),
        deferred: count(counts.deferred),
    }
}

/// The halt a convergence names. It carries no report: the convergence keeps the host, the check
/// and the version only.
fn halt_note(halt: &HaltNote) -> proto::HaltRecord {
    proto::HaltRecord {
        node_id: halt.node_id.clone(),
        check: halt.check.clone(),
        target: halt.target.as_str().to_owned(),
        report: None,
    }
}

/// A force that stands.
fn force_record(force: &ForceRecord) -> proto::ForceRecord {
    proto::ForceRecord {
        version: force.version.as_str().to_owned(),
        generation: force.generation,
        reason: force.reason.clone(),
    }
}

/// What the ops state marks on one service.
fn service_marks(marks: &ServiceOps) -> proto::ServiceMarks {
    proto::ServiceMarks {
        halt: marks.halt.as_ref().map(halt_record),
        halt_acked: marks.halt_acked.iter().map(halt_ack).collect(),
        halt_seen: marks.halt_seen.iter().map(halt_ack).collect(),
        halt_generation: marks.halt_generation,
        last_good: marks.last_good.as_ref().map(|v| v.as_str().to_owned()),
        bad: marks.bad.iter().map(|v| v.as_str().to_owned()).collect(),
        bad_by_force: marks
            .bad_by_force
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect(),
        failure_acked: marks.failure_acked.iter().map(failure_ack).collect(),
        force_generation: marks.force_generation,
    }
}

/// A standing halt with the report that raised it.
fn halt_record(halt: &Halt) -> proto::HaltRecord {
    proto::HaltRecord {
        node_id: halt.node_id.clone(),
        check: halt.check.clone(),
        target: halt.target.as_str().to_owned(),
        report: halt.report.as_ref().map(update_result),
    }
}

/// A halt report an operator cleared, or one received while the halt stood.
fn halt_ack(ack: &HaltAck) -> proto::HaltRecord {
    proto::HaltRecord {
        node_id: ack.node_id.clone(),
        // An acknowledgement keeps the report, not the check: the report's reason is the check.
        check: ack
            .report
            .as_ref()
            .and_then(|report| report.reason.as_ref())
            .map_or_else(String::new, |reason| reason.as_str().to_owned()),
        target: ack.target.as_str().to_owned(),
        report: ack.report.as_ref().map(update_result),
    }
}

/// A failure report an override acknowledged.
fn failure_ack(ack: &FailureAck) -> proto::FailureAck {
    proto::FailureAck {
        node_id: ack.node_id.clone(),
        report: Some(update_result(&ack.report)),
    }
}

/// One host as `service` sees it. `members` are the nodes the service does not count; a host
/// not among them is present.
fn service_host(
    service: &str,
    host: &ServiceHostStatus,
    members: &[Member],
    ops: &OpsState,
) -> proto::ServiceHost {
    let node_id = host.host.node_id.as_str();
    let role = host.host.role.as_str();
    let member = members.iter().find(|m| m.node_id == node_id);
    let excluded_reason = member.and_then(|m| m.reason.clone());
    let beat = &host.host.beat;
    proto::ServiceHost {
        node_id: node_id.to_owned(),
        role: role.to_owned(),
        live: host.host.live,
        view: Some(proto::HostService {
            service: service.to_owned(),
            up: host.host.up,
            holds_slot: host.host.holds_slot,
            gate_reason: gate_reason(host.hold).into(),
            membership: membership(member.map(|m| m.membership)).into(),
            holds: holding(&ops.holds, service, node_id, role)
                .map(hold_proto)
                .collect(),
            installed_version: installed_version(beat),
            health: health(beat.health).into(),
            last_update: beat.last_update.as_ref().map(update_result),
            excluded_reason: excluded_reason.clone(),
        }),
        beat: Some(service_beat(service, beat)),
        excluded_reason,
    }
}

/// The version a host runs of the service, unless it reported the service absent: an absent
/// service has no version installed, whatever the heartbeat's `version` says.
fn installed_version(beat: &Heartbeat) -> Option<String> {
    match beat.health {
        wire::Health::Absent => None,
        wire::Health::Healthy | wire::Health::Unhealthy => Some(beat.version.as_str().to_owned()),
    }
}

/// What the host's last heartbeat said of `service`: the heartbeat of a host in the status is
/// already the service's own entry.
fn service_beat(service: &str, beat: &Heartbeat) -> agent::ServiceBeat {
    agent::ServiceBeat {
        name: service.to_owned(),
        version: beat.version.as_str().to_owned(),
        health: health(beat.health).into(),
        last_update: beat.last_update.as_ref().map(update_result),
        updating: beat.updating.as_ref().map(|v| v.as_str().to_owned()),
        step: beat.step.as_ref().map(|step| agent::StepRun {
            name: step.name.as_str().to_owned(),
            attempt: step.attempt,
        }),
        commit: commit_state(beat.commit).into(),
        step_secs: beat
            .step_secs
            .iter()
            .map(|(step, secs)| (step.as_str().to_owned(), *secs))
            .collect(),
        promotion_pending: beat.promotion_pending,
        deferred: beat.deferred.as_ref().map(|d| agent::DeferredReport {
            target: d.target.as_str().to_owned(),
            reason: d.reason.as_str().to_owned(),
            retry_after_secs: d.retry_after_secs,
        }),
        pinned: None,
    }
}

/// A verdict of the host. The proto's start time, `forced` flag, detail and log lines have no
/// source on the heartbeat wire.
fn update_result(result: &wire::UpdateResult) -> agent::UpdateResult {
    agent::UpdateResult {
        target: result.target.as_str().to_owned(),
        outcome: outcome(result.outcome).into(),
        phase: phase(result.phase).into(),
        class: class(result.class).into(),
        reason: result
            .reason
            .as_ref()
            .map_or_else(String::new, |reason| reason.as_str().to_owned()),
        started_unix: None,
        order_try: result.order_try,
        order_id: result.order_id.as_ref().map(|id| id.as_str().to_owned()),
        forced: false,
        detail: None,
        log_tail: Vec::new(),
    }
}

/// A health by its name.
fn health(health: wire::Health) -> agent::Health {
    match health {
        wire::Health::Healthy => agent::Health::Healthy,
        wire::Health::Unhealthy => agent::Health::Unhealthy,
        wire::Health::Absent => agent::Health::Absent,
    }
}

/// How an update ended, by name.
fn outcome(outcome: wire::Outcome) -> agent::Outcome {
    match outcome {
        wire::Outcome::Succeeded => agent::Outcome::Succeeded,
        wire::Outcome::Failed => agent::Outcome::Failed,
    }
}

/// Where a failure happened; unspecified when the host did not say.
fn phase(phase: Option<wire::Phase>) -> agent::FailurePhase {
    match phase {
        None => agent::FailurePhase::Unspecified,
        Some(wire::Phase::BeforeCommit) => agent::FailurePhase::BeforeCommit,
        Some(wire::Phase::GateRelease) => agent::FailurePhase::GateRelease,
        Some(wire::Phase::GateEnvironment) => agent::FailurePhase::GateEnvironment,
        Some(wire::Phase::AfterCommit) => agent::FailurePhase::AfterCommit,
        Some(wire::Phase::Halt) => agent::FailurePhase::Halt,
    }
}

/// Whom a failure is blamed on; unspecified when the host did not say.
fn class(class: Option<wire::Class>) -> agent::FailureClass {
    match class {
        None => agent::FailureClass::Unspecified,
        Some(wire::Class::Release) => agent::FailureClass::Release,
        Some(wire::Class::Environment) => agent::FailureClass::Environment,
    }
}

/// Whether the running update is past the commit point; unspecified when the host did not say.
fn commit_state(state: Option<wire::CommitState>) -> agent::CommitState {
    match state {
        None => agent::CommitState::Unspecified,
        Some(wire::CommitState::Uncommitted) => agent::CommitState::Uncommitted,
        Some(wire::CommitState::Committed) => agent::CommitState::Committed,
    }
}

/// Why the rollout gate holds a host back: `none` when nothing does. A gate the proto has no
/// spelling for (`below_min_version`) is unspecified.
fn gate_reason(hold: Option<Why>) -> proto::GateReason {
    match hold {
        None => proto::GateReason::None,
        Some(Why::Gate(gate)) => gate_of(gate),
        Some(Why::Skip(skip)) => skip_of(skip),
    }
}

/// A closed gate by name.
fn gate_of(gate: Gate) -> proto::GateReason {
    match gate {
        Gate::Halted => proto::GateReason::Halted,
        Gate::Held => proto::GateReason::Held,
        Gate::Bad => proto::GateReason::Bad,
        Gate::BelowMinVersion => proto::GateReason::Unspecified,
        Gate::Updating => proto::GateReason::Updating,
        Gate::BusyElsewhere => proto::GateReason::BusyElsewhere,
        Gate::NoSlot => proto::GateReason::NoSlot,
        Gate::Quiet => proto::GateReason::Quiet,
        Gate::CanaryWait => proto::GateReason::CanaryWait,
        Gate::Baking => proto::GateReason::Baking,
    }
}

/// A question that does not apply, by name.
fn skip_of(skip: Skip) -> proto::GateReason {
    match skip {
        Skip::NoDefinition => proto::GateReason::NoDefinition,
        Skip::NoDesired => proto::GateReason::NoDesired,
        Skip::NotAMember => proto::GateReason::NotAMember,
        Skip::Absent => proto::GateReason::Absent,
        Skip::UpToDate => proto::GateReason::UpToDate,
        Skip::FailedHere => proto::GateReason::FailedHere,
        Skip::Deferred => proto::GateReason::Deferred,
    }
}

/// Whether the service counts a host: `present` for a host the service's members list does not
/// name.
fn membership(member: Option<Membership>) -> proto::Membership {
    match member {
        None | Some(Membership::Present) => proto::Membership::Present,
        Some(Membership::Excluded) => proto::Membership::Excluded,
        Some(Membership::NotSelected) => proto::Membership::NotSelected,
        Some(Membership::Down) => proto::Membership::Down,
        Some(Membership::Absent) => proto::Membership::Absent,
        Some(Membership::Missing) => proto::Membership::Missing,
        Some(Membership::Unmanaged) => proto::Membership::Unmanaged,
    }
}
