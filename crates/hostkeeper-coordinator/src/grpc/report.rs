//! The status and the convergence as `hostkeeper.operator.v1` says them: the coordinator's report
//! types ([`crate::status::Status`], [`crate::converge::Convergence`]) mapped field by field to
//! the messages of `Releases.Status` and `Releases.Converge`, and one host of the status as
//! `Hosts.List` and `Hosts.Get` answer it ([`host`]).
//!
//! The mapping decides nothing. A fact with no field in the proto is left out (the `hold` of a
//! host that the rollout gate has no spelling for, the legacy service's top-level status, the
//! flattened heartbeat's platform in `Releases.Status`); a proto field with no source here stays at its default (the
//! write record of a service, the pinned count of a role). A standing force is in the marks of
//! the ops state, so [`status`] puts it into the convergence of its service; [`convergence`]
//! alone has no force to name.

use std::collections::HashMap;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{self, Heartbeat};
use hostkeeper_wire::{HostId, Platform};

use crate::converge::{Convergence, HaltNote, PickCounts, RoleCounts, State};
use crate::decide::{Gate, Skip, Why};
use crate::desired::{Desired, RoleLimits};
use crate::fleet::Node;
use crate::force::ForceRecord;
use crate::holds::holding;
use crate::host_groups::GroupName;
use crate::membership::{HostCounts, Member, Membership};
use crate::operator::{entry_proto, hold_proto};
use crate::ops::{FailureAck, Halt, HaltAck, OpsState, ServiceOps};
use crate::status::{ServiceHostStatus, ServiceStatus, Status, UnheardHost};

/// A count as the proto's `uint32`: the number itself, or the largest `uint32` when it does not
/// fit.
fn count<T: TryInto<u32>>(n: T) -> u32 {
    n.try_into().unwrap_or(u32::MAX)
}

/// The status as `Releases.Status` answers it: every service in name order, or only `only` when
/// it is not empty (none when the coordinator manages no such service). A coordinator that holds
/// host groups answers one service for each package of each group, in the order of the group's
/// name and then the package's, `host_group` naming the group; `only` names a package then.
pub(crate) fn status(status: &Status, only: &str) -> proto::StatusResponse {
    let wanted = |name: &str| only.is_empty() || name == only;
    let services = match &status.groups {
        None => status
            .per_service
            .iter()
            .filter(|(name, _)| wanted(name))
            .map(|(name, one)| service_status(name, None, one, &status.ops))
            .collect(),
        Some(groups) => groups
            .iter()
            .filter(|((_, package), _)| wanted(package))
            .map(|((group, package), one)| service_status(package, Some(group), one, &status.ops))
            .collect(),
    };
    proto::StatusResponse {
        services,
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
/// convergence report holds neither, and `Converge` adds the write record of the desired-state
/// store while the status adds the force.
pub(crate) fn convergence(convergence: &Convergence) -> proto::Convergence {
    proto::Convergence {
        desired: convergence.desired.as_ref().map(|v| v.as_str().to_owned()),
        quiet: convergence.quiet,
        state: convergence_state(convergence.state).into(),
        roles: convergence.roles.iter().map(role_counts).collect(),
        halt: convergence.halt.as_ref().map(halt_note),
        hosts: host_counts(convergence.hosts.as_ref(), convergence.picks.as_ref()),
        coordinator_commit: convergence.commit.to_owned(),
        record: None,
        force: None,
        installer_dropped: convergence
            .picks
            .as_ref()
            .map(|picks| picks.dropped.clone())
            .unwrap_or_default(),
    }
}

/// One service of the status, with the ops state's marks and the force that stands: those of the
/// package in `group`, for a service made for one.
fn service_status(
    name: &str,
    group: Option<&GroupName>,
    one: &ServiceStatus,
    ops: &OpsState,
) -> proto::ServiceStatus {
    let own;
    let marks = match group {
        None => ops.services.get(name),
        Some(group) => {
            own = ops.marks_in(Some(group), name);
            Some(&own)
        }
    };
    let mut converged = convergence(&one.convergence);
    converged.force = marks.and_then(|m| m.force.as_ref()).map(force_record);
    proto::ServiceStatus {
        service: name.to_owned(),
        desired: one.desired.as_ref().map(desired_state),
        convergence: Some(converged),
        hosts: hosts(name, one, ops),
        up: count(one.up),
        slots_held: count(one.slots_held),
        marks: marks.map(service_marks),
        plan: one.plan.clone(),
        host_group: group.map(ToString::to_string).unwrap_or_default(),
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
        release_index_digest: desired
            .release_index
            .as_ref()
            .map(|record| record.digest.as_str().to_owned()),
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

/// The hosts a service does not count, by why, with the picks of a version set with targets;
/// none when there are neither.
fn host_counts(
    counts: Option<&HostCounts>,
    picks: Option<&PickCounts>,
) -> Option<proto::HostCounts> {
    if counts.is_none() && picks.is_none() {
        return None;
    }
    let (counts, picks) = (
        counts.copied().unwrap_or_default(),
        picks.cloned().unwrap_or_default(),
    );
    Some(proto::HostCounts {
        missing: count(counts.missing),
        absent: count(counts.absent),
        excluded: count(counts.excluded),
        unmanaged: count(counts.unmanaged),
        deferred: count(counts.deferred),
        picked: count(picks.picked),
        no_target: count(picks.no_target),
        no_build_for_host: count(picks.no_build_for_host),
        installer_dropped: count(picks.installer_dropped),
    })
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

/// The hosts of `service`, whose status is `one`, by node id: each host it heard, and each node it
/// places and has no heartbeat of ([`unheard_host`]), so that a package no host of its group runs
/// yet shows every host of the group and why it is not ordered.
fn hosts(service: &str, one: &ServiceStatus, ops: &OpsState) -> Vec<proto::ServiceHost> {
    let mut hosts: Vec<proto::ServiceHost> = one
        .hosts
        .iter()
        .map(|host| service_host(service, host, one, ops))
        .chain(
            one.unheard
                .iter()
                .map(|host| unheard_host(service, host, one, ops)),
        )
        .collect();
    hosts.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    hosts
}

/// A node `service` places and has no heartbeat of, as the service sees it: where it stands
/// (`missing` while it is to install the service, `excluded`, `down`, `unmanaged`) and the gate's
/// reason, such as `no_build_for_host` for a host the desired version's release has no entry it can
/// install for. Nothing the host did not say of the service is said: no beat, version, health or
/// update, it is not up and holds no slot, and it has a stage only when the plan counts it.
fn unheard_host(
    service: &str,
    host: &UnheardHost,
    one: &ServiceStatus,
    ops: &OpsState,
) -> proto::ServiceHost {
    let node_id = host.node_id.as_str();
    proto::ServiceHost {
        node_id: host.node_id.clone(),
        role: host.role.clone(),
        live: host.live,
        excluded_reason: host.excluded_reason.clone(),
        view: Some(proto::HostService {
            service: service.to_owned(),
            gate_reason: gate_reason(host.hold).into(),
            membership: membership(Some(host.membership)).into(),
            holds: holding(&ops.holds, service, node_id, &host.role)
                .map(hold_proto)
                .collect(),
            excluded_reason: host.excluded_reason.clone(),
            stage: one.plan.as_ref().and_then(|plan| stage_of(plan, node_id)),
            ..proto::HostService::default()
        }),
        beat: None,
    }
}

/// One host as `service` sees it. `members` are the nodes the service does not count; a host
/// not among them is present.
fn service_host(
    service: &str,
    host: &ServiceHostStatus,
    one: &ServiceStatus,
    ops: &OpsState,
) -> proto::ServiceHost {
    let view = host_service(service, host, one, ops);
    proto::ServiceHost {
        node_id: host.host.node_id.clone(),
        role: host.host.role.clone(),
        live: host.host.live,
        excluded_reason: view.excluded_reason.clone(),
        view: Some(view),
        beat: Some(service_beat(service, &host.host.beat)),
    }
}

/// How `service`, whose status is `one`, sees a host it heard: the members of `one` are the nodes
/// the service does not count, and a host not among them is present; the stage is the one of
/// the service's plan that takes the host.
fn host_service(
    service: &str,
    host: &ServiceHostStatus,
    one: &ServiceStatus,
    ops: &OpsState,
) -> proto::HostService {
    let node_id = host.host.node_id.as_str();
    let role = host.host.role.as_str();
    let member = one.members.iter().find(|m| m.node_id == node_id);
    let beat = &host.host.beat;
    proto::HostService {
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
        excluded_reason: member.and_then(|m| m.reason.clone()),
        stage: one.plan.as_ref().and_then(|plan| stage_of(plan, node_id)),
    }
}

/// The stage of `plan` that takes the host `node_id`, if the plan counts it.
fn stage_of(plan: &proto::RolloutPlan, node_id: &str) -> Option<u32> {
    plan.stages
        .iter()
        .find(|stage| stage.members.iter().any(|m| m.node_id == node_id))
        .map(|stage| stage.number)
}

/// How `service` sees a host it has no heartbeat of, but does not count for a reason
/// (`member`): excluded, absent, missing or unmanaged. Nothing it did not hear is said: no
/// health, version or update, and the gate's reason is unspecified, as the gate is asked only
/// of a host the service heard.
fn member_service(service: &str, member: &Member, ops: &OpsState) -> proto::HostService {
    proto::HostService {
        service: service.to_owned(),
        membership: membership(Some(member.membership)).into(),
        holds: holding(&ops.holds, service, &member.node_id, &member.role)
            .map(hold_proto)
            .collect(),
        excluded_reason: member.reason.clone(),
        ..proto::HostService::default()
    }
}

/// The host `id` of the node `node` as `Hosts.List` and `Hosts.Get` answer it, from `status`
/// at `now`, when the ops store's wall clock reads `unix_now`.
///
/// Its services are every service of the status that heard it or does not count it (in name
/// order); a service whose selector does not take it is not among them. Its last heartbeat is
/// put back together from what the coordinator keeps: the platform of the first service that
/// heard it, its agent and features, and one entry per service that heard it (each as
/// [`service_beat`] says it). The coordinator's clock is not the wall clock, so the time of the
/// last heartbeat is `unix_now` less the seconds the host has been silent. What the last
/// heartbeat said of the host's clock, certificate and trusted root is in the host's own fields
/// (`cert_not_after_unix`, `last_renew_error`, `clock_skew_beats`, `trusted_root_age_secs`,
/// `trusted_root_reason`), not in the heartbeat put back together.
pub(crate) fn host(
    id: &HostId,
    node: &Node,
    status: &Status,
    now: Duration,
    unix_now: u64,
) -> proto::Host {
    let node_id = id.to_string();
    let mut services = Vec::new();
    let mut beats = Vec::new();
    let mut platform = None;
    // The packages of the host groups first (a host is in one group, so each is shown once), then
    // the services of the coordinator's own definition that no group's entry showed already.
    let grouped = status
        .groups
        .iter()
        .flatten()
        .map(|((_, package), one)| (package, one));
    let mut shown = std::collections::BTreeSet::new();
    for (name, one) in grouped.chain(&status.per_service) {
        if shown.contains(name) {
            continue;
        }
        if let Some(heard) = one.hosts.iter().find(|h| h.host.node_id == node_id) {
            services.push(host_service(name, heard, one, &status.ops));
            beats.push(service_beat(name, &heard.host.beat));
            platform = platform.or(Some(heard.host.beat.platform));
            shown.insert(name);
        } else if let Some(member) = one.members.iter().find(|m| m.node_id == node_id) {
            services.push(member_service(name, member, &status.ops));
            shown.insert(name);
        }
    }
    services.sort_by(|a, b| a.service.cmp(&b.service));
    beats.sort_by(|a, b| a.name.cmp(&b.name));
    let silent = now.saturating_sub(node.seen).as_secs();
    proto::Host {
        live: node.is_live(now),
        last_heartbeat_unix: unix_now.saturating_sub(silent),
        heartbeat: Some(agent::HeartbeatRequest {
            platform: platform
                .map_or(agent::Platform::Unspecified, platform_of)
                .into(),
            agent: node.agent.as_ref().map(|said| agent::AgentInfo {
                name: said.name.as_str().to_owned(),
                version: said.version.as_str().to_owned(),
            }),
            features: node.features.clone(),
            services: beats,
            ..agent::HeartbeatRequest::default()
        }),
        labels: status
            .ops
            .hosts
            .get(&node_id)
            .map(|host| {
                host.labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        exclusions: status
            .ops
            .excluded
            .iter()
            .filter(|e| e.node_id == node_id)
            .map(|e| proto::Exclusion {
                service: e.service.clone(),
                reason: e.reason.clone(),
            })
            .collect(),
        services,
        cert_not_after_unix: node.cert_not_after_unix,
        last_renew_error: node.last_renew_error.clone(),
        clock_skew_beats: node.skewed_beats,
        trusted_root_age_secs: node.trusted_root.age_secs,
        trusted_root_reason: node.trusted_root.reason.into(),
        role: node.role.clone(),
        node_id,
        // `Hosts.List` and `Hosts.Get` fill these in from the host inventory.
        ..proto::Host::default()
    }
}

/// A platform by its name.
fn platform_of(platform: Platform) -> agent::Platform {
    match platform {
        Platform::LinuxX86_64 => agent::Platform::LinuxX8664,
        Platform::LinuxArm64 => agent::Platform::LinuxArm64,
        Platform::DarwinArm64 => agent::Platform::DarwinArm64,
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
        record_try: beat.record_try,
        // `Hosts.List` and `Hosts.Get` fill it in from the fleet ([`crate::picks::show`]).
        installer: None,
        // `Hosts.List` and `Hosts.Get` fill it in from the fleet ([`crate::picks::show`]).
        drift: None,
    }
}

/// A verdict of the host. The proto's start time, `forced` flag, detail and log lines have no
/// source on the heartbeat wire.
pub(crate) fn update_result(result: &wire::UpdateResult) -> agent::UpdateResult {
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

/// Why the rollout gate holds a host back: `none` when nothing does. A gate or a skip the proto
/// has no spelling for (`below_min_version`, `unsupported`, `no_digest`) is unspecified.
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
        Gate::Paused => proto::GateReason::Paused,
        Gate::AdvanceWait => proto::GateReason::AdvanceWait,
        Gate::StageFailed => proto::GateReason::StageFailed,
        Gate::StageWait => proto::GateReason::StageWait,
        Gate::BelowMinUp => proto::GateReason::BelowMinUp,
        Gate::RolloutElsewhere => proto::GateReason::RolloutElsewhere,
        Gate::InstallOrder => proto::GateReason::InstallOrder,
        Gate::UninstallOrder => proto::GateReason::UninstallOrder,
        Gate::Moving => proto::GateReason::Moving,
    }
}

/// A question that does not apply, by name.
fn skip_of(skip: Skip) -> proto::GateReason {
    match skip {
        Skip::NoDesired => proto::GateReason::NoDesired,
        Skip::NotAMember => proto::GateReason::NotAMember,
        Skip::UpToDate => proto::GateReason::UpToDate,
        Skip::FailedHere => proto::GateReason::FailedHere,
        Skip::Deferred => proto::GateReason::Deferred,
        Skip::NoPackageForPlatform => proto::GateReason::NoPackageForPlatform,
        Skip::NoTarget => proto::GateReason::NoTarget,
        Skip::NoBuildForHost => proto::GateReason::NoBuildForHost,
        Skip::InstallerDropped => proto::GateReason::InstallerDropped,
        Skip::Unsupported | Skip::NoDigest => proto::GateReason::Unspecified,
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
