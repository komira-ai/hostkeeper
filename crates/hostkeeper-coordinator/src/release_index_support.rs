//! Builders for the tests of release indexes: the text of an `index.json` as the release job
//! writes it, a set of the desired state that carries it, and the heartbeats of daemons of
//! different platforms. The rig is [`crate::marks_support::rig`]: `web` and `db` on the roles
//! `daemon` and `storage`.

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::{PackageEntry, ReleaseIndex};
use hostkeeper_config::names::{ServiceName, Version as IndexVersion};
use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Heartbeat, ORDER_FEATURE, ServiceBeat};
use serde_json::{Value, json};

use crate::desired::Desired;
use crate::marks_support::{beat_of, digest};
use crate::orders::Order;
use crate::service::SetError;
use crate::testutil::{Rig, ci_token};

/// The features of a daemon that reads order terms and takes installs and forces.
pub(crate) const AGENT: &[&str] = &[
    "heartbeat-v2",
    "halt-v1",
    "deferred-v1",
    "forced-v1",
    "abort-v1",
    "install-v1",
    ORDER_FEATURE,
];

/// The three platforms of the index, each with the package digest [`digest`] of its number.
pub(crate) const ALL: [(IndexPlatform, u8); 3] = [
    (IndexPlatform::LinuxX86_64, 1),
    (IndexPlatform::LinuxArm64, 2),
    (IndexPlatform::MacosArm64, 3),
];

/// The text of the index of `service` at `version` (the canonical form the release job writes)
/// that lists, for each of `packages`, the package whose digest is [`digest`] of its number.
pub(crate) fn index_text(service: &str, version: &str, packages: &[(IndexPlatform, u8)]) -> String {
    let entries = packages
        .iter()
        .map(|(platform, n)| PackageEntry {
            platform: *platform,
            sha256: Sha256Hex::parse(&digest(*n)).expect("a SHA-256"),
            bytes: 4096,
        })
        .collect();
    ReleaseIndex::new(
        ServiceName::new(service).expect("a service name"),
        IndexVersion::new(version).expect("a version"),
        entries,
    )
    .expect("an index")
    .to_json()
}

/// The SHA-256 of `text`, in lowercase hex.
pub(crate) fn sha(text: &str) -> String {
    crate::ops_file::digest(text.as_bytes())
}

/// The body of a set of `service` at `version` that names the index `text` and its digest
/// `digest_of_text`.
pub(crate) fn index_body(service: &str, version: &str, text: &str, digest_of_text: &str) -> Value {
    json!({
        "service": service,
        "version": version,
        "min_up": {"storage": 0, "frontend": 0},
        "release_index": text,
        "release_index_digest": digest_of_text,
    })
}

/// CI's set of `body`, with a token no one has used.
pub(crate) async fn try_set(rig: &Rig, body: &Value) -> Result<Desired, SetError> {
    rig.coordinator
        .set_desired(Some(&ci_token()), body.to_string().as_bytes())
        .await
}

/// CI sets `service` at `version` over the index that lists `packages`; the SHA-256 of the
/// index.
pub(crate) async fn set_index(
    rig: &Rig,
    service: &str,
    version: &str,
    packages: &[(IndexPlatform, u8)],
) -> String {
    let text = index_text(service, version, packages);
    let digest = sha(&text);
    try_set(rig, &index_body(service, version, &text, &digest))
        .await
        .expect("the set is accepted");
    digest
}

/// A heartbeat of a daemon of `platform` that runs `services`.
pub(crate) fn beat_on(platform: Platform, services: Vec<ServiceBeat>) -> Heartbeat {
    let mut beat = beat_of(AGENT, services);
    beat.platform = platform;
    beat
}

/// The verb and arguments an order is, as the agent reads them.
pub(crate) fn verb_of(order: Option<Order>) -> Option<agent::order::Verb> {
    order.and_then(|order| order.to_proto().verb)
}

/// The `update` of `service` to `version` from the package `package_digest`, whose index has the
/// digest `index_digest` (empty: none).
pub(crate) fn update_verb(
    service: &str,
    version: &str,
    package_digest: &str,
    index_digest: &str,
) -> Option<agent::order::Verb> {
    Some(agent::order::Verb::Update(agent::Update {
        service: service.to_owned(),
        version: version.to_owned(),
        package_digest: package_digest.to_owned(),
        index_digest: index_digest.to_owned(),
    }))
}

/// The `install` of `service` at `version` from the package `package_digest`, whose index has
/// the digest `index_digest` (empty: none).
pub(crate) fn install_verb(
    service: &str,
    version: &str,
    package_digest: &str,
    index_digest: &str,
) -> Option<agent::order::Verb> {
    Some(agent::order::Verb::Install(agent::Install {
        service: service.to_owned(),
        version: version.to_owned(),
        package_digest: package_digest.to_owned(),
        index_digest: index_digest.to_owned(),
    }))
}

/// The `force` of `service` to `version` from the package `package_digest`, whose index has the
/// digest `index_digest` (empty: none), under the halt generation `halt_generation`.
pub(crate) fn force_verb(
    service: &str,
    version: &str,
    package_digest: &str,
    index_digest: &str,
    halt_generation: u64,
) -> Option<agent::order::Verb> {
    Some(agent::order::Verb::Force(agent::Force {
        service: service.to_owned(),
        version: version.to_owned(),
        package_digest: package_digest.to_owned(),
        index_digest: index_digest.to_owned(),
        halt_generation,
    }))
}
