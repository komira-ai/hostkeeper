//! The typed orders a heartbeat answer carries on a transport that carries them: the
//! operator's, which go instead of the rollout gate's update (an `abort` an operator queued,
//! `Hosts.Abort`, and the `force` of a standing force, [`crate::force`]); the gate's update
//! itself, with the terms the JSON answer carries ([`Order::Update`]); and the gate's own
//! `install` and `uninstall` ([`crate::decide`]).
//!
//! They go only over a transport that carries typed orders
//! ([`crate::service::Coordinator::heartbeat_with_orders`], the `Order` of
//! `hostkeeper.agent.v1`), and only to a daemon that announced the order's feature: [`ABORT_FEATURE`]
//! for an abort, [`crate::force::FEATURE`] for a force, [`INSTALL_FEATURE`] for an install or an
//! uninstall. The JSON heartbeat route carries none of the operator's orders, an install or an
//! uninstall, so its answers never name them, and an abort queued for a host that cannot be sent
//! it waits. An abort and a force take no update slot; an install and an uninstall take it as an
//! update does.
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
use hostkeeper_wire::wire::{Heartbeat, PackageDigest};

use crate::fleet::Fleet;
use crate::ops::OpsState;
use crate::registry::Digest;

/// The feature a daemon announces to be sent an `abort`.
pub const ABORT_FEATURE: &str = "abort-v1";

/// The feature a daemon announces to be sent an `install` and an `uninstall`.
pub const INSTALL_FEATURE: &str = "install-v1";

/// How many hex digits of the package digest an install's order id carries: `install-` and
/// these fit the 64 bytes of an order id.
const INSTALL_ID_DIGITS: usize = 24;

/// The order id of an install of the package `digest`: `install-` and the first
/// [`INSTALL_ID_DIGITS`] digits of the digest, so every release has its own.
#[must_use]
pub fn install_order_id(digest: &Digest) -> String {
    let digits = digest
        .as_str()
        .get(..INSTALL_ID_DIGITS)
        .unwrap_or(digest.as_str());
    format!("install-{digits}")
}

/// The try of the order `order_id` to send a host whose last word about the service is `beat`:
/// one more than the try of the verdict it last reported of that order, else the first. A resend
/// before the host reports keeps the try; a restarted coordinator reads it from the beat, so it
/// never sends a lower one.
#[must_use]
pub fn next_try(beat: Option<&Heartbeat>, order_id: &str) -> u64 {
    beat.and_then(|b| b.last_update.as_ref())
        .filter(|r| {
            r.order_id
                .as_ref()
                .is_some_and(|id| id.as_str() == order_id)
        })
        .map_or(1, |r| r.order_try.unwrap_or(0).saturating_add(1))
}

/// An order a heartbeat answer carries as a typed order: an operator's, or the rollout gate's
/// update with its terms, install or uninstall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Order {
    /// The rollout gate's update, with the terms the JSON answer of an `order-v1` agent carries
    /// too ([`crate::order_terms`]): the same order id, try and package digest on both
    /// transports.
    Update {
        /// The service.
        service: String,
        /// The version to run.
        version: Version,
        /// The package digest CI registered for the version.
        digest: PackageDigest,
        /// The order's id ([`crate::order_terms::order_id_text`]).
        order_id: String,
        /// The try, from 1.
        order_try: u64,
    },
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
    /// Add a service the coordinator assigned to the host, by the update path with no previous
    /// version.
    Install {
        /// The service.
        service: String,
        /// Its desired version.
        version: Version,
        /// The package digest CI set for the version.
        digest: Digest,
        /// The order's id ([`install_order_id`]).
        order_id: String,
        /// The try ([`next_try`]).
        order_try: u64,
    },
    /// Stop the service's units, run its `uninstall` hook and remove its package and placed
    /// paths.
    Uninstall {
        /// The service.
        service: String,
        /// The order's id: `uninstall-<generation>` ([`crate::assignment`]).
        order_id: String,
        /// The try ([`next_try`]).
        order_try: u64,
    },
}

impl Order {
    /// The order as `hostkeeper.agent.v1` says it: the verb and its typed arguments, the order
    /// id, and the try (the first, for an operator's order).
    #[must_use]
    pub fn to_proto(&self) -> agent::Order {
        let (order_id, order_try, verb) = match self {
            Self::Update {
                service,
                version,
                digest,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Update(agent::Update {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.as_str().to_owned(),
                }),
            ),
            Self::Abort { service, order_id } => (
                order_id,
                1,
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
                1,
                agent::order::Verb::Force(agent::Force {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.to_string(),
                    halt_generation: *halt_generation,
                }),
            ),
            Self::Install {
                service,
                version,
                digest,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Install(agent::Install {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.to_string(),
                }),
            ),
            Self::Uninstall {
                service,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Uninstall(agent::Uninstall {
                    service: service.clone(),
                }),
            ),
        };
        agent::Order {
            order_id: order_id.clone(),
            r#try: order_try,
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
