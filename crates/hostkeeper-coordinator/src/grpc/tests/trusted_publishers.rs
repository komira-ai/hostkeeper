//! `Services.ListTrustedPublishers`, `Services.PutTrustedPublisher` and
//! `Services.RemoveTrustedPublisher` through `crate::grpc::route`: gRPC and the JSON gateway agree
//! on each answer and on each refusal's status and token, and the list the state holds stands in
//! the ops file a later start reads. No socket.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use serde_json::Value;
use tonic::Code;

use super::operator::World;
use super::support::decode;
use super::targets::{gateway, grpc, refused};
use crate::testutil::{OPERATOR, ci_token, ops_of};

const LIST: &str = "/hostkeeper.operator.v1.Services/ListTrustedPublishers";
const PUT: &str = "/hostkeeper.operator.v1.Services/PutTrustedPublisher";
const REMOVE: &str = "/hostkeeper.operator.v1.Services/RemoveTrustedPublisher";

fn put_message(service: &str) -> proto::PutTrustedPublisherRequest {
    proto::PutTrustedPublisherRequest {
        service: service.to_owned(),
        repository: "acme/kbf".to_owned(),
        workflow: ".github/workflows/release.yml".to_owned(),
        r#ref: String::new(),
        environment: String::new(),
        reason: "trust it".to_owned(),
    }
}

/// Proves: over gRPC and over the JSON gateway, a list answers the entries the allow-list file
/// stands for, a put adds one and answers the audit entry `trusted_publisher_put` and the list
/// after it, a remove takes it out with `trusted_publisher_remove`, a package nobody knows is
/// `unknown_service`, a remove of an entry the package lacks is `publisher_unknown` (NOT_FOUND),
/// a CI token is `forbidden` and no bearer `unauthorized`; and the state's list stands in the ops
/// file after the put.
///
/// Catches: a method missing from one wire, a refusal that reaches a caller under another code or
/// token, or an adapter that answers without the handler's entry.
#[tokio::test]
async fn the_calls_are_answered_and_refused_alike_over_both_wires() {
    let by_grpc = World::new("tp-wire-grpc");
    let by_json = World::new("tp-wire-json");

    let list = proto::ListTrustedPublishersRequest {
        service: "kbf".to_owned(),
    };
    let (code, _, answer) = grpc(&by_grpc, LIST, Some(OPERATOR), list.clone()).await;
    assert_eq!(code, Code::Ok);
    let held: proto::ListTrustedPublishersResponse = decode(&answer.expect("an answer"));
    assert_eq!(held.publishers.len(), 1);
    assert_eq!(held.publishers[0].repository_id, "4242");
    let (status, text) = gateway(&by_json, LIST, OPERATOR, r#"{"service":"kbf"}"#).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["publishers"][0]["repositoryId"], "4242");

    let (code, message, _) = grpc(&by_grpc, PUT, Some(&ci_token()), put_message("kbf")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::PermissionDenied, "forbidden")
    );
    let (code, message, _) = grpc(&by_grpc, PUT, None, put_message("kbf")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
    let (code, message, _) = grpc(&by_grpc, PUT, Some(OPERATOR), put_message("nobody")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "unknown_service")
    );
    let missing = proto::RemoveTrustedPublisherRequest {
        service: "kbf".to_owned(),
        repository: "acme/kbf".to_owned(),
        workflow: ".github/workflows/release.yml".to_owned(),
        r#ref: "main".to_owned(),
        environment: String::new(),
        reason: "gone".to_owned(),
    };
    let (code, message, _) = grpc(&by_grpc, REMOVE, Some(OPERATOR), missing).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "publisher_unknown")
    );
    let body = r#"{"service":"kbf","repository":"acme/kbf","workflow":".github/workflows/release.yml","ref":"main","reason":"gone"}"#;
    let (status, text) = gateway(&by_json, REMOVE, OPERATOR, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text, refused("NOT_FOUND", "publisher_unknown"));

    let (code, _, answer) = grpc(&by_grpc, PUT, Some(OPERATOR), put_message("kbf")).await;
    assert_eq!(code, Code::Ok);
    let done: proto::PutTrustedPublisherResponse = decode(&answer.expect("an answer"));
    assert_eq!(
        done.entry.expect("an entry").action,
        "trusted_publisher_put"
    );
    assert_eq!(done.publishers.len(), 2);
    let body = r#"{"service":"kbf","repository":"acme/kbf","workflow":".github/workflows/release.yml","reason":"trust it"}"#;
    let (status, text) = gateway(&by_json, PUT, OPERATOR, body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["entry"]["action"], "trusted_publisher_put");
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());
    for world in [&by_grpc, &by_json] {
        let held = ops_of(&world.rig.dir).state().services["kbf"]
            .trusted_publishers
            .clone()
            .expect("held");
        assert_eq!(held.len(), 2);
        assert_eq!(held[1].git_ref, "refs/heads/main");
    }

    let remove = proto::RemoveTrustedPublisherRequest {
        service: "kbf".to_owned(),
        repository: "acme/kbf".to_owned(),
        workflow: ".github/workflows/release.yml".to_owned(),
        r#ref: "main".to_owned(),
        environment: String::new(),
        reason: "gone".to_owned(),
    };
    let (code, _, answer) = grpc(&by_grpc, REMOVE, Some(OPERATOR), remove).await;
    assert_eq!(code, Code::Ok);
    let done: proto::RemoveTrustedPublisherResponse = decode(&answer.expect("an answer"));
    assert_eq!(
        done.entry.expect("an entry").action,
        "trusted_publisher_remove"
    );
    assert_eq!(done.publishers.len(), 1);
    let body = r#"{"service":"kbf","repository":"acme/kbf","workflow":".github/workflows/release.yml","ref":"main","reason":"gone"}"#;
    let (status, text) = gateway(&by_json, REMOVE, OPERATOR, body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());
}
