//! The reconciling decision: whether to tell one host to update now
//! (`docs/design/host-updates.md`).
//!
//! A pure function of the services, their desired states, the fleet the heartbeats built, the
//! host and the time, decided per host and service. A host is told to update a service only if
//! every rule below holds, checked in this order (the role is the host's role in that service's
//! definition):
//!
//! 1. the coordinator has been up for its quiet start (`--quiet-secs`, by default
//!    [`crate::fleet::QUIET_START`]);
//! 2. the host is not already updating, and no other service updates on it or has an order
//!    outstanding for it (a host does one update at a time);
//! 3. the host is behind: it does not run the desired version (a rollback is a desired
//!    version older than the one the hosts run, under the same rules);
//! 4. the host did not fail an update to this very version (it is not told again; setting
//!    another version lifts it);
//! 5. the host did not decline an update to this version within its wait
//!    ([`crate::defer`]; a deferral is not a failure, holds no slot, and a new desired
//!    version lifts it);
//! 6. the service's canary gate, if it has one, is open to the host ([`crate::canary`]; a
//!    host that is deferring is not chosen as a canary);
//! 7. the service runs on the host's role, and no update is live anywhere: no host, of any
//!    role, holds an update slot for any service. One update is live at a time across every
//!    service and role ([`live`]). A host holds a slot while updating, while its promotion is
//!    pending, after a failed update until it heartbeats healthy, from the order the
//!    coordinator gave it until the host reports it or the order ages out,
//!    [`crate::fleet::ORDER_AGE`], and, at its service's desired version, until it has beaten
//!    healthy that service's `bake_beats` times, [`crate::bake`]. The asking host's own slot
//!    for the asked service is not counted, so a host asking again under its own order is told
//!    the same again. A host the service no longer counts (excluded, out of its selector, not
//!    announcing it) still holds its slot until its beats or its order's age free it
//!    ([`Fleet::occupants_of`]).
//!
//! A version marked bad ([`crate::reconcile`]) is ordered to nobody, whatever the rules above
//! say, and is held at [`Gate::Bad`] after the quiet start; so is a desired version below the
//! service's `min_version` ([`Gate::BelowMinVersion`]), however it got into the state file: CI's
//! set refuses one, and a hand edit of the file does not reach a host.
//! A host that reports the service `absent` (declared, never registered) is held back before
//! all of these but the first, unless the definition says `install_when_absent` ([`Skip::Absent`]).
//!
//! A pair an operator holds ([`crate::holds`]: the host, its role at the time of the decision,
//! or the whole fleet) is told nothing, after a halt and the membership check and before every
//! numbered rule above ([`Gate::Held`]): no update, however far behind it is. It takes no slot
//! of rule 7 and never bakes; an update it was running, or an order it was given, when the hold
//! was set still counts until it ends. The pairs a standing force orders
//! ([`crate::force`]) take no slot either.
//!
//! The decision records nothing: the caller notes an `Update` it returns with
//! [`Fleet::order`], under the same lock, so the next host's decision sees it. A host that
//! asks again while its own order is outstanding is told the same again: its own order never
//! counts against it, and the others' slots have not changed.
//!
//! The reply carries the desired version and nothing else. [`decide_why`] is the same decision
//! that also says why it holds. A [`Gate`] is a rule that is closed at this moment and opens by
//! itself or when something changes (the quiet start, the one live update, the canary, a bake, a
//! version marked bad, a halt); a [`Skip`] is a question that does not apply (no definition, no
//! desired state, not a member of the service, a host the service has not registered, a host at
//! the desired version, one that failed it, one that declined it). [`Why`] is either, spelled in
//! snake case for the status. [`decide_in`] drops the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Outcome, Reply};
use serde::Serialize;

use crate::canary;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::{ServiceDef, Services};

/// What to tell the host `id` about `service` at `now`, given every service's `desired`
/// state. A service with no definition or no desired state is told nothing. This is
/// [`decide_why`] without the reason.
#[must_use]
pub fn decide_in(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Reply {
    match decide_why(services, desired, service, fleet, id, now) {
        Decision::Update { version } => Reply::Update { version },
        Decision::Gate(_) | Decision::Skip(_) => Reply::Nothing,
    }
}

/// A rule that holds an order back at this moment. The status says it in snake case
/// (`no_slot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// The service is halted: a host reported a failure no version can repair and an operator
    /// has not cleared it. Checked before every other rule ([`crate::halt::decide`]).
    Halted,
    /// An operator holds the pair ([`crate::holds`]): it is ordered nothing until the hold is
    /// released. Checked right after the host is found a member of the service.
    Held,
    /// The desired version is marked bad ([`crate::reconcile`]).
    Bad,
    /// The desired version is below the service's `min_version`: no order goes below the floor.
    BelowMinVersion,
    /// The host is updating this service.
    Updating,
    /// The host is updating another service or has an order outstanding for one.
    BusyElsewhere,
    /// A host, of any role, holds an update slot for some service (one live update at a time).
    NoSlot,
    /// The coordinator started less than its quiet start (`--quiet-secs`) ago.
    Quiet,
    /// The service's canary gate is closed and the host is not one of the canaries.
    CanaryWait,
    /// No host holds an update slot, but a host, of any role, runs its service's desired
    /// version and has not yet baked.
    Baking,
}

/// A question that does not apply to this host and service: there is nothing to decide. The
/// status says it in snake case (`up_to_date`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// The service has no definition.
    NoDefinition,
    /// The service has no desired state.
    NoDesired,
    /// The coordinator has not heard from the host for the service, or the service does not
    /// run on the host's role.
    NotAMember,
    /// The service is declared on the host and has never registered, and the definition does
    /// not say `install_when_absent`.
    Absent,
    /// The host runs the desired version.
    UpToDate,
    /// The host failed an update to the desired version.
    FailedHere,
    /// The host declined an update to the desired version and its wait is not over.
    Deferred,
}

/// Why a host is told nothing, as the status says it in `hold`: a gate or a skip, each in its own
/// snake case spelling (`"no_slot"`, `"up_to_date"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Why {
    /// A gate that is closed.
    Gate(Gate),
    /// A question that does not apply.
    Skip(Skip),
}

/// What [`decide_why`] found for one host and service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Tell the host to update to `version`.
    Update {
        /// The desired version.
        version: Version,
    },
    /// Tell it nothing: a gate is closed.
    Gate(Gate),
    /// Tell it nothing: the question does not apply.
    Skip(Skip),
}

impl Decision {
    /// Why the host is told nothing, or `None` when it is told to update.
    #[must_use]
    pub fn why(&self) -> Option<Why> {
        match self {
            Self::Update { .. } => None,
            Self::Gate(gate) => Some(Why::Gate(*gate)),
            Self::Skip(skip) => Some(Why::Skip(*skip)),
        }
    }
}

/// The decision for the host `id` about `service` at `now`, with the rule that held when it
/// is not an order, for a service none of whose versions is marked bad. [`decide_in`] drops
/// the reason.
#[must_use]
pub fn decide_why(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Decision {
    decide_marked(services, desired, &[], service, fleet, id, now)
}

/// [`decide_why`] for a service whose versions `bad` are marked bad: the only implementation
/// of the rules.
#[must_use]
pub fn decide_marked(
    services: &Services,
    desired: &BTreeMap<String, Desired>,
    bad: &[Version],
    service: &str,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Decision {
    let Some(def) = services.get(service) else {
        return Decision::Skip(Skip::NoDefinition);
    };
    let Some(want) = desired.get(service) else {
        return Decision::Skip(Skip::NoDesired);
    };
    first_hold(services, def, want, desired, bad, fleet, id, now).unwrap_or_else(|| {
        Decision::Update {
            version: want.version.clone(),
        }
    })
}

/// The first rule that fails for the host `id`, or `None` when every rule holds.
#[expect(
    clippy::too_many_arguments,
    reason = "the inputs of the one decision, passed down from decide_marked"
)]
fn first_hold(
    services: &Services,
    def: &ServiceDef,
    desired: &Desired,
    all: &BTreeMap<String, Desired>,
    bad: &[Version],
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Option<Decision> {
    let service = def.name.as_str();
    let Some(host) = fleet
        .get_in(service, id)
        .filter(|h| def.roles.contains(h.role_name()))
    else {
        return Some(Decision::Skip(Skip::NotAMember));
    };
    let failed_here = host
        .beat
        .last_update
        .as_ref()
        .is_some_and(|r| r.outcome == Outcome::Failed && r.target == desired.version);
    let live = live(services, all, fleet, service, id, now);
    let rules = [
        (fleet.is_held(service, id), Decision::Gate(Gate::Held)),
        (fleet.is_quiet(now), Decision::Gate(Gate::Quiet)),
        (bad.contains(&desired.version), Decision::Gate(Gate::Bad)),
        (
            def.min_version
                .as_ref()
                .is_some_and(|floor| !desired.version.at_least(floor)),
            Decision::Gate(Gate::BelowMinVersion),
        ),
        (
            host.beat.health == Health::Absent && !def.install_when_absent,
            Decision::Skip(Skip::Absent),
        ),
        (host.beat.updating.is_some(), Decision::Gate(Gate::Updating)),
        (
            fleet.is_busy_besides(service, id, now),
            Decision::Gate(Gate::BusyElsewhere),
        ),
        (
            host.beat.version == desired.version,
            Decision::Skip(Skip::UpToDate),
        ),
        (failed_here, Decision::Skip(Skip::FailedHere)),
        (
            fleet
                .deferred_until(service, id, &desired.version, &def.defer_backoff)
                .is_some_and(|until| now < until),
            Decision::Skip(Skip::Deferred),
        ),
        (
            canary::closed_to(def, desired, fleet, id, host, now),
            Decision::Gate(Gate::CanaryWait),
        ),
        (live.slots > 0, Decision::Gate(Gate::NoSlot)),
        (live.slots + live.baking > 0, Decision::Gate(Gate::Baking)),
    ];
    rules
        .into_iter()
        .find_map(|(holds, why)| holds.then_some(why))
}

/// The updates live across the fleet, as rule 7 counts them: each host once, however many
/// services it is busy with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Live {
    /// Hosts that hold an update slot for some service ([`Fleet::holds_slot`]).
    slots: usize,
    /// Hosts that hold none and are still baking some service's desired version
    /// ([`Fleet::is_baking`]).
    baking: usize,
}

/// The updates live across every service the coordinator manages and every role, leaving out
/// the asking host `id`'s own slot for the asked `service` and the pairs a standing force orders
/// ([`crate::force`]: a force takes no slot). A pair an operator holds counts only while it runs
/// an update or has an order outstanding ([`crate::holds`]). A service with no desired
/// state has nothing to bake, so only its slots count.
fn live(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    fleet: &Fleet,
    service: &str,
    id: &HostId,
    now: Duration,
) -> Live {
    let mut holding = BTreeSet::new();
    let mut baking = BTreeSet::new();
    for def in services.defs() {
        let name = def.name.as_str();
        let want = all.get(name);
        for (o, h) in fleet
            .occupants_of(name)
            .filter(|(o, _)| !(name == service && *o == id))
            .filter(|(o, _)| !fleet.is_forced(name, o))
        {
            // A held pair takes no slot and never bakes: only an update it was already running,
            // or an order it was given, when the hold was set counts, until it ends.
            let held = fleet.is_held(name, o);
            let slot = if held {
                h.beat.updating.is_some() || fleet.is_ordered(name, o, now)
            } else {
                fleet.holds_slot(name, o, h, now)
            };
            if slot {
                holding.insert(o);
            } else if !held
                && want
                    .is_some_and(|w| fleet.is_baking(name, o, h, &w.version, def.bake_beats, now))
            {
                baking.insert(o);
            }
        }
    }
    Live {
        slots: holding.len(),
        baking: baking.difference(&holding).count(),
    }
}
