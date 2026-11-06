//! A `HeartbeatRequest` of `hostkeeper.agent.v1` as the JSON heartbeat of `hostkeeper_wire::wire`,
//! the bytes `POST /v1/heartbeat` reads.
//!
//! The coordinator reads a heartbeat in one place ([`crate::http::beat`]), so the gRPC
//! `Heartbeat` is turned into the JSON it reads and no rule is written twice: what the JSON
//! reader drops (a bad optional fact, a bad `services` list) or refuses, it drops or refuses
//! here. A fact the request leaves unspecified is left out of the JSON, which is how the JSON
//! says it too; a required one left unspecified (the platform, a health, an outcome) is a
//! missing key, which the reader refuses or drops with its list.
//!
//! The JSON route's top level is the entry of `services` named by the coordinator's legacy
//! service, when the coordinator has one and the heartbeat does not announce `order-v1`. So
//! that entry is the JSON top level and the others are `services`. In every other case the top
//! level is the agent itself, as `hostkeeper-link` sends it on the JSON route (the agent's
//! version, health `healthy`, no update) and every entry is in `services`, which is sent even
//! when it is empty. A fact that has no field in the JSON (the log tail, the detail and start of
//! an update, a pin) is left out. The host's clock and certificate (`sent_unix`,
//! `cert_not_after_unix`, `last_renew_error`) are top-level keys, each only when the request
//! sets it.

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_wire::wire::ORDER_FEATURE;
use serde_json::{Map, Value, json};

/// The JSON heartbeat of `request` for a coordinator whose legacy service is `legacy`.
pub(crate) fn json(request: &proto::HeartbeatRequest, legacy: Option<&str>) -> Vec<u8> {
    let order_v1 = request.features.iter().any(|name| name == ORDER_FEATURE);
    let top = legacy
        .filter(|_| !order_v1)
        .and_then(|name| request.services.iter().find(|entry| entry.name == name));
    let mut object = Map::new();
    if let Some(platform) = platform(request.platform()) {
        object.insert("platform".to_owned(), platform.into());
    }
    match top {
        Some(entry) => object.extend(facts(entry)),
        None => {
            object.insert("health".to_owned(), "healthy".into());
            if let Some(agent) = &request.agent {
                object.insert("version".to_owned(), agent.version.clone().into());
            }
        }
    }
    let services: Vec<Value> = request
        .services
        .iter()
        .filter(|entry| top.is_none_or(|top| !std::ptr::eq(*entry, top)))
        .map(|entry| {
            let mut one = facts(entry);
            one.insert("name".to_owned(), entry.name.clone().into());
            Value::Object(one)
        })
        .collect();
    object.insert("services".to_owned(), services.into());
    if !request.features.is_empty() {
        object.insert("agent_features".to_owned(), request.features.clone().into());
    }
    if let Some(agent) = request
        .agent
        .as_ref()
        .filter(|agent| !agent.name.is_empty() && !agent.version.is_empty())
    {
        object.insert(
            "agent".to_owned(),
            json!({ "name": agent.name, "version": agent.version }),
        );
    }
    if let Some(sent) = request.sent_unix {
        object.insert("sent_unix".to_owned(), sent.into());
    }
    if let Some(not_after) = request.cert_not_after_unix {
        object.insert("cert_not_after_unix".to_owned(), not_after.into());
    }
    if let Some(error) = &request.last_renew_error {
        object.insert("last_renew_error".to_owned(), error.clone().into());
    }
    serde_json::to_vec(&Value::Object(object)).unwrap_or_default()
}

/// What the entry says of its service, as the JSON keys of the top level and of a `services`
/// entry share them (the name is not among them).
fn facts(entry: &proto::ServiceBeat) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("version".to_owned(), entry.version.clone().into());
    if let Some(health) = health(entry.health()) {
        object.insert("health".to_owned(), health.into());
    }
    if let Some(update) = &entry.last_update {
        object.insert("last_update".to_owned(), update_result(update));
    }
    if let Some(target) = &entry.updating {
        object.insert("updating".to_owned(), target.clone().into());
    }
    if let Some(step) = &entry.step {
        object.insert(
            "step".to_owned(),
            json!({ "name": step.name, "attempt": step.attempt }),
        );
    }
    if let Some(commit) = commit(entry.commit()) {
        object.insert("commit".to_owned(), commit.into());
    }
    if !entry.step_secs.is_empty() {
        object.insert("step_secs".to_owned(), json!(entry.step_secs));
    }
    if entry.promotion_pending {
        object.insert("promotion_pending".to_owned(), true.into());
    }
    if let Some(deferred) = &entry.deferred {
        object.insert(
            "deferred".to_owned(),
            json!({
                "target": deferred.target,
                "reason": deferred.reason,
                "retry_after_secs": deferred.retry_after_secs,
            }),
        );
    }
    object
}

/// The last update as the JSON `last_update`.
fn update_result(update: &proto::UpdateResult) -> Value {
    let mut object = Map::new();
    object.insert("target".to_owned(), update.target.clone().into());
    if let Some(outcome) = outcome(update.outcome()) {
        object.insert("outcome".to_owned(), outcome.into());
    }
    if let Some(phase) = phase(update.phase()) {
        object.insert("phase".to_owned(), phase.into());
    }
    if let Some(class) = class(update.class()) {
        object.insert("class".to_owned(), class.into());
    }
    if !update.reason.is_empty() {
        object.insert("reason".to_owned(), update.reason.clone().into());
    }
    if let Some(id) = &update.order_id {
        object.insert("order_id".to_owned(), id.clone().into());
    }
    if let Some(order_try) = update.order_try {
        object.insert("order_try".to_owned(), order_try.into());
    }
    Value::Object(object)
}

/// The platform's name on the JSON route.
fn platform(platform: proto::Platform) -> Option<&'static str> {
    match platform {
        proto::Platform::Unspecified => None,
        proto::Platform::LinuxX8664 => Some("linux-x86_64"),
        proto::Platform::LinuxArm64 => Some("linux-arm64"),
        proto::Platform::DarwinArm64 => Some("darwin-arm64"),
    }
}

/// The health's name on the JSON route.
fn health(health: proto::Health) -> Option<&'static str> {
    match health {
        proto::Health::Unspecified => None,
        proto::Health::Healthy => Some("healthy"),
        proto::Health::Unhealthy => Some("unhealthy"),
        proto::Health::Absent => Some("absent"),
    }
}

/// The outcome's name on the JSON route.
fn outcome(outcome: proto::Outcome) -> Option<&'static str> {
    match outcome {
        proto::Outcome::Unspecified => None,
        proto::Outcome::Succeeded => Some("succeeded"),
        proto::Outcome::Failed => Some("failed"),
    }
}

/// The failure phase's name on the JSON route.
fn phase(phase: proto::FailurePhase) -> Option<&'static str> {
    match phase {
        proto::FailurePhase::Unspecified => None,
        proto::FailurePhase::BeforeCommit => Some("before_commit"),
        proto::FailurePhase::GateRelease => Some("gate_release"),
        proto::FailurePhase::GateEnvironment => Some("gate_environment"),
        proto::FailurePhase::AfterCommit => Some("after_commit"),
        proto::FailurePhase::Halt => Some("halt"),
    }
}

/// The failure class's name on the JSON route.
fn class(class: proto::FailureClass) -> Option<&'static str> {
    match class {
        proto::FailureClass::Unspecified => None,
        proto::FailureClass::Release => Some("release"),
        proto::FailureClass::Environment => Some("environment"),
    }
}

/// The commit state's name on the JSON route.
fn commit(commit: proto::CommitState) -> Option<&'static str> {
    match commit {
        proto::CommitState::Unspecified => None,
        proto::CommitState::Uncommitted => Some("uncommitted"),
        proto::CommitState::Committed => Some("committed"),
    }
}
