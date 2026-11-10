//! Which services' rollouts are live, and so take the farm's `--live-rollouts` (default 1): a
//! service whose own rollout is not live is told `rollout_elsewhere` while that many others are
//! ([`crate::stages`]'s `elsewhere`).
//!
//! Only a service that is moving takes a place: one of its hosts runs an update (updating, its
//! promotion pending, or under an order the coordinator sent), bakes the desired version after
//! an update that did not fail, or its plan is under way and nothing stops it. A failed host is
//! the operator's to resolve: it keeps holding a slot of its own service, against its role's
//! `max_updating`, so the service piles no more failures on, but it does not make the service
//! live. A halted service, a service at a version marked bad, and a plan held by a failed host
//! order nothing until an operator acts, so they never hold another service's rollout.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Outcome;

use crate::desired::Desired;
use crate::fleet::{Fleet, Host};
use crate::services::{ServiceDef, Services};
use crate::stages::{HostState, current};

/// Whether the rollout of `def` is live, and so takes one of the farm's `--live-rollouts`: one of
/// its pairs runs an update (it is updating, its promotion is pending, or an order is
/// outstanding for it) or bakes the desired version `want`, or its plan is under way
/// ([`crate::stages::View::under_way`]), neither halted nor at a version marked bad, and not held by a failed
/// host. The pairs a standing force orders are left out; a held pair counts only while it runs
/// an update or has an order outstanding.
///
/// A failed host is never live by itself: one whose last update failed and that is not healthy
/// keeps holding a slot of its own service, against its role's `max_updating` (`no_slot`),
/// so the service piles no more failures on, but it runs nothing and nothing moves it until an
/// operator acts. A halted service, or one whose only occupants are failed hosts, is therefore
/// not live: one service's failure never holds another service's rollout.
#[must_use]
pub fn is_live(def: &ServiceDef, want: Option<&Desired>, fleet: &Fleet, now: Duration) -> bool {
    let service = def.name.as_str();
    let slot = fleet
        .ordered_unseen(service, now)
        .any(|o| !fleet.is_forced(service, o))
        || fleet
            .occupants_of(service)
            .filter(|(o, _)| !fleet.is_forced(service, o))
            .any(|(o, h)| {
                h.beat.updating.is_some()
                    || (h.beat.promotion_pending && !fleet.is_held(service, o))
                    || fleet.is_ordered(service, o, now)
                    || want.is_some_and(|w| is_baking(def, w, fleet, o, h, now))
            });
    slot || want.is_some_and(|want| {
        !fleet.is_stopped(service, &want.version) && {
            let view = current(def, want, fleet, now);
            view.under_way() && !view.members.iter().any(|m| m.state == HostState::Failed)
        }
    })
}

/// Whether the host `id` bakes the desired version `want` of `def` for [`is_live`]: the service
/// is neither halted nor at a version marked bad, the host is neither held nor excluded, it runs
/// `want`'s version and has not yet beaten healthy as often as the bake needs, and its update to
/// that version did not fail. A host that failed its update or install to the version and
/// reports it all the same (unhealthy, or never started) bakes nothing: it waits for an
/// operator.
fn is_baking(
    def: &ServiceDef,
    want: &Desired,
    fleet: &Fleet,
    id: &HostId,
    host: &Host,
    now: Duration,
) -> bool {
    let service = def.name.as_str();
    let failed = host
        .beat
        .last_update
        .as_ref()
        .is_some_and(|r| r.outcome == Outcome::Failed && r.target == want.version);
    !failed
        && !fleet.is_stopped(service, &want.version)
        && !fleet.is_held(service, id)
        && !fleet.is_excluded(service, id)
        && fleet.is_baking(service, id, host, &want.version, def.bake_beats, now)
}

/// When `service` may not start a rollout now, the services whose live rollouts hold it, by
/// name: its own rollout is not live ([`is_live`]) and as many others are as `--live-rollouts`
/// lets be. None when it may start one. What `Releases.Status` names for a plan that waits
/// `rollout_elsewhere`.
#[must_use]
pub(crate) fn live_elsewhere(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    now: Duration,
) -> Option<Vec<String>> {
    let live = |def: &ServiceDef| is_live(def, all.get(&def.name), fleet, now);
    if services.get(service).is_some_and(live) {
        return None;
    }
    let others: Vec<String> = services
        .defs()
        .filter(|def| def.name != service)
        .filter(|def| live(def))
        .map(|def| def.name.clone())
        .collect();
    (others.len() >= fleet.live_rollouts()).then_some(others)
}
