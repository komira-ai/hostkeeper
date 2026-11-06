//! The terms of an update order on the JSON heartbeat route: the `order_id`, `order_try` and
//! `package_digest` that the gRPC `Order` of `hostkeeper.agent.v1` carries and an update answer
//! carries too, for an agent that announced [`hostkeeper_wire::wire::ORDER_FEATURE`].
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
//! The package digest is the one CI registered with the version ([`crate::registry`]), or, for a
//! version registered with a release index, the one the index lists for the host's platform, with
//! the digest of the index (typed orders carry it; the JSON answer has no field for it). A
//! version with none registered, or whose index lists no package for the host's platform, has no
//! terms, and an agent that reads terms is not ordered it.

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{OrderId, PackageDigest, ServiceReply};
use hostkeeper_wire::{HostId, Platform};

use crate::ops_file;
use crate::orders::Order;
use crate::registry::{Digest, Package, Registry};

/// What an update answer carries for an agent that announced `order-v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    /// The id of the order.
    pub order_id: OrderId,
    /// The try of the order, from 1.
    pub order_try: u64,
    /// The digest of the package of the version for the host's platform.
    pub package_digest: PackageDigest,
    /// The digest of the release index the package digest was taken from, if the version was
    /// registered with one. Only a typed order carries it.
    pub index_digest: Option<Digest>,
}

/// The id of the order that updates `service` to `version`, as text: `update-` and 32 lowercase
/// hex characters.
#[must_use]
pub fn order_id_text(service: &str, version: &Version) -> String {
    let hash = ops_file::digest(format!("{service}\n{}", version.as_str()).as_bytes());
    format!("update-{}", &hash[..32])
}

/// The terms of the order that updates `service` to `version`, whose package for the host CI
/// registered as `package`, at the try `order_try`. `None` only
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

/// The terms of the update of `service` to `version` for the host `id`, of `platform`, at the try
/// `order_try` ([`crate::fleet::Fleet::next_try`]), with the release `digests` registers for the
/// version. With none
/// registered, or an index that lists no package for the platform, none, and a warning naming
/// the service, the version and the host: such a host is not ordered the version.
pub(crate) fn of_beat(
    digests: &Registry,
    service: &str,
    version: &Version,
    order_try: u64,
    id: &HostId,
    platform: Platform,
) -> Option<Terms> {
    let made = digests
        .get(version)
        .and_then(|release| release.package_for(platform))
        .and_then(|package| terms(service, version, &package, order_try));
    if made.is_none() {
        tracing::warn!(
            service,
            version = %version,
            node_id = %id,
            %platform,
            "not ordering an agent that reads order terms: no package digest is registered for the version and the host's platform"
        );
    }
    made
}

/// The update `reply` with its `terms` as a typed order: what a transport that carries typed
/// orders sends for it. None for any other reply, or without terms.
pub(crate) fn typed(reply: &ServiceReply, terms: Option<&Terms>) -> Option<Order> {
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
        }),
        _ => None,
    }
}
