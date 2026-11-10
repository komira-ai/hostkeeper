//! `hostkeeper.agent.v1.Coordinator` at the coordinator: what `Heartbeat` turns a message into
//! (`beat_map`), how a heartbeat the coordinator cannot read is refused, and how `Enroll`,
//! `Poll` and `Renew` reach the enrollment, or are told it is not there.

use std::collections::HashMap;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::poll_message;
use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::Coordinator as Api;
use hostkeeper_wire::wire::read_heartbeat;
use prost::Message;
use serde_json::{Value, json};
use tonic::{Code, Request};

use super::heartbeat::{
    Reply, beat, farm, farm_enrolled, grpc_call, legacy_beat, peer_of, updater,
};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, approve, enroll_request};
use crate::grpc::Admitted;
use crate::grpc::agent::AgentService;
use crate::grpc::beat_map;
use crate::http::{Peer, Surface};
use crate::service::Coordinator;

// ---- the JSON a message is turned into ------------------------------------------------------

/// The JSON `beat_map` makes of `request` for a coordinator with the legacy service `legacy`.
fn json_of(request: &proto::HeartbeatRequest, legacy: Option<&str>) -> Value {
    serde_json::from_slice(&beat_map::json(request, legacy)).expect("JSON")
}

fn entry(name: &str, version: &str) -> proto::ServiceBeat {
    proto::ServiceBeat {
        name: name.to_owned(),
        version: version.to_owned(),
        ..proto::ServiceBeat::default()
    }
}

fn agent(name: &str, version: &str) -> Option<proto::AgentInfo> {
    Some(proto::AgentInfo {
        name: name.to_owned(),
        version: version.to_owned(),
    })
}

fn request(
    agent: Option<proto::AgentInfo>,
    features: &[&str],
    services: Vec<proto::ServiceBeat>,
) -> proto::HeartbeatRequest {
    proto::HeartbeatRequest {
        installers: Vec::new(),
        platform: proto::Platform::LinuxArm64 as i32,
        agent,
        features: features.iter().map(|name| (*name).to_owned()).collect(),
        services,
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root_age_secs: None,
        trusted_root_reason: 0,
    }
}

/// The single service entry of `service` as JSON, among the entries of an agent heartbeat.
fn service_json(service: proto::ServiceBeat) -> Value {
    let request = request(agent("hostkeeper-daemon", "1.0"), &[], vec![service]);
    json_of(&request, None)["services"][0].clone()
}

/// Proves which entry is the JSON top level. A coordinator with a legacy service reads the
/// agent's `kbf` entry as the top level when the agent did not announce `order-v1` (the other
/// entries stay in `services`); it reads the agent itself as the top level when the agent
/// announced it, when the coordinator has no legacy service, and when no entry has its name.
#[test]
fn the_legacy_services_entry_is_the_top_level_unless_the_agent_announced_order_v1() {
    let mut unhealthy = entry("other", "2.0");
    unhealthy.health = proto::Health::Unhealthy as i32;
    let mut healthy_kbf = entry("kbf", "1.0");
    healthy_kbf.health = proto::Health::Healthy as i32;
    let listed = vec![healthy_kbf.clone(), unhealthy.clone()];
    let other = json!({"name": "other", "version": "2.0", "health": "unhealthy"});
    let kbf = json!({"name": "kbf", "version": "1.0", "health": "healthy"});

    let legacy_top = request(None, &[], listed.clone());
    assert_eq!(
        json_of(&legacy_top, Some("kbf")),
        json!({
            "platform": "linux-arm64", "version": "1.0", "health": "healthy",
            "services": [other.clone()],
        })
    );

    let with_agent = request(
        agent("hostkeeper-daemon", "1.2"),
        &["order-v1"],
        listed.clone(),
    );
    let agent_top = json!({
        "platform": "linux-arm64", "version": "1.2", "health": "healthy",
        "services": [kbf.clone(), other.clone()],
        "agent_features": ["order-v1"],
        "agent": {"name": "hostkeeper-daemon", "version": "1.2"},
    });
    assert_eq!(json_of(&with_agent, Some("kbf")), agent_top);

    let without_legacy = request(
        agent("hostkeeper-daemon", "1.2"),
        &["order-v1"],
        listed.clone(),
    );
    assert_eq!(json_of(&without_legacy, None), agent_top);

    let no_such_entry = request(agent("hostkeeper-daemon", "1.2"), &[], vec![unhealthy]);
    assert_eq!(
        json_of(&no_such_entry, Some("kbf")),
        json!({
            "platform": "linux-arm64", "version": "1.2", "health": "healthy",
            "services": [other],
            "agent": {"name": "hostkeeper-daemon", "version": "1.2"},
        })
    );
}

/// Proves what is left out when the message leaves it out: an agent with no version, or a
/// name or version that is empty, is no `agent` key; an agent top level with no agent has no
/// version, which the JSON reader refuses; no features is no `agent_features`; no services is
/// an empty list, sent all the same.
#[test]
fn what_the_message_leaves_out_the_json_leaves_out() {
    let none = request(None, &[], Vec::new());
    let sent = json_of(&none, None);
    assert_eq!(
        sent,
        json!({"platform": "linux-arm64", "health": "healthy", "services": []})
    );
    let bytes = beat_map::json(&none, None);
    assert!(read_heartbeat(&bytes, None).is_err(), "no version to read");

    for (name, version) in [("", "1.0"), ("hostkeeper-daemon", "")] {
        let sent = json_of(&request(agent(name, version), &[], Vec::new()), None);
        assert!(sent.get("agent").is_none(), "{name:?} {version:?}");
    }
    let sent = json_of(
        &request(agent("hostkeeper-daemon", ""), &[], Vec::new()),
        None,
    );
    assert_eq!(sent["version"], "");
    let sent = json_of(
        &request(agent("hostkeeper-daemon", "1.0"), &[], Vec::new()),
        None,
    );
    assert_eq!(
        sent["agent"],
        json!({"name": "hostkeeper-daemon", "version": "1.0"})
    );
    assert!(sent.get("agent_features").is_none());
}

/// Proves the platform's names, and that a platform the message does not set or sets to a value
/// the proto does not know is left out (the JSON reader then refuses the heartbeat).
#[test]
fn each_platform_has_its_json_name_and_an_unknown_one_has_none() {
    for (platform, name) in [
        (proto::Platform::LinuxX8664, "linux-x86_64"),
        (proto::Platform::LinuxArm64, "linux-arm64"),
        (proto::Platform::DarwinArm64, "darwin-arm64"),
    ] {
        let mut message = request(None, &[], Vec::new());
        message.platform = platform as i32;
        assert_eq!(json_of(&message, None)["platform"], name);
    }
    for unknown in [proto::Platform::Unspecified as i32, 99] {
        let mut message = request(None, &[], Vec::new());
        message.platform = unknown;
        assert!(
            json_of(&message, None).get("platform").is_none(),
            "{unknown}"
        );
    }
}

/// Proves a service entry with nothing but a name and a version is those two keys, and one with
/// every fact is every key the JSON route has for it: a health, the last update (its outcome,
/// phase, class, reason and order), the update running, its step, commit state and step
/// timings, the promotion, a deferral, and the try the step record holds. The facts that have no JSON key (a pin, an update's
/// start, force, detail and log tail) are left out.
#[test]
fn a_service_entry_is_the_json_entry_of_its_facts() {
    assert_eq!(
        service_json(entry("svc", "1.0")),
        json!({"name": "svc", "version": "1.0"})
    );
    let full = proto::ServiceBeat {
        installer: None,
        drift: None,
        name: "svc".to_owned(),
        version: "3.1".to_owned(),
        health: proto::Health::Absent as i32,
        last_update: Some(proto::UpdateResult {
            target: "3.1".to_owned(),
            outcome: proto::Outcome::Failed as i32,
            phase: proto::FailurePhase::AfterCommit as i32,
            class: proto::FailureClass::Environment as i32,
            reason: "disk_tainted".to_owned(),
            started_unix: Some(5),
            order_try: Some(2),
            order_id: Some("update-0123456789abcdef0123456789abcdef".to_owned()),
            forced: true,
            detail: Some("detail".to_owned()),
            log_tail: vec!["line".to_owned()],
        }),
        updating: Some("3.2".to_owned()),
        step: Some(proto::StepRun {
            name: "verify".to_owned(),
            attempt: 2,
        }),
        commit: proto::CommitState::Committed as i32,
        step_secs: HashMap::from([("cutover".to_owned(), 41)]),
        promotion_pending: true,
        deferred: Some(proto::DeferredReport {
            target: "3.0".to_owned(),
            reason: "drain_bound".to_owned(),
            retry_after_secs: 300,
        }),
        pinned: Some(proto::PinReport {
            commit: Some("abc".to_owned()),
        }),
        record_try: Some(2),
    };
    let want = json!({
        "name": "svc", "version": "3.1", "health": "absent",
        "last_update": {
            "target": "3.1", "outcome": "failed", "phase": "after_commit",
            "class": "environment", "reason": "disk_tainted",
            "order_id": "update-0123456789abcdef0123456789abcdef", "order_try": 2,
        },
        "updating": "3.2",
        "step": {"name": "verify", "attempt": 2},
        "commit": "committed",
        "step_secs": {"cutover": 41},
        "promotion_pending": true,
        "deferred": {"target": "3.0", "reason": "drain_bound", "retry_after_secs": 300},
        "record_try": 2,
    });
    assert_eq!(service_json(full.clone()), want);

    // The JSON reader takes every fact as it is written.
    let message = request(agent("hostkeeper-daemon", "1.0"), &["order-v1"], vec![full]);
    let bytes = beat_map::json(&message, None);
    let read = read_heartbeat(&bytes, None).expect("a heartbeat");
    assert_eq!(read.services_dropped, None);
    assert_eq!(read.extras_dropped, None);
    assert_eq!(read.beat.agent_features, ["order-v1"]);
    let listed = read.beat.services.expect("the services");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "svc");
    assert_eq!(
        listed[0].last_update.as_ref().and_then(|u| u.order_try),
        Some(2)
    );
    assert_eq!(listed[0].record_try, Some(2));
}

/// Proves every enum reaches its JSON word, and an unset one or one the proto does not know
/// leaves the key out: the healths, the outcomes, the failure phases and classes, the commit
/// states.
#[test]
fn each_enum_value_has_its_json_word_and_an_unset_one_has_none() {
    for (health, word) in [
        (proto::Health::Healthy, "healthy"),
        (proto::Health::Unhealthy, "unhealthy"),
        (proto::Health::Absent, "absent"),
    ] {
        let mut one = entry("svc", "1.0");
        one.health = health as i32;
        assert_eq!(service_json(one)["health"], word);
    }
    for (commit, word) in [
        (proto::CommitState::Uncommitted, "uncommitted"),
        (proto::CommitState::Committed, "committed"),
    ] {
        let mut one = entry("svc", "1.0");
        one.commit = commit as i32;
        assert_eq!(service_json(one)["commit"], word);
    }
    for unset in [0, 99] {
        let mut one = entry("svc", "1.0");
        one.health = unset;
        one.commit = unset;
        let sent = service_json(one);
        assert!(sent.get("health").is_none() && sent.get("commit").is_none());
    }

    let last = |result: proto::UpdateResult| {
        let mut one = entry("svc", "1.0");
        one.last_update = Some(result);
        service_json(one)["last_update"].clone()
    };
    for (outcome, word) in [
        (proto::Outcome::Succeeded, "succeeded"),
        (proto::Outcome::Failed, "failed"),
    ] {
        let result = proto::UpdateResult {
            outcome: outcome as i32,
            ..proto::UpdateResult::default()
        };
        assert_eq!(last(result)["outcome"], word);
    }
    for (phase, word) in [
        (proto::FailurePhase::BeforeCommit, "before_commit"),
        (proto::FailurePhase::GateRelease, "gate_release"),
        (proto::FailurePhase::GateEnvironment, "gate_environment"),
        (proto::FailurePhase::AfterCommit, "after_commit"),
        (proto::FailurePhase::Halt, "halt"),
    ] {
        let result = proto::UpdateResult {
            phase: phase as i32,
            ..proto::UpdateResult::default()
        };
        assert_eq!(last(result)["phase"], word);
    }
    for (class, word) in [
        (proto::FailureClass::Release, "release"),
        (proto::FailureClass::Environment, "environment"),
    ] {
        let result = proto::UpdateResult {
            class: class as i32,
            ..proto::UpdateResult::default()
        };
        assert_eq!(last(result)["class"], word);
    }
    for unset in [0, 99] {
        let result = proto::UpdateResult {
            target: "1.0".to_owned(),
            outcome: unset,
            phase: unset,
            class: unset,
            ..proto::UpdateResult::default()
        };
        assert_eq!(last(result), json!({"target": "1.0"}));
    }
}

/// Proves that the entry named by the legacy service is the top level with its facts and no
/// name, and the rest keep theirs: the top level has the entry's last update and deferral.
#[test]
fn the_top_level_entry_carries_its_facts_without_its_name() {
    let mut kbf = entry("kbf", "1.0");
    kbf.health = proto::Health::Healthy as i32;
    kbf.updating = Some("2.0".to_owned());
    kbf.deferred = Some(proto::DeferredReport {
        target: "2.0".to_owned(),
        reason: "drain_bound".to_owned(),
        retry_after_secs: 60,
    });
    let message = request(None, &[], vec![kbf]);
    assert_eq!(
        json_of(&message, Some("kbf")),
        json!({
            "platform": "linux-arm64", "version": "1.0", "health": "healthy",
            "updating": "2.0",
            "deferred": {"target": "2.0", "reason": "drain_bound", "retry_after_secs": 60},
            "services": [],
        })
    );
}

// ---- the adapter ------------------------------------------------------------------------------

const PATHS: [&str; 3] = [
    "/hostkeeper.agent.v1.Coordinator/Enroll",
    "/hostkeeper.agent.v1.Coordinator/Poll",
    "/hostkeeper.agent.v1.Coordinator/Renew",
];

async fn call<M: Message>(
    coordinator: &Arc<Coordinator>,
    peer: &Peer,
    path: &str,
    message: &M,
) -> Reply {
    grpc_call(
        coordinator,
        Surface::Heartbeat,
        peer,
        path,
        None,
        &message.encode_to_vec(),
    )
    .await
}

/// Proves a heartbeat the coordinator cannot read is refused `INVALID_ARGUMENT` with the token
/// `invalid_heartbeat`, as the JSON route answers 400: a message with no platform, and a
/// message whose services list is dropped from an agent that has no top level to fall back on.
#[tokio::test]
async fn a_heartbeat_the_coordinator_cannot_read_is_invalid_heartbeat() {
    let farm = farm("grpc-agent-invalid").await;
    let who = updater("d1", "daemon");
    let no_platform = proto::HeartbeatRequest::default();
    let reply = beat(&farm.coordinator, &who, &no_platform).await;
    assert_eq!(reply.token(), (Code::InvalidArgument, "invalid_heartbeat"));

    let mut bad_service = request(agent("hostkeeper-daemon", "1.0"), &["order-v1"], Vec::new());
    bad_service.services.push(entry("Not A Name", "1.0"));
    let reply = beat(&farm.coordinator, &who, &bad_service).await;
    assert_eq!(reply.token(), (Code::InvalidArgument, "invalid_heartbeat"));
    assert!(farm.heard_from().0.is_empty());
}

/// Proves a heartbeat is answered with the response of the decision: the order of an agent
/// that announced `order-v1`, and for one that announced nothing no features and no typed
/// order, as the JSON answer has no `features` for it.
#[tokio::test]
async fn a_heartbeat_is_answered_with_the_order_and_the_features_the_decision_names() {
    let farm = farm("grpc-agent-response").await;
    let who = updater("d1", "daemon");
    let mut running = entry("kbf", "1.0");
    running.health = proto::Health::Healthy as i32;
    running.installer = Some("apt".to_owned());
    let mut announced = request(
        agent("hostkeeper-daemon", "1.0"),
        &["order-v1", "unknown-v9", "target-v1"],
        vec![running],
    );
    announced.installers = vec!["apt".to_owned()];
    let reply = beat(&farm.coordinator, &who, &announced).await;
    let response: proto::HeartbeatResponse = reply.decode();
    assert_eq!(response.features, ["order-v1", "target-v1"]);
    let order = response.order.expect("an update order");
    assert_eq!(order.r#try, 1);
    assert!(order.order_id.starts_with("update-"));
    match order.verb {
        Some(proto::order::Verb::Update(update)) => {
            assert_eq!(
                (update.service.as_str(), update.version.as_str()),
                ("kbf", "2.0")
            );
            assert_eq!(update.package_digest, "1".repeat(64));
            let target = update.target.expect("the entry picked for the host");
            assert_eq!(target.installer, "apt");
        }
        other => panic!("not an update: {other:?}"),
    }

    let plain = beat(
        &farm.coordinator,
        &updater("d2", "daemon"),
        &legacy_beat("2.0", None),
    )
    .await;
    let response: proto::HeartbeatResponse = plain.decode();
    assert_eq!(response, proto::HeartbeatResponse::default());
}

/// Proves the adapter does not trust its caller to have been checked: a `Heartbeat` that reaches
/// it without the host the router admitted, or with another kind of admission, is refused
/// `forbidden`; so is a `Renew` that reaches it with no certificate recorded.
#[tokio::test]
async fn the_adapter_refuses_a_call_the_router_did_not_admit() {
    let farm = farm("grpc-agent-direct").await;
    let service = AgentService::new(Arc::clone(&farm.coordinator));

    let status = service
        .heartbeat(Request::new(legacy_beat("1.0", None)))
        .await
        .expect_err("refused");
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden")
    );

    let mut wrong = Request::new(legacy_beat("1.0", None));
    wrong.extensions_mut().insert(Admitted::Peer(None));
    let status = service.heartbeat(wrong).await.expect_err("refused");
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden")
    );
    assert!(farm.heard_from().0.is_empty());

    // The farm has no enrollment, so a renewal that reaches the adapter with no certificate
    // recorded is told the enrollment is not there.
    let status = service
        .renew(Request::new(proto::RenewRequest::default()))
        .await
        .expect_err("refused");
    assert_eq!((status.code(), status.message()), (Code::Unavailable, ""));
}

/// Proves a coordinator without enrollment answers `Enroll`, `Poll` and `Renew` `UNAVAILABLE`
/// with no message, whoever calls and whatever they send.
#[tokio::test]
async fn enroll_poll_and_renew_without_enrollment_are_unavailable() {
    let farm = farm("grpc-agent-no-enrollment").await;
    let key = HostKey::new();
    let some_cert = updater("d1", "daemon");
    let messages: [Vec<u8>; 3] = [
        enroll_request(&key, "machine-1").encode_to_vec(),
        proto::PollRequest::default().encode_to_vec(),
        proto::RenewRequest::default().encode_to_vec(),
    ];
    for who in [None, some_cert] {
        for (path, message) in PATHS.iter().zip(&messages) {
            let reply = grpc_call(
                &farm.coordinator,
                Surface::Heartbeat,
                &who,
                path,
                None,
                message,
            )
            .await;
            assert_eq!(reply.token(), (Code::Unavailable, ""), "{path}");
        }
    }
}

/// Proves the three calls are the enrollment's own: a host enrolls, is polled for a nonce, signs
/// it after approval and is issued its certificate, and renews with that certificate, each
/// answer being what the enrollment gives; and the enrollment's refusals arrive as the status
/// whose message is the token (a bad CSR, a request nobody made).
#[tokio::test]
async fn enroll_poll_and_renew_are_the_enrollments_own_answers() {
    let enrollment = EnrollmentRig::new("grpc-agent-flow");
    let farm = farm_enrolled("grpc-agent-flow-coordinator", &enrollment.enrollment).await;
    let c = &farm.coordinator;
    let key = HostKey::new();
    let request_id = key.key_hash().to_hex();

    let enrolled = call(c, &None, PATHS[0], &enroll_request(&key, "machine-1")).await;
    let enrolled: proto::EnrollResponse = enrolled.decode();
    assert_eq!(enrolled.request_id, request_id);
    assert_eq!(enrolled.state, proto::EnrollState::Pending as i32);

    let ask = proto::PollRequest {
        request_id: request_id.clone(),
        ..proto::PollRequest::default()
    };
    let challenge: proto::PollResponse = call(c, &None, PATHS[1], &ask).await.decode();
    assert_eq!(challenge.state, proto::PollState::Unspecified as i32);
    assert!(!challenge.nonce.is_empty(), "a nonce to sign");

    approve(&enrollment.enrollment, &key, "s1", false);
    let signed = proto::PollRequest {
        request_id,
        signature: key.sign(&poll_message(&key.key_hash(), &challenge.nonce)),
        nonce: challenge.nonce,
    };
    let issued: proto::PollResponse = call(c, &None, PATHS[1], &signed).await.decode();
    assert_eq!(issued.state, proto::PollState::Issued as i32);
    assert_eq!(issued.host_id, "s1");
    assert!(!issued.certificate_der.is_empty());
    assert!(
        issued
            .ca_bundle_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );

    let renewal = proto::RenewRequest { csr_der: key.csr() };
    let who = peer_of(&issued.certificate_der);
    let renewed: proto::RenewResponse = call(c, &who, PATHS[2], &renewal).await.decode();
    assert!(!renewed.certificate_der.is_empty());
    assert_ne!(renewed.certificate_der, issued.certificate_der);
    assert_eq!(renewed.ca_bundle_pem, issued.ca_bundle_pem);

    let bad = proto::EnrollRequest {
        csr_der: b"not a CSR".to_vec(),
        ..proto::EnrollRequest::default()
    };
    let reply = call(c, &None, PATHS[0], &bad).await;
    assert_eq!(reply.token(), (Code::InvalidArgument, "bad_csr"));
    let nobody = call(c, &None, PATHS[1], &proto::PollRequest::default()).await;
    assert_eq!(nobody.token(), (Code::NotFound, "unknown_request"));
}

/// Proves a `Renew` without a certificate of this CA is refused `PERMISSION_DENIED` `forbidden`
/// whether it has none or another's, and the enrollment's own check is the one that refuses:
/// an updater's certificate of the farm CA is no host certificate of the enrollment CA.
#[tokio::test]
async fn renew_without_a_certificate_of_the_enrollment_ca_is_forbidden() {
    let enrollment = EnrollmentRig::new("grpc-agent-renew");
    let farm = farm_enrolled("grpc-agent-renew-coordinator", &enrollment.enrollment).await;
    let renewal = proto::RenewRequest {
        csr_der: HostKey::new().csr(),
    };
    for who in [None, updater("d1", "daemon")] {
        let reply = call(&farm.coordinator, &who, PATHS[2], &renewal).await;
        assert_eq!(reply.token(), (Code::PermissionDenied, "forbidden"));
    }
}

/// Catches the host's clock or certificate lost between the gRPC message and the reader, put
/// under another key, or made up when the message has none: the three fields are top-level keys
/// the reader reads back, and an unset one is no key.
#[test]
fn the_host_clock_and_certificate_reach_the_reader() {
    let mut message = request(agent("hostkeeper", "1.0"), &["order-v1"], Vec::new());
    message.sent_unix = Some(2_000_000_123);
    message.cert_not_after_unix = Some(2_000_600_000);
    message.last_renew_error = Some("renewal refused: host_excluded".to_owned());
    let json = json_of(&message, None);
    assert_eq!(json["sent_unix"], json!(2_000_000_123_u64));
    assert_eq!(json["cert_not_after_unix"], json!(2_000_600_000_u64));
    assert_eq!(
        json["last_renew_error"],
        json!("renewal refused: host_excluded")
    );
    let read = read_heartbeat(&beat_map::json(&message, None), None).expect("a heartbeat");
    assert_eq!(read.beat.sent_unix, Some(2_000_000_123));
    assert_eq!(read.beat.cert_not_after_unix, Some(2_000_600_000));
    assert_eq!(
        read.beat.last_renew_error.as_deref(),
        Some("renewal refused: host_excluded")
    );

    let bare = json_of(&request(agent("hostkeeper", "1.0"), &[], Vec::new()), None);
    for key in ["sent_unix", "cert_not_after_unix", "last_renew_error"] {
        assert_eq!(bare.get(key), None, "{key}");
    }
}

/// Proves the `Enroll` route answers through the coordinator, which assigns what a nonce
/// approval approves: a Mac's request with a nonce issued for it, sent through the router to a
/// coordinator whose allowlist names its serial and whose gate says the device is enrolled and
/// supervised, is approved (answered with no waiting state), audited `enroll_approve` and then
/// `assign` by `mdm-nonce`, and its host is assigned `kbf`.
#[tokio::test]
async fn an_enroll_approved_by_its_nonce_assigns_the_entry_services() {
    use crate::assignment::Entry;
    use crate::enrollment::tests::support::{SERIAL, audit_after, audit_of, mac_request};
    use crate::enrollment::{DeviceStatus, FakeDeviceState, MacAllowlist, MacPolicy, os_random};
    use hostkeeper_enrollment::fs::RealFs;

    let gate = Arc::new(FakeDeviceState::default());
    gate.set(
        SERIAL,
        Ok(DeviceStatus {
            enrolled: true,
            supervised: true,
            enrolled_since_unix: None,
        }),
    );
    let policy = MacPolicy {
        device_state: gate,
        allowlist: MacAllowlist::parse(
            "[[mac]]\nserial = \"C02XK1ABJG5J\"\nhost_id_prefix = \"mac-\"\nrole = \"build\"\nservices = [\"kbf\"]\n",
        )
        .expect("an allowlist"),
    };
    let enrollment =
        EnrollmentRig::configured("grpc-agent-mac", Arc::new(RealFs), os_random(), |e| {
            e.with_mac(policy)
        });
    let farm = farm_enrolled("grpc-agent-mac-coordinator", &enrollment.enrollment).await;
    let c = &farm.coordinator;
    let key = HostKey::new();
    let nonce = enrollment
        .enrollment
        .issue_mac_nonce(SERIAL, &key.key_hash())
        .expect("a nonce");
    let before = audit_of(c).len();
    let request = mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes());
    let answer: proto::EnrollResponse = call(c, &None, PATHS[0], &request).await.decode();
    assert_eq!(answer.request_id, key.key_hash().to_hex());
    assert_eq!(answer.state, proto::EnrollState::Unspecified as i32);
    let actions: Vec<(String, String, Option<String>)> = audit_after(c, before)
        .into_iter()
        .map(|(by, action, node, _)| (by, action, node))
        .collect();
    let host = Some("mac-c02xk1abjg5j".to_owned());
    assert_eq!(
        actions,
        [
            (
                "coordinator".to_owned(),
                "enroll_requested".to_owned(),
                None
            ),
            (
                "mdm-nonce".to_owned(),
                "enroll_approve".to_owned(),
                host.clone()
            ),
            ("mdm-nonce".to_owned(), "assign".to_owned(), host),
        ]
    );
    let assigned = c.ops().state().assignments;
    assert_eq!(
        assigned["kbf"].hosts.get("mac-c02xk1abjg5j"),
        Some(&Entry::Include)
    );
}
