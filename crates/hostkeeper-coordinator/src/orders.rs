//! The operator's orders a heartbeat answer carries instead of the rollout gate's update: an
//! `abort` an operator queued (`Hosts.Abort`) and the `force` of a standing force
//! ([`crate::force`]).
//!
//! They go only over a transport that carries typed orders
//! ([`crate::service::Coordinator::heartbeat_with_orders`], the `Order` of
//! `hostkeeper.agent.v1`), and only to a daemon that announced the order's feature: [`ABORT_FEATURE`]
//! for an abort, [`crate::force::FEATURE`] for a force. The JSON heartbeat route carries neither,
//! so its answers never name them, and an abort queued for a host that cannot be sent it waits.
//! Neither takes an update slot.
//!
//! An abort is sent once: it is taken from the queue when it is answered. A force's order goes
//! to each host the force orders at every heartbeat until the host runs the version, is running
//! the force (it reports updating to the version) or reported a verdict of the force's order id;
//! so a lost answer is sent again, and a host that reported the force failed is not ordered
//! again. A pair an operator held after the force was ordered is not sent it, unless the force
//! included held pairs.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use crate::fleet::Fleet;
use crate::ops::OpsState;
use crate::registry::Digest;

/// The feature a daemon announces to be sent an `abort`.
pub const ABORT_FEATURE: &str = "abort-v1";

/// An order of the operator's, as a heartbeat answer carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// Stop the service's running step and end its order in the halt phase.
    Abort {
        /// The service.
        service: String,
        /// The order's id: `abort-<n>`, the number of its audit entry.
        order_id: String,
    },
    /// Hookless: stop the service's units, switch to the version, link, start them, report.
    Force {
        /// The service.
        service: String,
        /// The version forced.
        version: Version,
        /// The package digest CI registered for the version.
        digest: Digest,
        /// Greater than the halt generation the host holds the service under.
        halt_generation: u64,
        /// The force's order id: `force-<generation>`.
        order_id: String,
    },
}

impl Order {
    /// The order as `hostkeeper.agent.v1` says it: the verb and its typed arguments, the order
    /// id, and the first try.
    #[must_use]
    pub fn to_proto(&self) -> agent::Order {
        let (order_id, verb) = match self {
            Self::Abort { service, order_id } => (
                order_id,
                agent::order::Verb::Abort(agent::Abort {
                    service: service.clone(),
                }),
            ),
            Self::Force {
                service,
                version,
                digest,
                halt_generation,
                order_id,
            } => (
                order_id,
                agent::order::Verb::Force(agent::Force {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.to_string(),
                    halt_generation: *halt_generation,
                }),
            ),
        };
        agent::Order {
            order_id: order_id.clone(),
            r#try: 1,
            verb: Some(verb),
        }
    }
}

/// The operator's order for the host `id` at this heartbeat, if one is due: an abort queued for
/// it first (taken, so it is sent once), then the force of the first of `services` (the order
/// the gate goes by) whose standing force orders the host and is not done there.
pub(crate) fn next(
    fleet: &mut Fleet,
    state: &OpsState,
    id: &HostId,
    services: &[&str],
) -> Option<Order> {
    if fleet.announced(id, ABORT_FEATURE)
        && let Some((service, order_id)) = fleet.take_abort(id)
    {
        return Some(Order::Abort { service, order_id });
    }
    if !fleet.announced(id, crate::force::FEATURE) {
        return None;
    }
    services
        .iter()
        .find_map(|service| force_for(fleet, state, service, id))
}

/// The force of `service` for the host `id`, if its standing force orders the host and the
/// host neither runs the version, nor is running the force, nor reported a verdict of it.
fn force_for(fleet: &Fleet, state: &OpsState, service: &str, id: &HostId) -> Option<Order> {
    let force = state.services.get(service)?.force.as_ref()?;
    let host = fleet.get_in(service, id)?;
    let order_id = force.order_id();
    let reported = host
        .beat
        .last_update
        .as_ref()
        .and_then(|last| last.order_id.as_ref())
        .is_some_and(|reported| reported.as_str() == order_id);
    let done = host.beat.version == force.version
        || host.beat.updating.as_ref() == Some(&force.version)
        || reported;
    let ordered = force.hosts.iter().any(|host| host == id.as_str());
    let held = !force.include_held && fleet.is_held(service, id);
    (ordered && !done && !held).then(|| Order::Force {
        service: service.to_owned(),
        version: force.version.clone(),
        digest: force.digest.clone(),
        halt_generation: force.halt_generation,
        order_id,
    })
}
