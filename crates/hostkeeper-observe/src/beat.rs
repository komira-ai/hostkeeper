//! What an observation puts in the heartbeat: the `Health` and the version of a `ServiceBeat`.
//! The hook's verdict is not an input: only what the daemon observed reaches the beat, so a hook
//! that says ok while the service is down never makes a healthy beat (design section 8.5).
//!
//! | Observation | Health | Version |
//! |---|---|---|
//! | `Absent` | `HEALTH_ABSENT` | empty |
//! | `Unmonitored` | `HEALTH_UNSPECIFIED` | recorded |
//! | `Stopped`, `Transitioning`, `Unobserved` | `HEALTH_UNHEALTHY` | recorded |
//! | `Live`, version `Mismatch` | `HEALTH_UNHEALTHY` | recorded |
//! | `Live`, health `Unhealthy` | `HEALTH_UNHEALTHY` | recorded |
//! | `Live`, health `Healthy` | `HEALTH_HEALTHY` | recorded |
//! | `Live`, no `[health]`, version not contradicted | `HEALTH_HEALTHY` | recorded |
//!
//! `HEALTH_UNHEALTHY` reads "does not, or the daemon cannot tell"; the reason apart
//! (`stopped`, `version_mismatch`, `unobserved`, ...) is [`Observation::reason_token`], for the
//! daemon's status.

use hostkeeper_proto::hostkeeper::agent::v1::Health;

use crate::observe::{HealthView, Observation, ServiceState, VersionView};

/// The `Health` of the beat, and its version (`None` for an empty one).
pub fn beat_health(observation: &Observation) -> (Health, Option<String>) {
    let health = match &observation.state {
        ServiceState::Absent => return (Health::Absent, None),
        ServiceState::Unmonitored => Health::Unspecified,
        ServiceState::Stopped | ServiceState::Transitioning | ServiceState::Unobserved { .. } => {
            Health::Unhealthy
        }
        ServiceState::Live => match (&observation.version, &observation.health) {
            (VersionView::Mismatch { .. }, _) | (_, HealthView::Unhealthy { .. }) => {
                Health::Unhealthy
            }
            _ => Health::Healthy,
        },
    };
    (health, observation.recorded.clone())
}
