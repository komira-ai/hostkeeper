//! The terms of an update order: the `order_id`, `order_try` and `package_digest` that the gRPC
//! `Order` of `hostkeeper.agent.v1` carries and the answer of a transport that carries typed
//! orders carries too, for an agent that announced [`hostkeeper_wire::wire::ORDER_FEATURE`]. The
//! JSON heartbeat route carries no order, so none of them.
//!
//! The order id names the update, not the answer: it is `update-` and the first 32 lowercase
//! hex characters of the SHA-256 of the service, a newline and the version, so the same update
//! has the same id at every heartbeat and across a coordinator restart, and fits the 64 bytes of
//! an [`OrderId`]. The try is what the host's verdicts say, per service as the daemon keys it
//! (design section 8.3, [`crate::fleet::Fleet::next_try`]): one past the highest try the host's
//! heartbeats carried for the service, whatever order the verdict ended (an install, an earlier
//! update, a force), and 1 when none carried one. A resend of an outstanding order therefore keeps
//! its try, the next order after a verdict raises it, and a restarted coordinator, which rebuilds
//! the tries from the heartbeats and the verdicts file, never issues a try lower than the host
//! reported.
//!
//! The package digest is the one of the entry picked for the host ([`crate::picks`]), with the
//! digest of the release index set beside the targets, if one was (typed orders carry it; the
//! answer has no field for it). A version with no release kept picks no entry, so it has no
//! terms and nothing is ordered it.

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{OrderId, PackageDigest, ServiceReply};

use crate::ops_file;
use crate::orders::{Order, PickedTarget};
use crate::registry::{Digest, Package};

/// What an update answer carries for an agent that announced `order-v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// The id of the order.
    pub order_id: OrderId,
    /// The try of the order, from 1.
    pub order_try: u64,
    /// The digest of the package of the version for the host's platform.
    pub package_digest: PackageDigest,
    /// The digest of the release index set beside the targets, if the set had one. Only a typed
    /// order carries it.
    pub index_digest: Option<Digest>,
}

/// The id of the order that updates `service` to `version`, as text: `update-` and 32 lowercase
/// hex characters.
#[must_use]
pub fn order_id_text(service: &str, version: &Version) -> String {
    let hash = ops_file::digest(format!("{service}\n{}", version.as_str()).as_bytes());
    format!("update-{}", &hash[..32])
}

/// The terms of the order that updates `service` to `version`, whose package for the host is
/// `package`, at the try `order_try`. `None` only
/// if the id or the digest were no such thing, which their grammars rule out for a version and a
/// digest that were read.
#[must_use]
pub fn terms(service: &str, version: &Version, package: &Package, order_try: u64) -> Option<Terms> {
    OrderId::parse(&order_id_text(service, version))
        .zip(PackageDigest::parse(package.digest.as_str()))
        .map(|(id, package_digest)| Terms {
            order_try,
            order_id: id,
            package_digest,
            index_digest: package.index_digest.clone(),
        })
}

/// The update `reply` with its `terms` as a typed order: what a transport that carries typed
/// orders sends for it, with the entry `target` picked for the host of a version set with targets
/// ([`crate::picks`]). None for any other reply, or without terms.
pub(crate) fn typed(
    reply: &ServiceReply,
    terms: Option<&Terms>,
    target: Option<PickedTarget>,
) -> Option<Order> {
    match (reply, terms) {
        (
            ServiceReply::Update {
                service: Some(service),
                version,
            },
            Some(terms),
        ) => Some(Order::Update {
            service: service.clone(),
            version: version.clone(),
            digest: terms.package_digest.clone(),
            index_digest: terms.index_digest.clone(),
            order_id: terms.order_id.as_str().to_owned(),
            order_try: terms.order_try,
            target,
        }),
        _ => None,
    }
}
