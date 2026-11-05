//! The terms of an update order on the JSON heartbeat route: the `order_id`, `order_try` and
//! `package_digest` that the gRPC `Order` of `hostkeeper.agent.v1` carries and an update answer
//! carries too, for an agent that announced [`hostkeeper_wire::wire::ORDER_FEATURE`].
//!
//! The order id names the update, not the answer: it is `update-` and the first 32 lowercase
//! hex characters of the SHA-256 of the service, a newline and the version, so the same update
//! has the same id at every heartbeat and across a coordinator restart, and fits the 64 bytes of
//! an [`OrderId`]. The try is what the host's verdicts say: 1 for an order the host has not
//! reported a verdict of, and one more than the try of the verdict the host reports for this
//! order id. A resend of an outstanding order therefore keeps its try, an order given again after
//! a verdict raises it, and a restarted coordinator, which remembers no tries, never issues a try
//! lower than the host reported.
//!
//! The package digest is the one CI registered with the version ([`crate::registry`]); a version
//! with none registered has no terms, and an agent that reads terms is not ordered it.

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{OrderId, PackageDigest, UpdateResult};

use crate::ops_file;
use crate::registry::Digest;

/// What an update answer carries for an agent that announced `order-v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// The id of the order.
    pub order_id: OrderId,
    /// The try of the order, from 1.
    pub order_try: u64,
    /// The digest of the package of the version.
    pub package_digest: PackageDigest,
}

/// The id of the order that updates `service` to `version`, as text: `update-` and 32 lowercase
/// hex characters.
#[must_use]
pub fn order_id_text(service: &str, version: &Version) -> String {
    let hash = ops_file::digest(format!("{service}\n{}", version.as_str()).as_bytes());
    format!("update-{}", &hash[..32])
}

/// The try of the order `id`, given the last update the host reported for the service: one more
/// than the try of that verdict when it is a verdict of this order, else 1.
#[must_use]
pub fn order_try(id: &OrderId, last: Option<&UpdateResult>) -> u64 {
    last.filter(|result| result.order_id.as_ref() == Some(id))
        .map_or(1, |result| result.order_try.unwrap_or(0).saturating_add(1))
}

/// The terms of the order that updates `service` to `version`, whose package CI registered as
/// `digest`, given the last update the host reported for the service. `None` only if the
/// id or the digest were no such thing, which their grammars rule out for a version and a digest
/// that were read.
#[must_use]
pub fn terms(
    service: &str,
    version: &Version,
    digest: &Digest,
    last: Option<&UpdateResult>,
) -> Option<Terms> {
    OrderId::parse(&order_id_text(service, version))
        .zip(PackageDigest::parse(digest.as_str()))
        .map(|(id, package_digest)| Terms {
            order_try: order_try(&id, last),
            order_id: id,
            package_digest,
        })
}
