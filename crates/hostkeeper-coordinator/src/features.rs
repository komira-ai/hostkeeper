//! The features this coordinator advertises ([`FEATURES`]).

use hostkeeper_wire::wire::ORDER_FEATURE;

use crate::{halt, verdict_file};

/// The features this coordinator advertises to an agent that announces `agent_features`.
///
/// Each name is added in the change that implements what it names, so a name is never
/// advertised for behaviour that is not there. `deferred-v1`: a `deferred` report ends the
/// order, holds no slot and backs the host off ([`crate::defer`]). `halt-v1`: the clear
/// generations of [`crate::halt`]. `heartbeat-v2`: selector membership ([`crate::membership`]).
/// `forced-v1`: the `force` order of a standing force ([`crate::force`]), which a daemon runs
/// only while this is advertised. `order-v1`: the order terms of [`crate::order_terms`], and the
/// agent itself, never the legacy service, as the heartbeat's top level. `verdict-acks-v1`: the
/// acks of the verdicts stored ([`crate::verdict_file`]). `grpc-v1`: the agent API is served on
/// the heartbeat listener ([`crate::grpc`]), so an agent may send its next heartbeat there.
/// `target-v1`: the installers of a heartbeat over gRPC are read, and the entry of a release
/// picked for the host goes in its orders over gRPC ([`crate::picks`]). `repositories-v1`: the
/// report of a gRPC heartbeat is read and the host is sent its set ([`crate::repository_push`]).
/// `facts-v1`: the `facts_generation` of a gRPC heartbeat is read, and the host is sent its facts
/// (host id, host group, labels) in every answer until it reports their generation
/// ([`crate::facts`]). `operator-keys-v1`: the `operator_keys` report of a gRPC heartbeat is read,
/// and the host is sent the operator keys of its host group until it reports their digest
/// ([`crate::operator_keys`]). `prerequisites-v1`: the `prerequisites` report of a gRPC heartbeat
/// is read, and the host is sent the host prerequisites of its host group until it reports their
/// digest ([`crate::prerequisites`]). `ssh-v1`: the `ssh` report of a gRPC heartbeat is read, and
/// the host is sent its sshd access (the CA public keys of its class, its principal and login user,
/// the password policy and its revoked serials) in every answer until it reports its generation
/// ([`crate::ssh_heartbeat`]); advertised always, and acted on only while the coordinator holds an
/// SSH user CA. `maintenance-v1`: the `maintenance` report of a heartbeat is read, kept with the host
/// and shown in `Hosts.List` and `Hosts.Get` ([`crate::maintenance`]), and the host is sent the
/// `maintain` that starts its turn of a maintenance run over gRPC ([`crate::decide_maintenance`]).
pub const FEATURES: &[&str] = &[
    "deferred-v1",
    halt::FEATURE,
    crate::membership::HEARTBEAT_V2,
    crate::force::FEATURE,
    ORDER_FEATURE,
    verdict_file::FEATURE,
    crate::grpc::GRPC_FEATURE,
    crate::picks::TARGET_FEATURE,
    crate::repository_push::FEATURE,
    crate::facts::FEATURE,
    crate::operator_keys::FEATURE,
    crate::ssh_heartbeat::FEATURE,
    crate::prerequisites::FEATURE,
    crate::maintenance::FEATURE,
];

/// The features of [`FEATURES`] advertised in the answer to a gRPC heartbeat of every agent that
/// announces features, whether or not it named them: `maintenance-v1`. A daemon names it only once
/// a reply has advertised it, so that the names it sends a coordinator older than `ssh-v1` stay
/// within the 16 that coordinator reads; echoed back only when named, it would never be agreed.
/// What it gates on the coordinator's side (the `maintenance` report kept, the `maintain` order)
/// still waits for a heartbeat that announces it. An older agent drops a name it does not know.
pub(crate) const OFFERED: &[&str] = &[crate::maintenance::FEATURE];

/// The fields of the answer to a heartbeat that go only to an agent that announced a feature,
/// each with the feature: the one place that says which. An older agent reads the answer
/// strictly and refuses a field it does not know, so a field listed here is left out of the
/// answer to an agent that did not announce its feature, and to every agent while this
/// coordinator does not advertise it ([`FEATURES`]). A field not listed is sent to every agent.
pub const GATED_FIELDS: &[(&str, &str)] = &[
    ("halts", halt::FEATURE),
    ("order_id", ORDER_FEATURE),
    ("order_try", ORDER_FEATURE),
    ("package_digest", ORDER_FEATURE),
    ("acks", verdict_file::FEATURE),
];

/// Whether the answer field `field` goes to an agent whose announced features this coordinator
/// also advertises are `agreed` (none for an agent that announced none): yes for a field
/// [`GATED_FIELDS`] does not list, and for one it lists only when its feature is agreed.
#[must_use]
pub fn sends(field: &str, agreed: Option<&[String]>) -> bool {
    GATED_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .is_none_or(|(_, feature)| agreed.is_some_and(|names| names.iter().any(|n| n == feature)))
}
