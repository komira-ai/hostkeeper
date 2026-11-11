//! Which services' rollouts are live, and so take the farm's `--live-rollouts` (default 1): a
//! service whose own rollout is not live is told `rollout_elsewhere` while that many others are
//! ([`crate::stages`]'s `elsewhere`).
//!
//! Only a service that is moving takes a place: one of its hosts runs an update (updating, or its
//! promotion pending), an order the coordinator sent it is outstanding, one of its plan's hosts
//! bakes the desired version after an update the coordinator saw, or its plan is under way and
//! nothing stops it. A failed host is the operator's to resolve: it keeps holding a slot of its
//! own service, against its role's `max_updating`, so the service piles no more failures on, but
//! it does not make the service live. A halted service, a service at a version marked bad, and a
//! plan held by a failed host order nothing until an operator acts, so they never hold another
//! service's rollout.
//!
//! A bake counts only for a host of the plan: an excluded or held host, a host that stopped
//! heartbeating and a host the release picks no package for bake nothing that holds the others.
//! Nor does a host the coordinator first heard, after it started, already settled at the version
//! ([`crate::bake::Found`]): a restart does not make a converged service live. So a service whose
//! every host runs its desired version, with nothing updating or ordered, is live only while a
//! host it saw update bakes.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Outcome;

use crate::desired::Desired;
use crate::fleet::{Fleet, Host};
use crate::services::{ServiceDef, Services};
use crate::stages::{HostState, current};

/// What makes a service's rollout live: the first of these found, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// A host's last word is an update of the service running.
    Updating,
    /// A host's last word is that it waits for the promotion of its update.
    PromotionPending,
    /// An order the coordinator sent a host is outstanding (it ages out after
    /// [`crate::fleet::ORDER_AGE`]).
    Ordered,
    /// A host of the plan bakes the desired version after an update.
    Baking,
    /// The plan has started a host and has one left to order.
    PlanUnderWay,
}

/// Why a service's rollout is live, and the host that makes it so (none for
/// [`Why::PlanUnderWay`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Live {
    /// What it does.
    pub(crate) why: Why,
    /// The host, by node id.
    pub(crate) host: Option<HostId>,
}

impl Live {
    fn of(why: Why, host: &HostId) -> Self {
        Self {
            why,
            host: Some(host.clone()),
        }
    }
}

/// Whether the rollout of `def` is live, and so takes one of the farm's `--live-rollouts`
/// ([`why_live`]).
#[must_use]
pub fn is_live(def: &ServiceDef, want: Option<&Desired>, fleet: &Fleet, now: Duration) -> bool {
    why_live(def, want, fleet, now).is_some()
}

/// Why the rollout of `def` is live, for its desired state `want`, at `now`; none when it is not.
/// In order, the first host by node id within each: a host whose last word is an update running
/// or its promotion pending (a host taken off the service, or silent, mid-update included: the
/// update may still run), a host with an order outstanding (one the fleet has no word of, or one
/// it has; an order ages out after [`crate::fleet::ORDER_AGE`]), a host of the plan that bakes
/// `want`'s version after an update the coordinator saw ([`is_baking`]), and a plan under way
/// ([`crate::stages::View::under_way`]) that is neither halted nor at a version marked bad, nor
/// held by a failed host. The pairs a standing force orders are left out; a held host's promotion
/// counts only while it is not held.
pub(crate) fn why_live(
    def: &ServiceDef,
    want: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> Option<Live> {
    let service = def.name.as_str();
    let counted = |id: &HostId| !fleet.is_forced_in(def, id);
    let occupants = || fleet.occupants_in(def).filter(|(id, _)| counted(id));
    if let Some((id, _)) = occupants().find(|(_, h)| h.beat.updating.is_some()) {
        return Some(Live::of(Why::Updating, id));
    }
    let pending = |id: &HostId, h: &Host| h.beat.promotion_pending && !fleet.is_held(service, id);
    if let Some((id, _)) = occupants().find(|(id, h)| pending(id, h)) {
        return Some(Live::of(Why::PromotionPending, id));
    }
    let unseen = fleet.ordered_unseen_in(def, now).filter(|id| counted(id));
    let ordered = occupants()
        .map(|(id, _)| id)
        .filter(|id| fleet.is_ordered(service, id, now));
    if let Some(id) = unseen.chain(ordered).min() {
        return Some(Live::of(Why::Ordered, id));
    }
    let want = want?;
    if fleet.is_stopped_in(def, &want.version) {
        return None;
    }
    let view = current(def, want, fleet, now);
    let baking = view
        .members
        .iter()
        .filter(|m| is_baking(def, want, fleet, &m.id, now))
        .map(|m| &m.id)
        .min();
    if let Some(id) = baking {
        return Some(Live::of(Why::Baking, id));
    }
    let failed = view.members.iter().any(|m| m.state == HostState::Failed);
    (view.under_way() && !failed).then_some(Live {
        why: Why::PlanUnderWay,
        host: None,
    })
}

/// Whether the host `id`, a host of the plan of `def` (eligible: live, assigned, neither excluded
/// nor held, picked a package of the version) with a word of the service its last heartbeat still
/// lists, bakes the desired version `want` for [`why_live`]:
/// it runs `want`'s version and has not yet beaten healthy as often as the bake needs (an
/// unhealthy beat starts the count again), its update to that version did not fail, and the
/// coordinator saw it move there. A host that failed its update or install to the version and
/// reports it all the same (unhealthy, or never started) bakes nothing: it waits for an operator.
/// One the coordinator first heard, after it started, already settled at the version
/// ([`crate::bake::Found`]) proves no update by its bake: a restart does not make a converged
/// service live.
fn is_baking(def: &ServiceDef, want: &Desired, fleet: &Fleet, id: &HostId, now: Duration) -> bool {
    let service = def.name.as_str();
    let Some(host) = crate::decide::heard(fleet, service, id) else {
        return false;
    };
    let failed = host
        .beat
        .last_update
        .as_ref()
        .is_some_and(|r| r.outcome == Outcome::Failed && r.target == want.version);
    !failed
        && !fleet.found.at(service, id, &want.version)
        && fleet.is_baking(service, id, host, &want.version, def.bake_beats, now)
}

/// When `service` may not start a rollout now, the services whose live rollouts hold it, by
/// name, each with why it is live ([`why_live`]): its own rollout is not live and as many others
/// are as `--live-rollouts` lets be. None when it may start one. What `Releases.Status` names for
/// a plan that waits `rollout_elsewhere`.
#[must_use]
pub(crate) fn live_elsewhere(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    now: Duration,
) -> Option<Vec<(String, Live)>> {
    let live = |def: &ServiceDef| why_live(def, all.get(&def.name), fleet, now);
    let own = services.get(service);
    if own.and_then(live).is_some() {
        return None;
    }
    let others: Vec<(String, Live)> = services
        .defs()
        .filter(|def| def.name != service)
        .filter_map(|def| live(def).map(|why| (def.name.clone(), why)))
        .collect();
    let limit = own.map_or(fleet.live_rollouts(), |def| fleet.live_limit(def));
    (others.len() >= limit).then_some(others)
}
