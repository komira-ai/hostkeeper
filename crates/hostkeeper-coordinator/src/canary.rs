//! The canary gate of a service (`docs/design/host-updates.md`, section 5.3): while it is
//! closed only the first hosts are ordered.
//!
//! A service with `canary = { count = n }` orders at most `n` hosts until `n` hosts at the
//! desired version have baked. The roles and labels form of the canary is not implemented:
//! the rollout file refuses it.
//!
//! The hosts that have *started* are those updating, under an order, failed at the desired
//! version, or at it: silent ones too, so a canary that went down after it began is not
//! replaced. The next hosts are chosen among the live ones that have not started, by node id,
//! leaving out the hosts that are deferring.

use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Outcome;

use crate::desired::Desired;
use crate::fleet::{Fleet, Host};
use crate::services::ServiceDef;

/// The hosts of a service that go alone first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canary {
    /// How many hosts.
    pub count: u32,
}

/// Whether the canary gate of `def` is closed to the host `id` at `now`: the service has a
/// canary, fewer than `count` hosts have baked at the desired version, and `id` (the host
/// `me`, a member of the service) is not among the hosts that may still be ordered. A host
/// under its own order is never held: it is told the same again.
#[must_use]
pub(crate) fn closed_to(
    def: &ServiceDef,
    want: &Desired,
    fleet: &Fleet,
    id: &HostId,
    me: &Host,
    now: Duration,
) -> bool {
    let Some(canary) = def.canary else {
        return false;
    };
    let service = def.name.as_str();
    let members = || {
        fleet
            .hosts_of(service)
            .filter(|(_, h)| def.roles.contains(h.role_name()))
            // A pair an operator holds is never a canary, and neither counts as one nor bakes.
            .filter(|(o, _)| !fleet.is_held(service, o))
    };
    let started = |o: &HostId, h: &Host| {
        h.beat.updating.is_some()
            || fleet.is_ordered(service, o, now)
            || h.beat.version == want.version
            || h.beat
                .last_update
                .as_ref()
                .is_some_and(|r| r.outcome == Outcome::Failed && r.target == want.version)
    };
    let baked = members()
        .filter(|(o, h)| {
            h.beat.version == want.version
                && fleet.is_up(service, o, h, now)
                && fleet.bake_beats(service, o, &want.version) >= def.bake_beats
        })
        .count();
    if baked >= canary.count as usize {
        return false;
    }
    // A host that declined the update is left alone: it is not chosen, and the next host is.
    let deferring = |o: &HostId| {
        fleet
            .deferred_until(service, o, &want.version, &def.defer_backoff)
            .is_some_and(|until| now < until)
    };
    if started(id, me) {
        return false;
    }
    let before = members()
        .filter(|(o, h)| h.is_live(now) && !started(o, h) && !deferring(o) && *o < id)
        .count();
    let free =
        (canary.count as usize).saturating_sub(members().filter(|(o, h)| started(o, h)).count());
    before >= free
}
