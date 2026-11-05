//! Holds: an operator's mark that freezes a service where it is, on one host, on every host of a
//! role or on the whole fleet (design 5.4, **Holds**).
//!
//! A hold carries no version. The coordinator orders a held pair (a host and a service) nothing:
//! the rollout gate holds it at [`crate::decide::Gate::Held`] before its other rules, a force
//! skips it unless the call says `include_held` ([`crate::force`]), and an automatic rollback or a
//! version marked bad does not reach it. A held pair takes no update slot, is never a canary,
//! never holds back a bake and is left out of the convergence and of the hosts a rollback is
//! counted against; an update it was already running, or an order it was given, when the hold
//! was set still counts as live until it ends. The host never learns of the hold.
//!
//! The holds are kept in the ops file ([`crate::ops::OpsState::holds`]), so they survive a
//! restart. A role hold is matched against the host's role at the time of each decision, so a
//! host that joins the role later is held and one that leaves it is released. The fleet keeps a
//! copy of the holds for its decisions, refreshed under its lock ([`crate::fleet::Fleet::is_held`]).
//!
//! A held host that runs a version marked bad is an alert: an error-level line with
//! `alert = "held_on_bad_version"`, once per host, service and version ([`alert_on_bad_version`]),
//! and the status flags it.

use std::collections::BTreeSet;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

use crate::fleet::{Fleet, Marks};
use crate::ops::OpsState;

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
/// force orders ([`crate::force`]), the assignments ([`crate::assignment`]), the labels and the
/// exclusions.
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
    Marks {
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
    }
}

/// The version of `service` the host `id` runs, when an operator holds the pair and the version
/// is marked bad.
fn bad_version(fleet: &Fleet, state: &OpsState, service: &str, id: &HostId) -> Option<Version> {
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

/// Raises the alert of a held host that runs a version of `service` marked bad, once per host,
/// service and version: an error-level line with `alert = "held_on_bad_version"`. Nothing is
/// ordered: a hold is not reached by a bad mark, and the operator decides.
pub fn alert_on_bad_version(fleet: &mut Fleet, state: &OpsState, service: &str, id: &HostId) {
    if let Some(version) = bad_version(fleet, state, service, id)
        && fleet.first_alert(service, id, &version)
    {
        tracing::error!(
            alert = "held_on_bad_version",
            service,
            node_id = %id,
            version = %version,
            "an operator holds a host that runs a version marked bad: it is not ordered off it until the hold is released"
        );
    }
}
