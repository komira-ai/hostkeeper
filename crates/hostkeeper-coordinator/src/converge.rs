//! Whether the farm has reached the desired version: the answer CI waits for after a
//! release is published.
//!
//! A pure function of the desired state, the fleet the heartbeats built and the time; it
//! stores nothing. A host counts only while it is live (heartbeated within
//! [`crate::fleet::DOWN_AFTER`]); a silent host is `down`, and is not at the desired
//! version whatever it last said.
//!
//! The state is one of four:
//!
//! - `blocked`: some live host failed an update to the desired version and is still behind
//!   it. It stays so until the host is fixed or another version is set (the coordinator
//!   does not tell it again, `decide`).
//! - `converged`: past the quiet start, at least one live host, every live host at the
//!   desired version, and none updating, failed, still baking or waiting for a promotion.
//!   The last two keep a coordinator that has just restarted, and has heard from few hosts,
//!   from reporting a farm converged. The convergence reads no `min_up`; the update plan does
//!   ([`crate::stages`]).
//! - `halted`: a host reported a failure no version can repair and an operator has not
//!   cleared it. It wins over every other state, and the convergence names the halt
//!   (`halt`: the host, the check and the version); see [`crate::halt`].
//! - `in_progress`: everything else.
//!
//! A host whose platform the desired version's release index lists no package for is not ordered
//! the version ([`crate::decide::Skip::NoPackageForPlatform`]) and could never reach it: it is
//! left out of every count, as a pair an operator holds is, so it neither blocks the others nor
//! keeps the service from converging.
//!
//! How long a farm has gone without progress is the caller's to judge: it needs the time of
//! the last change, which the coordinator does not keep.

use std::collections::BTreeSet;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::Outcome;
use serde::Serialize;

use crate::desired::Desired;
use crate::fleet::{Fleet, Host};
use crate::membership::HostCounts;
use crate::ops::Halt;
use crate::services::ServiceDef;

/// Where the farm stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Every live host runs the desired version, and none is updating, failed, baking or
    /// waiting for a promotion.
    Converged,
    /// A live host failed its update to the desired version.
    Blocked,
    /// A host reported a failure no version can repair and an operator has not cleared it
    /// ([`crate::halt`]): nothing is ordered, whatever the other hosts say.
    Halted,
    /// Neither.
    InProgress,
}

/// The hosts of one role, by where they stand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RoleCounts {
    /// The role (`frontend`, `storage`, `daemon`, `mac-daemon`, or another a definition names).
    pub role: String,
    /// Live and running the desired version.
    pub at_desired: usize,
    /// Live and updating.
    pub updating: usize,
    /// Live, behind, not updating, and the last update to the desired version failed.
    pub failed: usize,
    /// Silent for [`crate::fleet::DOWN_AFTER`] or more.
    pub down: usize,
}

/// The halt a convergence names: the host, the check and the version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HaltNote {
    /// The host that reported it.
    pub node_id: String,
    /// The check that failed.
    pub check: String,
    /// The version it was updating to.
    pub target: Version,
}

impl From<Halt> for HaltNote {
    fn from(halt: Halt) -> Self {
        Self {
            node_id: halt.node_id,
            check: halt.check,
            target: halt.target,
        }
    }
}

/// What CI reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Convergence {
    /// The desired version, if one was ever set.
    pub desired: Option<Version>,
    /// The coordinator is still within its quiet period and orders nothing.
    pub quiet: bool,
    /// Where the farm stands.
    pub state: State,
    /// The counts of every role that has a host, in role order.
    pub roles: Vec<RoleCounts>,
    /// The halt that stands, when the state is `halted`: the host, the check and the version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halt: Option<HaltNote>,
    /// The hosts that are not counted in `roles`, by why. Left out for a service with no
    /// selector until there is one to count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<HostCounts>,
    /// The commit this coordinator was built from ([`crate::BUILD_COMMIT`]): what a client
    /// compares with a release before it orders one.
    pub commit: &'static str,
}

impl Convergence {
    /// This convergence of a service that `halt` stops: `halted`, naming the halt.
    #[must_use]
    pub fn halted(self, halt: Halt) -> Self {
        Self {
            state: State::Halted,
            halt: Some(halt.into()),
            ..self
        }
    }
}

fn failed_to_reach(host: &Host, desired: &Desired) -> bool {
    host.beat.version != desired.version
        && host.beat.updating.is_none()
        && host
            .beat
            .last_update
            .as_ref()
            .is_some_and(|r| r.outcome == Outcome::Failed && r.target == desired.version)
}

/// Whether the desired version's release index lists no package for the platform of `host`:
/// nothing is ordered it, so it is not counted.
fn unlisted(host: &Host, desired: &Desired) -> bool {
    desired.lacks_package_for(host.beat.platform)
}

/// Whether the host `id` of `def` is counted against `desired`: the service is assigned to it
/// ([`Fleet::is_assigned`], the function the order path asks), an operator does not hold the pair,
/// and the release index lists a package for its platform.
fn counted(def: &ServiceDef, desired: &Desired, fleet: &Fleet, id: &HostId, host: &Host) -> bool {
    fleet.is_assigned(def, id) && !fleet.is_held(&def.name, id) && !unlisted(host, desired)
}

fn counts(
    def: &ServiceDef,
    role: &str,
    desired: &Desired,
    fleet: &Fleet,
    now: Duration,
) -> RoleCounts {
    let mine = || {
        fleet
            .hosts_of(&def.name)
            .filter(|(id, h)| counted(def, desired, fleet, id, h))
            .map(|(_, h)| h)
            .filter(move |h| h.role_name() == role)
    };
    RoleCounts {
        role: role.to_owned(),
        at_desired: mine()
            .filter(|h| h.is_live(now) && h.beat.version == desired.version)
            .count(),
        updating: mine()
            .filter(|h| h.is_live(now) && h.beat.updating.is_some())
            .count(),
        failed: mine()
            .filter(|h| h.is_live(now) && failed_to_reach(h, desired))
            .count(),
        down: mine().filter(|h| !h.is_live(now)).count(),
    }
}

/// The counts of every role that has a counted host of `def` (assigned, not held, listed):
/// the rollout file's roles first, in their order ([`ServiceDef::role_names`]), then any other
/// role, by name, that holds a host an operator's entry assigned (`Services.Assign`); none when
/// no desired state was set.
#[must_use]
pub fn role_counts(
    def: &ServiceDef,
    desired: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> Vec<RoleCounts> {
    let Some(desired) = desired else {
        return Vec::new();
    };
    let assigned: BTreeSet<&str> = fleet
        .hosts_of(&def.name)
        .filter(|(id, h)| counted(def, desired, fleet, id, h))
        .map(|(_, h)| h.role_name())
        .collect();
    let base = def.role_names();
    let added = assigned.iter().copied().filter(|role| !base.contains(role));
    base.iter()
        .copied()
        .filter(|role| assigned.contains(role))
        .chain(added)
        .map(|role| counts(def, role, desired, fleet, now))
        .filter(|c| c.at_desired + c.updating + c.failed + c.down > 0)
        .collect()
}

/// Where the hosts of the service `def` stand at `now` against its `desired`, over the hosts
/// that heartbeated the service, that it is assigned to ([`Fleet::is_assigned`]: by the rollout
/// file or an operator's entry) and that an operator does not hold ([`Fleet::is_held`]). One
/// rule serves every service: converged when every live member runs the desired version and
/// none is updating, failed, baking or waiting for a promotion; no number of hosts up is
/// required.
#[must_use]
pub fn converge_service(
    def: &ServiceDef,
    desired: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> Convergence {
    let quiet = fleet.is_quiet(now);
    let Some(desired) = desired else {
        return Convergence {
            desired: None,
            quiet,
            state: State::InProgress,
            roles: Vec::new(),
            halt: None,
            hosts: None,
            commit: crate::BUILD_COMMIT,
        };
    };
    let service = def.name.as_str();
    let roles = role_counts(def, Some(desired), fleet, now);
    let live = fleet
        .hosts_of(service)
        .filter(|(id, h)| h.is_live(now) && counted(def, desired, fleet, id, h))
        .count();
    let at_desired: usize = roles.iter().map(|c| c.at_desired).sum();
    let clear = roles.iter().all(|c| c.updating == 0 && c.failed == 0);
    // A host still baking, or waiting for its promotion, is not yet a host that took the
    // version. A pair an operator holds is left out of every count: it never blocks the others.
    let settled = fleet.hosts_of(service).all(|(id, h)| {
        !h.is_live(now)
            || !counted(def, desired, fleet, id, h)
            || !(h.beat.promotion_pending
                || fleet.is_baking(service, id, h, &desired.version, def.bake_beats, now))
    });
    let state = if roles.iter().any(|c| c.failed > 0) {
        State::Blocked
    } else if !quiet && live > 0 && at_desired == live && clear && settled {
        State::Converged
    } else {
        State::InProgress
    };
    Convergence {
        desired: Some(desired.version.clone()),
        quiet,
        state,
        roles,
        halt: None,
        hosts: None,
        commit: crate::BUILD_COMMIT,
    }
}
