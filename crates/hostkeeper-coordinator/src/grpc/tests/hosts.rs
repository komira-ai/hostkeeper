//! `Hosts.List`, `Hosts.Get` and `Releases.UnmarkBad` end to end, through the router as the
//! private listener serves them: over a coordinator that enrolled a host and heard its
//! heartbeat on its certificate, by gRPC and by the gateway the UI calls.
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
use super::operator::World;
use super::support::{call, decode, grpc_request, raw};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::http::Surface;
use crate::ops::{OpsState, ServiceOps};
use crate::testutil::{OPERATOR, Rig};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// The reply of `Hosts.List` the UI's tests render, as this coordinator answers it.
const HOSTS_LIST: &str =
    include_str!("../../../../hostkeeper-ui-tests/tests/fixtures/hosts_list.json");

/// A coordinator of the recorded kbf service with the enrollment of `enrollment`, its audit
/// file's clock fixed.
fn world_over(name: &str, enrollment: &EnrollmentRig) -> World {
    let rig = Rig::new(name);
    let started =
        Rig::start(&rig.dir, &rig.clock).with_enrollment(Arc::clone(&enrollment.enrollment));
    World::over(rig, started)
}

/// The operator's gRPC call of `path` with `message`: the code, its message, and the reply.
async fn operator_call<M: Message + Default>(
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
/// its certificate, live, its heartbeat as it sent it, the labels the operator set, and kbf's
/// view of it holding the operator's hold; `Get` of a host never heard is `NOT_FOUND`
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
    let sent = proto_message(&[svc("kbf", "1.0")]);
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
