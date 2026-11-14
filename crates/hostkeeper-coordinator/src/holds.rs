//! Holds: an operator's mark that freezes a service where it is, on one host, on every host of a
//! role or on the whole fleet (design 5.4, **Holds**).
//!
//! A hold carries no version. The coordinator orders a held pair (a host and a service) nothing:
//! the rollout gate holds it at [`crate::decide::Gate::Held`] before its other rules, a force
//! skips it unless the call says `include_held` ([`crate::force`]), and an automatic rollback or a
//! version marked bad does not reach it. A held pair takes no update slot, is in no stage of the
//! service's update plan ([`crate::stages`]: it takes no place in a stage and does not count
//! towards one being done), never holds back a bake and is left out of the convergence and of the
//! hosts a rollback is counted against; an update it was already running, or an order it was
//! given, when the hold was set still counts as live until it ends. The host never learns of the hold.
//!
//! The holds are kept in the ops file ([`crate::ops::OpsState::holds`]), so they survive a
//! restart. A role hold is matched against the host's role at the time of each decision, so a
//! host that joins the role later is held and one that leaves it is released. The fleet keeps a
//! copy of the holds for its decisions, refreshed under its lock ([`crate::fleet::Fleet::is_held`]).
//!
//! A held host that runs a version marked bad is an alert ([`crate::alerts`]: an error-level line
//! with `alert = "held_on_bad_version"` once when it appears), and the status flags it.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

use crate::fleet::{Fleet, Marks};
use crate::ops::OpsState;
use crate::rollouts::Rollout;

/// The most holds the ops file keeps. Raise it here if a farm needs more.
pub const MAX_HOLDS: usize = 4096;

/// Where a hold applies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldScope {
    /// One host, by node id.
    Host(String),
    /// Every host whose role is this one when the coordinator decides, those that join it later
    /// included.
    Role(String),
    /// Every host.
    Fleet,
}

impl HoldScope {
    /// Whether the scope covers the host `node_id`, whose role is `role` now.
    #[must_use]
    pub fn covers(&self, node_id: &str, role: &str) -> bool {
        match self {
            Self::Host(id) => id == node_id,
            Self::Role(name) => name == role,
            Self::Fleet => true,
        }
    }

    /// The scope as the audit says it: `host d1`, `role storage` or `fleet`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Host(id) => format!("host {id}"),
            Self::Role(name) => format!("role {name}"),
            Self::Fleet => "fleet".to_owned(),
        }
    }
}

/// An operator's hold of a service in a scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldMark {
    /// The service held.
    pub service: String,
    /// Where.
    pub scope: HoldScope,
    /// Why, in the operator's words.
    pub reason: String,
    /// The name of the operator's token that set it.
    pub by: String,
    /// When it was set, in seconds since the epoch, for the people who read it; no decision
    /// reads it.
    pub at_unix: u64,
}

/// The holds of `holds` that hold `service` on the host `node_id`, whose role is `role` now.
pub fn holding<'a>(
    holds: &'a [HoldMark],
    service: &'a str,
    node_id: &'a str,
    role: &'a str,
) -> impl Iterator<Item = &'a HoldMark> + 'a {
    holds
        .iter()
        .filter(move |hold| hold.service == service && hold.scope.covers(node_id, role))
}

/// What the fleet keeps of the ops state for its decisions: the holds, the hosts each standing
/// force orders ([`crate::force`]), the assignments ([`crate::assignment`]), the labels, the
/// exclusions, where each update plan stands ([`crate::rollouts`]), and the services halted or
/// with versions marked bad, which a plan reads of the other services.
#[must_use]
pub fn marks_of(state: &OpsState) -> Marks {
    let forced = state
        .services
        .iter()
        .filter_map(|(name, marks)| {
            let force = marks.force.as_ref()?;
            let hosts: BTreeSet<HostId> = force
                .hosts
                .iter()
                .filter_map(|host| host.parse::<HostId>().ok())
                .collect();
            Some((name.clone(), hosts))
        })
        .collect();
    let mut group_rollouts: BTreeMap<String, BTreeMap<String, Rollout>> = BTreeMap::new();
    let mut group_halted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut group_forced: BTreeMap<String, BTreeMap<String, BTreeSet<HostId>>> = BTreeMap::new();
    for (group, packages) in &state.host_groups.0 {
        for (package, marks) in packages {
            let name = group.to_string();
            if !marks.rollout.is_empty() {
                group_rollouts
                    .entry(name.clone())
                    .or_default()
                    .insert(package.clone(), marks.rollout.clone());
            }
            if marks.halt.is_some() {
                group_halted
                    .entry(name.clone())
                    .or_default()
                    .insert(package.clone());
            }
            if let Some(force) = &marks.force {
                let hosts = force.hosts.iter().filter_map(|host| host.parse().ok());
                group_forced
                    .entry(name)
                    .or_default()
                    .insert(package.clone(), hosts.collect());
            }
        }
    }
    Marks {
        group_rollouts,
        group_halted,
        group_forced,
        group_mode: false,
        holds: state.holds.clone(),
        forced,
        assignments: state.assignments.clone(),
        labels: state
            .hosts
            .iter()
            .map(|(id, host)| (id.clone(), host.labels.clone()))
            .collect(),
        excluded: state
            .excluded
            .iter()
            .map(|e| (e.service.clone(), e.node_id.clone()))
            .collect(),
        rollouts: state.rollouts.clone(),
        halted: state
            .services
            .iter()
            .filter(|(_, marks)| marks.halt.is_some())
            .map(|(name, _)| name.clone())
            .collect(),
        bad: state
            .services
            .iter()
            .filter(|(_, marks)| !marks.bad.is_empty())
            .map(|(name, marks)| (name.clone(), marks.bad.clone()))
            .collect(),
        acked: state
            .services
            .iter()
            .map(|(name, marks)| {
                let halts = marks
                    .halt_acked
                    .iter()
                    .map(|ack| (ack.node_id.clone(), ack.target.as_str().to_owned()));
                let failures = marks
                    .failure_acked
                    .iter()
                    .map(|ack| (ack.node_id.clone(), ack.report.target.as_str().to_owned()));
                (name.clone(), halts.chain(failures).collect::<BTreeSet<_>>())
            })
            .filter(|(_, acked)| !acked.is_empty())
            .collect(),
    }
}

/// The version of `service` the host `id` runs, when an operator holds the pair and the version
/// is marked bad.
pub(crate) fn bad_version(
    fleet: &Fleet,
    state: &OpsState,
    service: &str,
    id: &HostId,
) -> Option<Version> {
    let version = &fleet.get_in(service, id)?.beat.version;
    let bad = state
        .services
        .get(service)
        .is_some_and(|marks| marks.bad.contains(version));
    (bad && fleet.is_held(service, id)).then(|| version.clone())
}

/// Whether the host `id`, held for `service`, runs a version of it that is marked bad.
#[must_use]
pub fn on_bad_version(fleet: &Fleet, state: &OpsState, service: &str, id: &HostId) -> bool {
    bad_version(fleet, state, service, id).is_some()
}
