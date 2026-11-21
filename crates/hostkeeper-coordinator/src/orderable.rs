//! Whether a plan has a host it can order now: what lets a rollout take one of the cluster's
//! `--live-rollouts` ([`crate::live`]) or a place in the queue for one ([`crate::queue`]).
//!
//! The members of a plan that have not started are the hosts it orders next
//! ([`crate::stages::View`]). But the decision checks rules before the plan's gate
//! ([`crate::decide`]), and one of them can hold such a host for as long as its cause stands: the
//! host holds a halt of the service of its own (`halted_here`, `stale_halt_here`), its install
//! waits for a package listed before it that is not installed (`install_order`), it was moved into
//! its group and is not admitted yet or its uninstalls are not done (`moving`), another service
//! updates on it (`busy_elsewhere`), or its agent cannot take the order (`unsupported`,
//! `no_digest` and the picks' skips). A plan whose every host left to order is held so has nothing
//! to order: it takes no place, so it holds no other service of any group, and its plan says so
//! (`nothing_to_order`). It takes its place again, by when its version was set, as soon as one of
//! those hosts can be ordered.
//!
//! A host that deferred the update is not one left to order: it waits out its backoff by itself
//! ([`crate::defer`]). The quiet start holds every host alike and is no reason of one host, so it
//! is not read here.

use std::time::Duration;

use hostkeeper_wire::HostId;

use crate::decide::{Decision, Gate, consider};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::install_order;
use crate::services::ServiceDef;
use crate::stages::{HostState, View};

/// Whether a rule before the plan's gate holds the host `id`, a member of the plan of `def` at
/// `want` that has not started, from being ordered now. `group` is every package of the group
/// `def` was made for with its desired state, for the order of installs (empty for a service of
/// the rollout file).
fn held(
    def: &ServiceDef,
    want: &Desired,
    group: &[(&ServiceDef, Option<&Desired>)],
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> bool {
    let service = def.name.as_str();
    let halted_here = fleet
        .occupant(service, id)
        .is_some_and(|host| host.beat.local_halt.is_some());
    // A host moving into its group is held until it is admitted and, once it is, while its
    // uninstalls are not done, as the decision holds it (`moving`).
    let moving = fleet.holds_moving(id) || (fleet.is_moving(id) && fleet.uninstalls_listed(id));
    if halted_here || (def.scope.is_some() && moving) {
        return true;
    }
    let considered = consider(def, want, fleet.bad_versions(service), fleet, id, now);
    if !matches!(considered.hold, None | Some(Decision::Gate(Gate::Quiet))) {
        return true;
    }
    !considered.installed && install_order::waits_among(group, def, fleet, id, now)
}

/// Whether the plan `view` of `def` at `want` has a host left to order that the decision can
/// order now: a member that has not started, is not deferring, and that no rule before the plan's
/// gate holds ([`held`]).
pub(crate) fn can_order(
    def: &ServiceDef,
    want: &Desired,
    view: &View,
    fleet: &Fleet,
    now: Duration,
) -> bool {
    let cluster = fleet.cluster();
    let group: Vec<(&ServiceDef, Option<&Desired>)> = def
        .group()
        .map(|name| {
            cluster
                .in_group(name)
                .map(|entry| (&entry.def, entry.want.as_ref()))
                .collect()
        })
        .unwrap_or_default();
    view.members
        .iter()
        .filter(|m| m.state == HostState::Waiting && !m.deferring)
        .any(|m| !held(def, want, &group, fleet, &m.id, now))
}
