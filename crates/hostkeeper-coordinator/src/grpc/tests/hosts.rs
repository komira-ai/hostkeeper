//! `Hosts.List`, `Hosts.Get`, `Hosts.Retire`, `Hosts.ClearRecord`, `Enrollment.Reject` and
//! `Releases.UnmarkBad` end to end, through the router as the private listener serves them: over
//! a coordinator that enrolled a host and heard its heartbeat on its certificate, by gRPC and by
//! the gateway the UI calls.
//!
//! The gateway's answer to `Hosts.List` here is the reply the UI's Fleet page is tested on
//! (`crates/hostkeeper-ui-tests/tests/fixtures/hosts_list.json`): the test below fails when
//! the coordinator's answer and that file part.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use prost::Message;
use serde_json::Value;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::operator::{AUDIT_UNIX, World};
use super::support::{call, decode, grpc_request, raw};
use crate::enrollment::tests::support::{
    HostKey, NOW, Rig as EnrollmentRig, enroll_request, enrolled,
};
use crate::http::Surface;
use crate::ops::{OpsState, ServiceOps};
use crate::testutil::{OPERATOR, Rig};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// The reply of `Hosts.List` the UI's tests render, as this coordinator answers it.
const HOSTS_LIST: &str =
    include_str!("../../../../hostkeeper-ui-tests/tests/fixtures/hosts_list.json");

/// A coordinator of the recorded kbf service with the enrollment of `enrollment`, its audit
/// file's clock fixed.
pub(super) fn world_over(name: &str, enrollment: &EnrollmentRig) -> World {
    let rig = Rig::new(name);
    let started =
        Rig::start(&rig.dir, &rig.clock).with_enrollment(Arc::clone(&enrollment.enrollment));
    World::over(rig, started)
}

/// The operator's gRPC call of `path` with `message`: the code, its message, and the reply.
pub(super) async fn operator_call<M: Message + Default>(
    world: &World,
    path: &str,
    message: impl Message,
) -> (Code, String, Option<M>) {
    let request = grpc_request(path, message, Some(OPERATOR));
    let (code, said, body) = call(&world.coordinator, Surface::Control, &None, request).await;
    (code, said, body.map(|bytes| decode::<M>(&bytes)))
}

/// The operator's gateway call of `path` with the JSON `body`: the status and the JSON reply.
async fn gateway(world: &World, path: &str, body: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let (status, _, bytes) = raw(&world.coordinator, Surface::Control, &None, request).await;
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

/// Proves: a host that enrolled and heartbeated on the certificate it was issued is listed by
/// `Hosts.List` and answered by `Hosts.Get` as one and the same host: the node id and role of
/// its certificate, live, its heartbeat as it sent it, the time that heartbeat arrived by the
/// enrollment's clock as its first, the labels the operator set, and kbf's view of it holding
/// the operator's hold; `Get` of a host never heard is `NOT_FOUND`
/// `unknown_host`; and the gateway's JSON of `Hosts.List` is the reply the UI's Fleet page is
/// tested on.
///
/// Catches: a list or a host read from somewhere other than what the heartbeat left, a host
/// keyed by what its message says rather than its certificate, and a UI test that renders a
/// reply this coordinator does not give.
#[tokio::test]
async fn list_and_get_answer_the_host_that_enrolled_and_heartbeated() {
    let enrollment = EnrollmentRig::new("grpc-hosts-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-hosts-list", &enrollment);
    let c = &world.coordinator;
    // The UI's fixture is the reply to a daemon that lists no installer and announces no
    // `target-v1`.
    let mut sent = proto_message(&[svc("kbf", "1.0")]);
    sent.installers.clear();
    sent.features.retain(|feature| feature != "target-v1");
    for service in &mut sent.services {
        service.installer = None;
    }
    assert_eq!(
        beat(c, &peer_of(&certificate), &sent).await.token(),
        (Code::Ok, "")
    );
    let labels = proto::SetLabelsRequest {
        node_id: "s1".to_owned(),
        labels: [("rack".to_owned(), "r1".to_owned())].into(),
    };
    let (code, _, _) = operator_call::<proto::SetLabelsResponse>(
        &world,
        "/hostkeeper.operator.v1.Hosts/SetLabels",
        labels,
    )
    .await;
    assert_eq!(code, Code::Ok);
    let hold = proto::HoldRequest {
        service: "kbf".to_owned(),
        scope: Some(proto::HoldScope {
            scope: Some(proto::hold_scope::Scope::Host("s1".to_owned())),
        }),
        reason: "disk swap".to_owned(),
    };
    let (code, _, held) =
        operator_call::<proto::HoldResponse>(&world, "/hostkeeper.operator.v1.Services/Hold", hold)
            .await;
    assert_eq!(code, Code::Ok);
    let held = held.and_then(|answer| answer.hold).expect("the hold");

    let (code, said, listed) = operator_call::<proto::ListHostsResponse>(
        &world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let listed = listed.expect("a reply");
    assert_eq!(listed.next_page_token, "");
    let [host] = listed.hosts.as_slice() else {
        panic!("one host: {:?}", listed.hosts);
    };
    assert_eq!(
        (host.node_id.as_str(), host.role.as_str(), host.live),
        ("s1", "storage", true)
    );
    assert_eq!(host.heartbeat.as_ref(), Some(&sent), "as it sent it");
    assert_eq!(host.labels, labels_of(&[("rack", "r1")]));
    assert_eq!(host.first_heartbeat_unix, Some(NOW), "its first heartbeat");
    assert_eq!(
        host.first_serving_unix,
        Some(NOW),
        "kbf healthy and counted"
    );
    let [kbf] = host.services.as_slice() else {
        panic!("kbf alone: {:?}", host.services);
    };
    assert_eq!(
        (kbf.service.as_str(), kbf.holds.as_slice()),
        ("kbf", &[held][..])
    );

    let get = proto::GetHostRequest {
        node_id: "s1".to_owned(),
    };
    let (code, _, got) =
        operator_call::<proto::GetHostResponse>(&world, "/hostkeeper.operator.v1.Hosts/Get", get)
            .await;
    assert_eq!(code, Code::Ok);
    assert_eq!(got.and_then(|answer| answer.host).as_ref(), Some(host));
    let ghost = proto::GetHostRequest {
        node_id: "ghost".to_owned(),
    };
    let (code, said, _) =
        operator_call::<proto::GetHostResponse>(&world, "/hostkeeper.operator.v1.Hosts/Get", ghost)
            .await;
    assert_eq!((code, said.as_str()), (Code::NotFound, "unknown_host"));

    let (status, json) = gateway(&world, "/hostkeeper.operator.v1.Hosts/List", "{}").await;
    assert_eq!(status, StatusCode::OK);
    let fixture: Value = serde_json::from_str(HOSTS_LIST).expect("the fixture is JSON");
    assert_eq!(
        json,
        fixture,
        "the UI's fixture is not this reply; it is:\n{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

fn labels_of(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// The ops state with what a change leaves beside its marks set aside: the audit ring, and a
/// service whose marks are all empty, which is what no marks at all are.
fn marks_of(mut state: OpsState) -> OpsState {
    state.audit.clear();
    state
        .services
        .retain(|_, marks| *marks != ServiceOps::default());
    state
}

/// Proves: `MarkBad` by gRPC and then `UnmarkBad` by the gateway, as the UI's button sends it,
/// leave the ops state as it was before the mark, with an `unmark_bad` entry under the operator
/// after the `mark_bad`; a second `UnmarkBad` is `FAILED_PRECONDITION` `not_marked_bad` (400
/// at the gateway) and changes nothing.
///
/// Catches: an unmark that leaves the mark, that changes another part of the ops state, or that
/// answers success for a version that is not marked.
#[tokio::test]
async fn unmark_bad_after_mark_bad_leaves_the_ops_state_as_it_was() {
    let rig = Rig::new("grpc-hosts-unmark");
    let started = Rig::start(&rig.dir, &rig.clock);
    let world = World::over(rig, started);
    let c = &world.coordinator;
    let before = c.ops.state();
    let mark = proto::MarkBadRequest {
        service: "kbf".to_owned(),
        version: "2.0".to_owned(),
        reason: "crashes on start".to_owned(),
    };
    let (code, _, _) = operator_call::<proto::MarkBadResponse>(
        &world,
        "/hostkeeper.operator.v1.Releases/MarkBad",
        mark,
    )
    .await;
    assert_eq!(code, Code::Ok);
    assert_ne!(marks_of(c.ops.state()), marks_of(before.clone()), "marked");

    let body = r#"{"service":"kbf","version":"2.0","reason":"the disk"}"#;
    let (status, json) = gateway(&world, "/hostkeeper.operator.v1.Releases/UnmarkBad", body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["entry"]["action"], "unmark_bad");
    assert_eq!(json["entry"]["by"], "ACTOR_OPERATOR");
    assert_eq!(json["entry"]["reason"], "2.0: the disk");
    let after = c.ops.state();
    assert_eq!(marks_of(after.clone()), marks_of(before));
    let actions: Vec<&str> = after.audit.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["mark_bad", "unmark_bad"]);

    let (status, json) = gateway(&world, "/hostkeeper.operator.v1.Releases/UnmarkBad", body).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"code": "FAILED_PRECONDITION", "reason": "not_marked_bad"})
        )
    );
    assert_eq!(c.ops.state(), after, "a refused unmark changes nothing");
}

/// Proves: what a host's gRPC heartbeat says of its certificate and clock reaches `Hosts.Get`
/// and `Hosts.List` through the router, as protobuf both ways: the certificate's end and the
/// renewal error of the last heartbeat, and the count of heartbeats whose time was more than the
/// skew bound from the coordinator's wall clock (one of the two sent here).
///
/// Catches: a field dropped between the mounted heartbeat handler and the operator's answer, or
/// encoded under another number than the one decoded.
#[tokio::test]
async fn get_and_list_answer_the_certificate_and_clock_the_grpc_heartbeat_said() {
    let enrollment = EnrollmentRig::new("grpc-hosts-cert-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-hosts-cert", &enrollment);
    let c = &world.coordinator;
    let mut sent = proto_message(&[svc("kbf", "1.0")]);
    sent.cert_not_after_unix = Some(AUDIT_UNIX + 86_400);
    sent.last_renew_error = Some("renewal refused: host_excluded".to_owned());
    for sent_unix in [AUDIT_UNIX + 3_600, AUDIT_UNIX] {
        sent.sent_unix = Some(sent_unix);
        assert_eq!(
            beat(c, &peer_of(&certificate), &sent).await.token(),
            (Code::Ok, "")
        );
    }

    let get = proto::GetHostRequest {
        node_id: "s1".to_owned(),
    };
    let (code, _, got) =
        operator_call::<proto::GetHostResponse>(&world, "/hostkeeper.operator.v1.Hosts/Get", get)
            .await;
    assert_eq!(code, Code::Ok);
    let host = got.and_then(|answer| answer.host).expect("the host");
    let said = |host: &proto::Host| {
        (
            host.cert_not_after_unix,
            host.last_renew_error.clone(),
            host.clock_skew_beats,
        )
    };
    let expected = (
        Some(AUDIT_UNIX + 86_400),
        Some("renewal refused: host_excluded".to_owned()),
        1,
    );
    assert_eq!(said(&host), expected);
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        &world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let hosts = listed.expect("a reply").hosts;
    assert_eq!(hosts.iter().map(said).collect::<Vec<_>>(), [expected]);
}

/// Proves: `Hosts.Retire` of a host that enrolled and heartbeated answers, by gRPC, its
/// `host_retire` entry with no update in flight, and the host's next heartbeat on its
/// certificate is refused `denied` (`PERMISSION_DENIED`); a second `Retire` by the gateway, as
/// the UI's Host page sends it, is 200 with no entry; `Retire` of a host id never enrolled is
/// `NOT_FOUND` `unknown_host` (404 at the gateway). `Enrollment.Reject` of a pending request by
/// the gateway, as the UI's Enrollment page sends it, is 200 with its `enroll_reject` entry,
/// and of a request nobody made 404 `unknown_request`.
///
/// Catches: a method the router does not serve, a gateway body the coordinator does not read
/// (a field the UI names that the proto does not), and an unknown id answered as success.
#[tokio::test]
async fn retire_and_reject_answer_by_grpc_and_the_gateway() {
    let enrollment = EnrollmentRig::new("grpc-hosts-retire");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let pending = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&pending, "machine-2"))
        .expect("waits");
    let world = world_over("grpc-hosts-retire-coordinator", &enrollment);
    let c = &world.coordinator;
    let sent = proto_message(&[svc("kbf", "1.0")]);
    assert_eq!(
        beat(c, &peer_of(&certificate), &sent).await.token(),
        (Code::Ok, "")
    );

    let retire = proto::RetireRequest {
        node_id: "s1".to_owned(),
        reason: "board failed".to_owned(),
    };
    let (code, said, answer) = operator_call::<proto::RetireResponse>(
        &world,
        "/hostkeeper.operator.v1.Hosts/Retire",
        retire,
    )
    .await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let answer = answer.expect("a reply");
    assert!(!answer.update_in_flight);
    assert_eq!(
        answer.entry.map(|entry| entry.action).as_deref(),
        Some("host_retire")
    );
    assert_eq!(
        beat(c, &peer_of(&certificate), &sent).await.token(),
        (Code::PermissionDenied, "denied")
    );

    let path = "/hostkeeper.operator.v1.Hosts/Retire";
    let (status, json) = gateway(&world, path, r#"{"nodeId":"s1","reason":"again"}"#).await;
    assert_eq!((status, json), (StatusCode::OK, serde_json::json!({})));
    let (status, json) = gateway(&world, path, r#"{"nodeId":"ghost"}"#).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({"code": "NOT_FOUND", "reason": "unknown_host"})
        )
    );

    let path = "/hostkeeper.operator.v1.Enrollment/Reject";
    let body = format!(r#"{{"requestId":"{}"}}"#, pending.key_hash());
    let (status, json) = gateway(&world, path, &body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["entry"]["action"], "enroll_reject");
    let body = format!(r#"{{"requestId":"{}"}}"#, HostKey::new().key_hash());
    let (status, json) = gateway(&world, path, &body).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({"code": "NOT_FOUND", "reason": "unknown_request"})
        )
    );
}

/// Proves: `Hosts.ClearRecord` of a host whose last heartbeat did not announce
/// `clear-record-v1` is `FAILED_PRECONDITION` `daemon_too_old` by gRPC and 400 at the gateway,
/// and the host's next heartbeat carries no order; once its heartbeat announces the feature, the
/// gateway's call answers the order id, and the host's next gRPC heartbeat carries that order as
/// the `clear_record` case of `Order` (try 1) and the one after carries none. A host id nobody
/// heard from is `NOT_FOUND` `unknown_host`.
///
/// Catches: a method the router does not serve, a refusal with another code or token, a body
/// the gateway does not read, and an order that does not reach the host's heartbeat answer on
/// the wire, or reaches it twice.
#[tokio::test]
async fn clear_record_answers_by_grpc_and_the_gateway_and_reaches_the_heartbeat() {
    use hostkeeper_proto::hostkeeper::agent::v1 as agent;

    let enrollment = EnrollmentRig::new("grpc-hosts-clear-record");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-hosts-clear-record-coordinator", &enrollment);
    let c = &world.coordinator;
    let peer = peer_of(&certificate);
    let today = proto_message(&[svc("kbf", "1.0")]);
    let mut announcing = today.clone();
    announcing
        .features
        .push(crate::clear_record::FEATURE.to_owned());
    let order_of =
        |reply: super::heartbeat::Reply| reply.decode::<agent::HeartbeatResponse>().order;
    assert_eq!(order_of(beat(c, &peer, &today).await), None);

    let path = "/hostkeeper.operator.v1.Hosts/ClearRecord";
    let request = proto::ClearRecordRequest {
        node_id: "s1".to_owned(),
        service: "kbf".to_owned(),
    };
    let (code, said, answer) =
        operator_call::<proto::ClearRecordResponse>(&world, path, request).await;
    assert_eq!(
        (code, said.as_str(), answer),
        (Code::FailedPrecondition, "daemon_too_old", None)
    );
    let body = r#"{"nodeId":"s1","service":"kbf"}"#;
    assert_eq!(
        gateway(&world, path, body).await,
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"code": "FAILED_PRECONDITION", "reason": "daemon_too_old"})
        )
    );
    assert_eq!(order_of(beat(c, &peer, &announcing).await), None);

    let (status, json) = gateway(&world, path, body).await;
    assert_eq!(status, StatusCode::OK);
    let order_id = json["orderId"].as_str().expect("an order id").to_owned();
    assert!(order_id.starts_with("clear-record-"), "{order_id}");
    assert_eq!(
        order_of(beat(c, &peer, &announcing).await),
        Some(agent::Order {
            order_id,
            r#try: 1,
            verb: Some(agent::order::Verb::ClearRecord(agent::ClearRecord {
                service: "kbf".to_owned(),
            })),
        })
    );
    assert_eq!(order_of(beat(c, &peer, &announcing).await), None);

    let ghost = proto::ClearRecordRequest {
        node_id: "ghost".to_owned(),
        service: "kbf".to_owned(),
    };
    let (code, said, _) = operator_call::<proto::ClearRecordResponse>(&world, path, ghost).await;
    assert_eq!((code, said.as_str()), (Code::NotFound, "unknown_host"));
}
