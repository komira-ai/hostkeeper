//! Host health (docs/design/host-health-alerts.md): the name of the feature by which a daemon
//! reports its host's hardware health (the heartbeat's field `health`) and is answered the records
//! written (`HeartbeatResponse.health_written`).
//!
//! This coordinator reads none of it: [`FEATURE`] is in neither [`crate::features::FEATURES`]
//! nor `OFFERED`, so no daemon is told to send health, and a heartbeat that carries it anyway is
//! answered as one without. The feature is advertised by the change that keeps the hardware event
//! log and answers `health_written`.

/// The feature a daemon announces to report its host's hardware health, once a coordinator
/// advertises it.
pub const FEATURE: &str = "host-health-v1";
