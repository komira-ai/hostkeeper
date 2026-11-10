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
/// ([`crate::facts`]).
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
];
