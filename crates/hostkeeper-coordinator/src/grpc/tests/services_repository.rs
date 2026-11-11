//! `Services.SetRepository` through `crate::grpc::route`: gRPC and the JSON gateway agree on the
//! answer and on each refusal's status and token, and the repository stands in the ops file a
//! later start reads. No socket.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_release::registry;
use hyper::StatusCode;
use serde_json::Value;
use tonic::Code;

use super::operator::World;
use super::support::decode;
use super::targets::{gateway, grpc, refused};
use crate::repositories_support::repository;
use crate::testutil::{OPERATOR, ci_token, ops_of};

const SET: &str = "/hostkeeper.operator.v1.Services/SetRepository";
const PUT: &str = "/hostkeeper.operator.v1.Repositories/Put";

/// The set of `kbf` to `repository`, as a gRPC message.
fn set_message(repository: &str) -> proto::SetRepositoryRequest {
    proto::SetRepositoryRequest {
        service: "kbf".to_owned(),
        repository: repository.to_owned(),
        reason: "moved".to_owned(),
    }
}

/// Puts the repository `spare` over gRPC.
async fn put_spare(world: &World) {
    let spare = repository("spare", "https://spare.example.com/apt", &["testing"], 2);
    assert!(!registry::is_private(&spare));
    let put = proto::PutRepositoryRequest {
        repository: Some(spare),
        reason: "for a test".to_owned(),
    };
    let (code, message, _) = grpc(world, PUT, Some(OPERATOR), put).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
}

/// Proves: over gRPC and over the JSON gateway, a set of a repository the registry lacks is
/// `repository_unknown` (NOT_FOUND over gRPC, the gateway's 404 with its closed token), a CI
/// token is `forbidden`, no bearer `unauthorized`, a name that breaks the grammar
/// `invalid_request`, and a set of `spare`, once the registry holds it, answers the audit entry
/// `set_repository` of `kbf` with the same reason over both; the ops file of each world holds the
/// repository afterwards and the ops file of a refused call is as it was.
///
/// Catches: a method missing from one wire, a refusal that reaches a caller under another code or
/// token, or an adapter that answers without the handler's entry.
#[tokio::test]
async fn set_repository_is_answered_and_refused_alike_over_both_wires() {
    let by_grpc = World::new("sr-wire-grpc");
    let by_json = World::new("sr-wire-json");
    put_spare(&by_grpc).await;
    put_spare(&by_json).await;
    let before = by_grpc.ops_files();

    let (code, message, _) = grpc(&by_grpc, SET, Some(OPERATOR), set_message("nowhere")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "repository_unknown")
    );
    let body = r#"{"service":"kbf","repository":"nowhere","reason":"moved"}"#;
    let (status, text) = gateway(&by_json, SET, OPERATOR, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text, refused("NOT_FOUND", "repository_unknown"));
    let (code, message, _) = grpc(&by_grpc, SET, Some(OPERATOR), set_message("Bad Name")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::InvalidArgument, "invalid_request")
    );
    let (code, message, _) = grpc(&by_grpc, SET, Some(&ci_token()), set_message("spare")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::PermissionDenied, "forbidden")
    );
    let (code, message, _) = grpc(&by_grpc, SET, None, set_message("spare")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
    assert_eq!(by_grpc.ops_files(), before);

    let (code, _, answer) = grpc(&by_grpc, SET, Some(OPERATOR), set_message("spare")).await;
    assert_eq!(code, Code::Ok);
    let done: proto::SetRepositoryResponse = decode(&answer.expect("an answer"));
    let entry = done.entry.expect("an entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.service.as_deref(),
            entry.reason.as_deref()
        ),
        (
            "set_repository",
            Some("kbf"),
            Some("repository spare: moved")
        )
    );
    let body = r#"{"service":"kbf","repository":"spare","reason":"moved"}"#;
    let (status, text) = gateway(&by_json, SET, OPERATOR, body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["entry"]["action"], "set_repository");
    assert_eq!(said["entry"]["reason"], "repository spare: moved");
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());

    for world in [&by_grpc, &by_json] {
        let reopened = ops_of(&world.rig.dir).state();
        assert_eq!(
            reopened.services["kbf"].repository.as_deref(),
            Some("spare")
        );
    }
}
