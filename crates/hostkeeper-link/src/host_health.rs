//! The host's hardware health as the heartbeat carries it (`host-health-v1`,
//! docs/design/host-health-alerts.md section 4.1): the daemon's source of it
//! ([`HostHealthSource`]), which the loop asks for the health of each heartbeat to a coordinator
//! that advertises the feature, and tells which records the coordinator wrote. The daemon keeps
//! the records (`hostkeeper_daemon::host_health`); this crate only carries them. The JSON route
//! is frozen and sends nothing of it, but the heartbeat's bound counts it on both routes.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

/// What the daemon holds of its host's hardware health. A daemon that has one announces
/// `host-health-v1`, once the coordinator advertised it, and sends what [`HostHealthSource::health`]
/// gives in every heartbeat from then on.
pub trait HostHealthSource: Send + Sync {
    /// The health of the next heartbeat: the host's boot and its shutdown marker, and at most
    /// `hostkeeper_proto::health::MAX_EVENTS` of the records not yet written, the most severe
    /// first, then the oldest, within the bounds of `hostkeeper_proto::health::check_health`.
    /// Asked only for a heartbeat to a coordinator that advertises `host-health-v1`; it never
    /// waits for a source.
    fn health(&self) -> pb::HostHealth;

    /// The records the coordinator wrote to the host's log (`HeartbeatResponse.health_written`),
    /// by `event_id`: the source holds them no longer. An id it does not hold is passed over.
    fn written(&self, event_ids: &[String]);
}
