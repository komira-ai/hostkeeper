//! What a `Releases.WaitConverged` answer that did not end converged says the fleet still waits
//! on (`NotYet`, waits.proto): each host of the pairs the wait reads that is not settled on the
//! version, and why each pair's update plan orders no further host.
//!
//! It is read from the status at the moment of the answer ([`Coordinator::status_now`]), the same
//! read `Releases.Status` makes, and only for the pairs the wait reads: the service of the rollout
//! file, the one group the call names, or, with none named, every group that lists the package
//! and has a desired version of it. So a CI token that may wait on a service reads the hosts of
//! that service and no other.
//!
//! A host is listed, in the first state of these that holds:
//!
//! - EXCLUDED: an operator excluded it, and it is not on the version (an excluded host on it says
//!   nothing a reader needs);
//! - SILENT: not live; since its last heartbeat, when the service heard one;
//! - UPDATING: an update of the service runs on it, to `updating_to`, in `step`;
//! - SETTLING: on the version, and its promotion is pending or its plan has it baking: the two
//!   things besides the version that keep a convergence from `CONVERGED` ([`crate::converge`]);
//!   a host on the version that is neither is not listed;
//! - FAILED: its last update to the version failed, with that update's reason and start;
//! - BEHIND: anything else, with the rollout gate's reason (`no_slot`, `busy_elsewhere`), or,
//!   for a node the service has no heartbeat of, where it stands for the service (`missing`).
//!
//! At most [`MAX_HOSTS`] are listed, by the pairs' group order and then node id; the rest are
//! counted.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::report;
use crate::group_converge::Scope;
use crate::host_groups::GroupName;
use crate::ops::OpsState;
use crate::service::Coordinator;
use crate::status::ServiceStatus;

/// The most hosts one answer lists.
pub(crate) const MAX_HOSTS: usize = 64;

/// One pair a wait reads: its group (none without host groups) and its status.
struct Pair<'a> {
    group: Option<&'a GroupName>,
    one: &'a ServiceStatus,
}

impl Coordinator {
    /// What a wait for `version` of `service` in `host_group` (empty: every group that lists it)
    /// still waits on, now.
    pub(crate) fn not_yet(&self, service: &str, host_group: &str, version: &str) -> proto::NotYet {
        let status = self.status_now();
        let unix_now = self.ops.unix_now();
        let grouped = status.groups.iter().flatten();
        let pairs: Vec<Pair<'_>> = match self.scope_of(host_group, service) {
            Ok(Scope::Service) => status
                .per_service
                .get(service)
                .map(|one| Pair { group: None, one })
                .into_iter()
                .collect(),
            Ok(Scope::One(only)) => grouped
                .filter(|((group, package), _)| *group == only && package == service)
                .map(|((group, _), one)| Pair {
                    group: Some(group),
                    one,
                })
                .collect(),
            Ok(Scope::All) => grouped
                .filter(|((_, package), one)| package == service && one.desired.is_some())
                .map(|((group, _), one)| Pair {
                    group: Some(group),
                    one,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        not_yet(service, version, &pairs, &status.ops, unix_now)
    }
}

/// What a wait for `version` of `service` still waits on in `pairs`, answered when the store's
/// clock reads `unix_now`.
fn not_yet(
    service: &str,
    version: &str,
    pairs: &[Pair<'_>],
    ops: &OpsState,
    unix_now: u64,
) -> proto::NotYet {
    let mut hosts = Vec::new();
    let mut plans = Vec::new();
    for pair in pairs {
        let silent: BTreeMap<&str, u64> = pair
            .one
            .hosts
            .iter()
            .map(|h| (h.host.node_id.as_str(), h.host.silent_secs))
            .collect();
        let baking: BTreeSet<&str> = pair
            .one
            .plan
            .iter()
            .flat_map(|plan| &plan.stages)
            .flat_map(|stage| &stage.members)
            .filter(|member| member.state() == proto::PlanHostState::Baking)
            .map(|member| member.node_id.as_str())
            .collect();
        for host in report::hosts(service, pair.one, ops) {
            let read = Read {
                version,
                silent_secs: silent.get(host.node_id.as_str()).copied(),
                baking: baking.contains(host.node_id.as_str()),
                unix_now,
            };
            hosts.extend(waiting_host(host, &read));
        }
        if let Some(plan) = &pair.one.plan {
            plans.push(proto::WaitingPlan {
                host_group: pair
                    .group
                    .map_or_else(String::new, |g| g.as_str().to_owned()),
                waiting: plan.waiting,
                waiting_on: plan.waiting_on.clone(),
                pause_reason: plan.pause_reason.clone(),
            });
        }
    }
    let (hosts, hosts_left_out) = cap(hosts);
    proto::NotYet {
        hosts,
        hosts_left_out,
        plans,
        answered_unix: unix_now,
    }
}

/// The first [`MAX_HOSTS`] of `hosts`, and how many are left out past them.
fn cap(mut hosts: Vec<proto::WaitingHost>) -> (Vec<proto::WaitingHost>, u32) {
    let left_out = hosts.len().saturating_sub(MAX_HOSTS);
    hosts.truncate(MAX_HOSTS);
    (hosts, u32::try_from(left_out).unwrap_or(u32::MAX))
}

/// What the listing of one host reads besides the host's row.
struct Read<'a> {
    /// The version waited for.
    version: &'a str,
    /// How long the host has been silent, when the service heard it.
    silent_secs: Option<u64>,
    /// Its plan has it baking.
    baking: bool,
    /// The store's clock.
    unix_now: u64,
}

/// `name` of a proto enum value, less its `prefix`, in lower case: `GATE_REASON_NO_SLOT` is
/// `no_slot`.
fn token(name: &str, prefix: &str) -> String {
    name.strip_prefix(prefix)
        .unwrap_or(name)
        .to_ascii_lowercase()
}

/// Why a live host that is not on the version and not failed is not ordered: the gate's reason,
/// or, when the gate says none and the host is not counted as present, where it stands.
fn behind(view: &proto::HostService) -> String {
    let gate = proto::GateReason::try_from(view.gate_reason).unwrap_or_default();
    let membership = proto::Membership::try_from(view.membership).unwrap_or_default();
    match (gate, membership) {
        (
            proto::GateReason::Unspecified | proto::GateReason::None,
            proto::Membership::Absent | proto::Membership::Missing | proto::Membership::Unmanaged,
        ) => token(membership.as_str_name(), "MEMBERSHIP_"),
        _ => token(gate.as_str_name(), "GATE_REASON_"),
    }
}

/// The last update of the host to `version`, when it failed.
fn failed_to<'a>(
    version: &str,
    view: &'a proto::HostService,
    beat: Option<&'a agent::ServiceBeat>,
) -> Option<&'a agent::UpdateResult> {
    view.last_update
        .as_ref()
        .or_else(|| beat.and_then(|b| b.last_update.as_ref()))
        .filter(|update| update.target == version && update.outcome() == agent::Outcome::Failed)
}

/// `host` as a wait for `read.version` lists it, or `None` when it is settled on the version.
fn waiting_host(host: proto::ServiceHost, read: &Read<'_>) -> Option<proto::WaitingHost> {
    let view = host.view.unwrap_or_default();
    let beat = host.beat.as_ref();
    let on_version = view.installed_version.as_deref() == Some(read.version);
    let mut waiting = proto::WaitingHost {
        node_id: host.node_id,
        role: host.role,
        installed_version: view.installed_version.clone(),
        ..proto::WaitingHost::default()
    };
    let (state, reason, since) = if let Some(reason) = host.excluded_reason {
        if on_version {
            return None;
        }
        (proto::WaitingState::Excluded, reason, None)
    } else if !host.live {
        let since = read
            .silent_secs
            .map(|secs| read.unix_now.saturating_sub(secs));
        (proto::WaitingState::Silent, String::new(), since)
    } else if let Some(target) = beat.and_then(|b| b.updating.clone()) {
        waiting.updating_to = Some(target);
        if let Some(step) = beat.and_then(|b| b.step.as_ref()) {
            waiting.step = Some(step.name.clone());
            waiting.step_attempt = step.attempt;
        }
        (proto::WaitingState::Updating, String::new(), None)
    } else if on_version {
        let reason = if beat.is_some_and(|b| b.promotion_pending) {
            "promotion_pending"
        } else if read.baking {
            "baking"
        } else {
            return None;
        };
        (proto::WaitingState::Settling, reason.to_owned(), None)
    } else if let Some(failed) = failed_to(read.version, &view, beat) {
        (
            proto::WaitingState::Failed,
            failed.reason.clone(),
            failed.started_unix,
        )
    } else {
        (proto::WaitingState::Behind, behind(&view), None)
    };
    waiting.state = state.into();
    waiting.reason = reason;
    waiting.since_unix = since;
    Some(waiting)
}

#[cfg(test)]
#[path = "not_yet_tests.rs"]
mod tests;
