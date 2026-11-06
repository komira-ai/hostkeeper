//! The reconciling decision: whether to tell one host to update, install or uninstall a service
//! now (`docs/design/host-updates.md`; design 5.4 and 5.7).
//!
//! A pure function of the services, their desired states, the fleet the heartbeats built, the
//! host and the time, decided per host and service. A host runs the services the coordinator
//! assigns to it ([`crate::assignment`]: by its role in the rollout file, with the operator's
//! entries on top), and nothing else: a service not assigned to the host is never ordered,
//! whether the host runs it or not, unless an operator's `Unassign` ordered the host to
//! uninstall it ([`Decision::Uninstall`]). A service the coordinator does not manage, a host an
//! operator excluded, and a pair the fleet has no word of are [`Skip::NotAMember`].
//!
//! An assigned service the host runs is told to update, and one it does not run (it does not
//! list it, or lists it `absent`: staged, never run) is told to install
//! ([`Decision::Install`], with the package digest of the desired version for the host's
//! platform), under the same rules,
//! checked in this order (an install has no version to be up to date at, and is ordered only to
//! a daemon that announced [`crate::orders::INSTALL_FEATURE`] and lists every service it runs):
//!
//! 1. the coordinator has been up for its quiet start (`--quiet-secs`, by default
//!    [`crate::fleet::QUIET_START`]);
//! 2. the host is not already updating, and no other service updates on it or has an order
//!    outstanding for it (a host does one update at a time);
//! 3. the host is behind: it does not run the desired version (a rollback is a desired
//!    version older than the one the hosts run, under the same rules), or does not run the
//!    service at all;
//! 4. the host did not fail an update or an install to this very version (it is not told
//!    again; setting another version lifts it);
//! 5. the host did not decline an update to this version within its wait
//!    ([`crate::defer`]; a deferral is not a failure, holds no slot, and a new desired
//!    version lifts it);
//! 6. the service's canary gate, if it has one, is open to the host ([`crate::canary`]; a
//!    host that is deferring is not chosen as a canary);
//! 7. no update is live anywhere: no host, of any role, holds an update slot for any service.
//!    One update is live at a time across every service and role ([`live`]). A host holds a
//!    slot while updating, while its promotion is pending, after a failed update until it
//!    heartbeats healthy, from the order the coordinator gave it (an update, an install or an
//!    uninstall) until the host reports it or the order ages out, [`crate::fleet::ORDER_AGE`],
//!    and, at its service's desired version, until it has beaten healthy that service's
//!    `bake_beats` times, [`crate::bake`]. The asking host's own slot for the asked service is
//!    not counted, so a host asking again under its own order is told the same again. A host
//!    the service no longer counts (excluded, out of its selector, not announcing it) still
//!    holds its slot until its beats or its order's age free it ([`Fleet::occupants_of`]).
//!
//! A version marked bad ([`crate::reconcile`]) is ordered to nobody, whatever the rules above
//! say, and is held at [`Gate::Bad`] after the quiet start; so is a desired version below the
//! minimum version that applies to the host ([`Gate::BelowMinVersion`]: its own, else its
//! role's, never below the rollout file's `min_version`), however it got into the state file:
//! CI's set refuses one below the rollout file's, and a hand edit of the file does not reach a
//! host.
//!
//! An uninstall is ordered to a host that lists the service, is not assigned it, and has an
//! uninstall intent for it, under the halt, the hold, the quiet start and the one live update
//! (rules 2 and 7); one that failed is left for the operator, and one the host declined waits
//! out its backoff, keyed by the version the host runs.
//!
//! A pair an operator holds ([`crate::holds`]: the host, its role at the time of the decision,
//! or the whole fleet) is told nothing, after a halt and the membership check and before every
//! numbered rule above ([`Gate::Held`]): no update, install or uninstall, however far behind it
//! is. It takes no slot of rule 7 and never bakes; an update it was running, or an order it was
//! given, when the hold was set still counts until it ends. The pairs a standing force orders
//! ([`crate::force`]) take no slot either.
//!
//! The decision records nothing: the caller notes an order it returns with [`Fleet::order`] or
//! [`Fleet::order_uninstall`], under the same lock, so the next host's decision sees it. A host
//! that asks again while its own order is outstanding is told the same again: its own order
//! never counts against it, and the others' slots have not changed.
//!
//! The JSON reply carries the desired version of an update and nothing else; an install and an
//! uninstall go only as typed orders ([`crate::orders`]). [`decide_why`] is the same decision
//! that also says why it holds. A [`Gate`] is a rule that is closed at this moment and opens by
//! itself or when something changes (the quiet start, the one live update, the canary, a bake, a
//! version marked bad, a halt); a [`Skip`] is a question that does not apply (no desired state,
//! not a member of the service, a host at the desired version, one that failed it, one that
//! declined it, an install the host's agent cannot take or that has no package digest, a release
//! index that lists no package for the host's platform). [`Why`]
//! is either, spelled in snake case for the status. [`decide_in`] drops the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Outcome, Reply};
use serde::Serialize;

use crate::agent_service::runs_itself;
use crate::canary;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::membership::HEARTBEAT_V2;
use crate::orders::INSTALL_FEATURE;
use crate::registry::Digest;
use crate::services::{ServiceDef, Services};

/// What to tell the host `id` about `service` at `now`, given every service's `desired`
/// state, as the JSON reply says it: an update, or nothing (an install and an uninstall go only
/// as typed orders). This is [`decide_why`] without the reason.
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
        Decision::Install { .. }
        | Decision::Uninstall { .. }
        | Decision::Gate(_)
        | Decision::Skip(_) => Reply::Nothing,
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
    /// The desired version is below the minimum version that applies to the host: no order
    /// goes below the floor.
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
    /// The service has no desired state.
    NoDesired,
    /// The service is not one the coordinator manages or not assigned to the host, an operator
    /// excluded the host from it, or the coordinator has not heard from the host for it (nor,
    /// for one the host does not run, from an agent that lists every service it runs).
    NotAMember,
    /// The host runs the desired version.
    UpToDate,
    /// The host failed an update or an install to the desired version, or the uninstall it is
    /// ordered.
    FailedHere,
    /// The host declined an update to the desired version, or the uninstall it is ordered, and
    /// its wait is not over.
    Deferred,
    /// The pair needs an install or an uninstall, and the host's agent did not announce that it
    /// takes them ([`crate::orders::INSTALL_FEATURE`]); or the service is the host's agent
    /// itself, whose install of the desired version succeeded while the agent kept running
    /// another, and the agent cannot take an update to the release it installed
    /// ([`crate::agent_service`]): it is ordered the next version set.
    Unsupported,
    /// The pair needs an install, and the desired version has no package digest to install.
    NoDigest,
    /// The desired version's release index lists no package for the host's platform: the host
    /// is not ordered the version, an update or an install. It is no failed update: it counts
    /// as none, halts nothing and rolls nothing back.
    NoPackageForPlatform,
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
    /// Tell the host to install the service at `version`, from the package `digest`.
    Install {
        /// The desired version.
        version: Version,
        /// The digest of its package for the host's platform, as CI set it.
        digest: Digest,
        /// The digest of the release index `digest` was taken from, when CI set an index.
        index_digest: Option<Digest>,
    },
    /// Tell the host, which runs `running` of the service, to uninstall it.
    Uninstall {
        /// The order id of the uninstall the operator's `Unassign` ordered.
        order_id: String,
        /// The version the host runs, which the order is tracked against.
        running: Version,
    },
    /// Tell it nothing: a gate is closed.
    Gate(Gate),
    /// Tell it nothing: the question does not apply.
    Skip(Skip),
}

impl Decision {
    /// Why the host is told nothing, or `None` when it is told to update, install or uninstall.
    #[must_use]
    pub fn why(&self) -> Option<Why> {
        match self {
            Self::Update { .. } | Self::Install { .. } | Self::Uninstall { .. } => None,
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
        return Decision::Skip(Skip::NotAMember);
    };
    let member = !fleet.is_excluded(service, id) && fleet.is_assigned(def, id);
    if !member
        && !fleet.is_excluded(service, id)
        && let Some(order_id) = fleet.uninstall_order(service, id)
    {
        return uninstall(services, def, desired, fleet, id, order_id, now);
    }
    let Some(want) = desired.get(service) else {
        return Decision::Skip(Skip::NoDesired);
    };
    if !member {
        return Decision::Skip(Skip::NotAMember);
    }
    order_or_hold(services, def, want, desired, bad, fleet, id, now)
}

/// The update or the install the host `id`, assigned `def`, is told, or the first rule that
/// holds it back.
#[expect(
    clippy::too_many_arguments,
    reason = "the inputs of the one decision, passed down from decide_marked"
)]
fn order_or_hold(
    services: &Services,
    def: &ServiceDef,
    desired: &Desired,
    all: &BTreeMap<String, Desired>,
    bad: &[Version],
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Decision {
    let service = def.name.as_str();
    // What the fleet kept of a service the host's last heartbeat no longer lists is no word of it.
    let host = fleet.get_in(service, id).filter(|_| {
        fleet
            .node(id)
            .is_none_or(|node| node.announced.contains_key(service))
    });
    // A host the service has no word of is told to install it only when its agent lists every
    // service it runs: from any other, a missing entry says nothing.
    if host.is_none() && !fleet.announced(id, HEARTBEAT_V2) {
        return Decision::Skip(Skip::NotAMember);
    }
    let beat = host.map(|h| &h.beat);
    let installed = beat.is_some_and(|b| b.health != Health::Absent);
    // The host's platform picks its package of a release index. Every host reaching here has
    // one: it has a word of the service, or it announced that it lists every service it runs,
    // which only a node says.
    let platform = fleet.platform_of(service, id);
    let package = platform.and_then(|platform| desired.package_for(platform));
    let unlisted = desired.release_index.is_some() && package.is_none();
    let order = if installed {
        Some(Decision::Update {
            version: desired.version.clone(),
        })
    } else {
        package.map(|package| Decision::Install {
            version: desired.version.clone(),
            digest: package.digest,
            index_digest: package.index_digest,
        })
    };
    let failed_here = beat
        .and_then(|b| b.last_update.as_ref())
        .is_some_and(|r| r.outcome == Outcome::Failed && r.target == desired.version);
    // The agent's own service, installed at the desired version by an agent that still runs
    // another: an agent that installs its own release without switching to it (one before the
    // install of `hostkeeper` ran its self-update) cannot take an update to the release it has
    // installed, so it waits for the next one ([`crate::agent_service`]).
    let installed_not_run = installed
        && fleet
            .node(id)
            .is_some_and(|node| runs_itself(node, service))
        && beat.is_some_and(|b| {
            b.version != desired.version
                && b.last_update
                    .as_ref()
                    .is_some_and(|r| r.outcome == Outcome::Succeeded && r.target == desired.version)
        });
    let live = live(services, all, fleet, service, id, now);
    let rules = [
        (fleet.is_held(service, id), Decision::Gate(Gate::Held)),
        (fleet.is_quiet(now), Decision::Gate(Gate::Quiet)),
        (bad.contains(&desired.version), Decision::Gate(Gate::Bad)),
        (
            fleet
                .min_version(def, id)
                .is_some_and(|floor| !desired.version.at_least(floor)),
            Decision::Gate(Gate::BelowMinVersion),
        ),
        (
            beat.is_some_and(|b| b.updating.is_some()),
            Decision::Gate(Gate::Updating),
        ),
        (
            fleet.is_busy_besides(service, id, now),
            Decision::Gate(Gate::BusyElsewhere),
        ),
        (
            installed && beat.is_some_and(|b| b.version == desired.version),
            Decision::Skip(Skip::UpToDate),
        ),
        (
            (!installed && !fleet.announced(id, INSTALL_FEATURE)) || installed_not_run,
            Decision::Skip(Skip::Unsupported),
        ),
        (unlisted, Decision::Skip(Skip::NoPackageForPlatform)),
        (order.is_none(), Decision::Skip(Skip::NoDigest)),
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
        .or(order)
        .unwrap_or(Decision::Skip(Skip::NoDigest))
}

/// The uninstall of `def` the host `id` is ordered by `order_id`, or the first rule that holds
/// it back: the host must still list the service, and the hold, the quiet start, the one update
/// at a time on the host, its failure or deferral of this uninstall and the one live update hold
/// it.
fn uninstall(
    services: &Services,
    def: &ServiceDef,
    all: &BTreeMap<String, Desired>,
    fleet: &Fleet,
    id: &HostId,
    order_id: String,
    now: Duration,
) -> Decision {
    let service = def.name.as_str();
    let Some(host) = fleet
        .occupant(service, id)
        .filter(|_| fleet.lists(id, service))
    else {
        return Decision::Skip(Skip::NotAMember);
    };
    let failed = host.beat.last_update.as_ref().is_some_and(|r| {
        r.outcome == Outcome::Failed
            && r.order_id
                .as_ref()
                .is_some_and(|reported| reported.as_str() == order_id)
    });
    let live = live(services, all, fleet, service, id, now);
    let rules = [
        (fleet.is_held(service, id), Decision::Gate(Gate::Held)),
        (fleet.is_quiet(now), Decision::Gate(Gate::Quiet)),
        (host.beat.updating.is_some(), Decision::Gate(Gate::Updating)),
        (
            fleet.is_busy_besides(service, id, now),
            Decision::Gate(Gate::BusyElsewhere),
        ),
        (
            !fleet.announced(id, INSTALL_FEATURE),
            Decision::Skip(Skip::Unsupported),
        ),
        (failed, Decision::Skip(Skip::FailedHere)),
        (
            fleet
                .deferred_until(service, id, &host.beat.version, &def.defer_backoff)
                .is_some_and(|until| now < until),
            Decision::Skip(Skip::Deferred),
        ),
        (live.slots > 0, Decision::Gate(Gate::NoSlot)),
        (live.slots + live.baking > 0, Decision::Gate(Gate::Baking)),
    ];
    rules
        .into_iter()
        .find_map(|(holds, why)| holds.then_some(why))
        .unwrap_or(Decision::Uninstall {
            order_id,
            running: host.beat.version.clone(),
        })
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
        // A host told to install a service it does not list holds its slot by its order alone.
        holding.extend(
            fleet
                .ordered_unseen(name, now)
                .filter(|o| !(name == service && *o == id))
                .filter(|o| !fleet.is_forced(name, o)),
        );
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
