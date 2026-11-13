//! `Services.Put` and `Services.Remove` through `crate::grpc::route`: gRPC and the JSON gateway
//! agree on each answer and on each refusal's status and token, and the registered package stands
//! in the ops file a later start reads. No socket.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use serde_json::Value;
use tonic::Code;

use super::operator::World;
use super::support::decode;
use super::targets::{gateway, grpc, refused};
use crate::repositories_support::repository;
use crate::testutil::{OPERATOR, ci_token, ops_of};

const PUT: &str = "/hostkeeper.operator.v1.Services/Put";
const REMOVE: &str = "/hostkeeper.operator.v1.Services/Remove";
const REPOSITORIES_PUT: &str = "/hostkeeper.operator.v1.Repositories/Put";

/// The put of `js-agent` into `repository`, as a gRPC message.
fn put_message(repository: &str) -> proto::PutServiceRequest {
    proto::PutServiceRequest {
        service: "js-agent".to_owned(),
        repository: repository.to_owned(),
        reason: "registered".to_owned(),
    }
}

/// The remove of `js-agent`, as a gRPC message.
fn remove_message() -> proto::RemoveServiceRequest {
    proto::RemoveServiceRequest {
        service: "js-agent".to_owned(),
        reason: "gone".to_owned(),
    }
}

/// Puts the repository `spare` over gRPC.
async fn put_spare(world: &World) {
    let put = proto::PutRepositoryRequest {
        repository: Some(repository(
            "spare",
            "https://spare.example.com/apt",
            &["testing"],
            2,
        )),
        reason: "for a test".to_owned(),
    };
    let (code, message, _) = grpc(world, REPOSITORIES_PUT, Some(OPERATOR), put).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
}

/// Proves: over gRPC and over the JSON gateway, a put into a repository the registry lacks is
/// `repository_unknown` (NOT_FOUND, the gateway's 404 with its closed token), a CI token is
/// `forbidden`, no bearer `unauthorized`, a remove of a package nothing registers is
/// `unknown_service`; a put into `spare`, once the registry holds it, answers the audit entry
/// `service_put` and a remove the entry `service_remove`, the same over both, and the ops file of
/// each world holds the package after the put and not after the remove.
///
/// Catches: a method missing from one wire, a refusal that reaches a caller under another code or
/// token, or an adapter that answers without the handler's entry.
#[tokio::test]
async fn put_and_remove_are_answered_and_refused_alike_over_both_wires() {
    let by_grpc = World::new("sp-wire-grpc");
    let by_json = World::new("sp-wire-json");
    put_spare(&by_grpc).await;
    put_spare(&by_json).await;
    let before = by_grpc.ops_files();

    let (code, message, _) = grpc(&by_grpc, PUT, Some(OPERATOR), put_message("nowhere")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "repository_unknown")
    );
    let body = r#"{"service":"js-agent","repository":"nowhere","reason":"registered"}"#;
    let (status, text) = gateway(&by_json, PUT, OPERATOR, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text, refused("NOT_FOUND", "repository_unknown"));
    let (code, message, _) = grpc(&by_grpc, PUT, Some(&ci_token()), put_message("spare")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::PermissionDenied, "forbidden")
    );
    let (code, message, _) = grpc(&by_grpc, PUT, None, put_message("spare")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
    let (code, message, _) = grpc(&by_grpc, REMOVE, Some(OPERATOR), remove_message()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "unknown_service")
    );
    assert_eq!(by_grpc.ops_files(), before);

    let (code, _, answer) = grpc(&by_grpc, PUT, Some(OPERATOR), put_message("spare")).await;
    assert_eq!(code, Code::Ok);
    let done: proto::PutServiceResponse = decode(&answer.expect("an answer"));
    let entry = done.entry.expect("an entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.service.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "service_put",
            Some("js-agent"),
            Some("repository spare: registered")
        )
    );
    let body = r#"{"service":"js-agent","repository":"spare","reason":"registered"}"#;
    let (status, text) = gateway(&by_json, PUT, OPERATOR, body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["entry"]["action"], "service_put");
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());
    for world in [&by_grpc, &by_json] {
        let reopened = ops_of(&world.rig.dir).state();
        assert_eq!(
            reopened.services["js-agent"].repository.as_deref(),
            Some("spare")
        );
    }

    let (code, _, answer) = grpc(&by_grpc, REMOVE, Some(OPERATOR), remove_message()).await;
    assert_eq!(code, Code::Ok);
    let done: proto::RemoveServiceResponse = decode(&answer.expect("an answer"));
    assert_eq!(done.entry.expect("an entry").action, "service_remove");
    let body = r#"{"service":"js-agent","reason":"gone"}"#;
    let (status, text) = gateway(&by_json, REMOVE, OPERATOR, body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["entry"]["action"], "service_remove");
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());
    for world in [&by_grpc, &by_json] {
        assert!(
            !ops_of(&world.rig.dir)
                .state()
                .services
                .contains_key("js-agent")
        );
    }
}
