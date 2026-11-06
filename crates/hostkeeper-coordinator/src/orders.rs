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
use hostkeeper_wire::wire::{PackageDigest, UpdateResult};

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

/// The try to send a host with its next order of a service, given the verdict `last` it reports
/// of that service: one more than that verdict's try, whatever order the verdict ended, else the
/// first. The daemon keys tries per service (design section 8.3): its step record holds the try of
/// the service's last order, and refuses the same try of another package (`try_conflict`) and a
/// lower one (`stale_try`); so the order after an install, an update or a force of the service
/// is one past it, whatever its order id. A resend before the host reports keeps the try; a
/// restarted coordinator reads it from the beat (an acknowledged verdict's from the verdicts
/// file), so it never sends a lower one. A verdict with no try counts as try 0, as the daemon's
/// record does.
#[must_use]
pub fn next_try(last: Option<&UpdateResult>) -> u64 {
    last.and_then(|result| result.order_try)
        .unwrap_or(0)
        .saturating_add(1)
}

/// The `index_digest` of an order as the agent's messages say it: the digest, or empty when the
/// order names no index (the daemon checks none).
fn index_text(digest: Option<&Digest>) -> String {
    digest.map(ToString::to_string).unwrap_or_default()
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
        /// The package digest CI registered for the version, for the host's platform.
        digest: PackageDigest,
        /// The digest of the release index it was taken from; none for a version registered
        /// with one package digest.
        index_digest: Option<Digest>,
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
        /// The package digest CI registered for the version, for the host's platform.
        digest: Digest,
        /// The digest of the release index it was taken from; none for a version registered
        /// with one package digest.
        index_digest: Option<Digest>,
        /// Greater than the halt generation the host holds the service under.
        halt_generation: u64,
        /// The force's order id: `force-<generation>`.
        order_id: String,
        /// The try ([`next_try`]).
        order_try: u64,
    },
    /// Add a service the coordinator assigned to the host, by the update path with no previous
    /// version.
    Install {
        /// The service.
        service: String,
        /// Its desired version.
        version: Version,
        /// The package digest CI set for the version, for the host's platform.
        digest: Digest,
        /// The digest of the release index it was taken from; none for a version set with one
        /// package digest.
        index_digest: Option<Digest>,
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
    /// id, and the try (the first, for an `abort`, which the daemon keys by its own order id and
    /// not by the service's step record).
    #[must_use]
    pub fn to_proto(&self) -> agent::Order {
        let (order_id, order_try, verb) = match self {
            Self::Update {
                service,
                version,
                digest,
                index_digest,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Update(agent::Update {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.as_str().to_owned(),
                    index_digest: index_text(index_digest.as_ref()),
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
                index_digest,
                halt_generation,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Force(agent::Force {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.to_string(),
                    halt_generation: *halt_generation,
                    index_digest: index_text(index_digest.as_ref()),
                }),
            ),
            Self::Install {
                service,
                version,
                digest,
                index_digest,
                order_id,
                order_try,
            } => (
                order_id,
                *order_try,
                agent::order::Verb::Install(agent::Install {
                    service: service.clone(),
                    version: version.to_string(),
                    package_digest: digest.to_string(),
                    index_digest: index_text(index_digest.as_ref()),
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
    // The package of the host's own platform; a platform the force's index does not list is
    // ordered nothing (the plan left such a host out when it was made).
    let package = force.package_for(host.beat.platform)?;
    (ordered && !done && !held).then(|| Order::Force {
        service: service.to_owned(),
        version: force.version.clone(),
        digest: package.digest,
        index_digest: package.index_digest,
        halt_generation: force.halt_generation,
        order_id,
        order_try: next_try(host.beat.last_update.as_ref()),
    })
}
