//! The kbf JSON keys of a heartbeat and of its reply, and the field of `hostkeeper.agent.v1` the
//! coordinator's JSON adapter carries each in. `tests/kbf_json.rs` holds the recorded fixtures to
//! them, and `tests/wire.rs` pins them.

/// A service's facts, as kbf writes them at the top level (for its legacy service) and in each
/// entry of `services`: the JSON key, and the field of `ServiceBeat`.
pub const SERVICE_FACTS: &[(&str, &str)] = &[
    ("version", "version"),
    ("health", "health"),
    ("last_update", "last_update"),
    ("last_update.target", "last_update.target"),
    ("last_update.outcome", "last_update.outcome"),
    ("last_update.phase", "last_update.phase"),
    ("last_update.class", "last_update.class"),
    ("last_update.reason", "last_update.reason"),
    ("last_update.started_unix", "last_update.started_unix"),
    ("updating", "updating"),
    ("step", "step"),
    ("step.name", "step.name"),
    ("step.attempt", "step.attempt"),
    ("commit", "commit"),
    ("step_secs", "step_secs"),
    ("promotion_pending", "promotion_pending"),
    ("deferred", "deferred"),
    ("deferred.target", "deferred.target"),
    ("deferred.reason", "deferred.reason"),
    ("deferred.retry_after_secs", "deferred.retry_after_secs"),
    ("pinned", "pinned"),
    ("pinned.commit", "pinned.commit"),
];

/// The heartbeat's own keys: the JSON key, and the field of `HeartbeatRequest`.
pub const HEARTBEAT: &[(&str, &str)] = &[
    ("platform", "platform"),
    ("agent", "agent"),
    ("agent.name", "agent.name"),
    ("agent.version", "agent.version"),
    ("agent_features", "features"),
    ("services", "services"),
    ("services[].name", "services.name"),
];

/// The reply's keys: the JSON key, and the field of `HeartbeatResponse`. `reply` is `nothing`
/// (no `order`) or `update` (an `order`); its `service` and `version` are the arguments of the
/// order's verb, `update`, or `force` when kbf's reply carries `forced`.
pub const REPLY: &[(&str, &str)] = &[
    ("reply", "order"),
    ("service", "order.update.service"),
    ("version", "order.update.version"),
    ("features", "features"),
    ("halts", "halt_generations"),
    ("forced", "order.force"),
];
