//! `Repositories` on the operator API and what hangs on it, through `crate::grpc::route`: gRPC and
//! the JSON gateway agree, the 256 KiB of `Put` against the 4 KiB of the other methods, a set of
//! targets that names a repository the registry lacks, and the repositories a gRPC heartbeat is
//! answered with. No socket.

use base64::Engine as _;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry;
use hyper::StatusCode;
use prost::Message as _;
use serde_json::{Value, json};
use tonic::Code;

use super::heartbeat::{beat, farm, json_beat, updater};
use super::operator::World;
use super::support::decode;
use super::targets::{gateway, grpc, grpc_bytes, padded, refused};
use crate::http::{MAX_OPERATOR_BYTES, MAX_SET_DESIRED_BYTES};
use crate::repositories_support::{
    fingerprint, keyring_of_size, komira, repository, repository_entry, targets_of,
};
use crate::testutil::{LEGACY, OPERATOR};

const PUT: &str = "/hostkeeper.operator.v1.Repositories/Put";
const GET: &str = "/hostkeeper.operator.v1.Repositories/Get";
const LIST: &str = "/hostkeeper.operator.v1.Repositories/List";
const REMOVE: &str = "/hostkeeper.operator.v1.Repositories/Remove";
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";

fn put_message(repository: Repository) -> proto::PutRepositoryRequest {
    proto::PutRepositoryRequest {
        repository: Some(repository),
        reason: "first".to_owned(),
    }
}

/// `repository` as the proto3 JSON of a `Put` body: the keyring as base64.
fn put_body(repository: &Repository, reason: &str) -> String {
    json!({
        "repository": {
            "name": repository.name,
            "kind": repository.kind,
            "uri": repository.uri,
            "suites": repository.suites,
            "components": repository.components,
            "fingerprints": repository.fingerprints,
            "keyring": base64::engine::general_purpose::STANDARD.encode(&repository.keyring),
        },
        "reason": reason,
    })
    .to_string()
}

/// Proves: a `Put` and a `Remove` over gRPC and the same two over the JSON gateway on twin
/// coordinators leave the same repositories file and the same ops and audit files, and answer the
/// same: the gateway's answer is the gRPC answer as proto3 JSON.
///
/// Catches: a gateway that reads the body another way than the message, or an adapter that writes
/// the registry or the audit differently from the other.
#[tokio::test]
async fn put_and_remove_by_the_gateway_and_by_grpc_leave_the_same_files() {
    let by_grpc = World::new("repositories-grpc");
    let by_json = World::new("repositories-json");
    let komira = komira(1);

    let (code, message, answer) =
        grpc(&by_grpc, PUT, Some(OPERATOR), put_message(komira.clone())).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let put: proto::PutRepositoryResponse = decode(&answer.expect("an answer"));
    let (status, text) = gateway(&by_json, PUT, OPERATOR, &put_body(&komira, "first")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let said: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(said["repository"]["generation"], "1");
    assert_eq!(
        said["repository"]["keyringSha256"],
        put.repository.expect("held").keyring_sha256
    );
    assert_eq!(said["entry"]["action"], "repository_put");

    let remove = proto::RemoveRepositoryRequest {
        name: "komira".to_owned(),
        reason: "retired".to_owned(),
    };
    // The kbf service names komira: it is in use, over both.
    let (code, message, _) = grpc(&by_grpc, REMOVE, Some(OPERATOR), remove.clone()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::FailedPrecondition, "in_use")
    );
    let (status, text) = gateway(
        &by_json,
        REMOVE,
        OPERATOR,
        r#"{"name":"komira","reason":"retired"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(text, refused("FAILED_PRECONDITION", "in_use"));

    let spare = repository("spare", "https://spare.example.com/", &["x"], 2);
    grpc(&by_grpc, PUT, Some(OPERATOR), put_message(spare.clone())).await;
    gateway(&by_json, PUT, OPERATOR, &put_body(&spare, "first")).await;
    let spare_out = proto::RemoveRepositoryRequest {
        name: "spare".to_owned(),
        ..remove
    };
    let (code, _, _) = grpc(&by_grpc, REMOVE, Some(OPERATOR), spare_out).await;
    assert_eq!(code, Code::Ok);
    let (status, text) = gateway(
        &by_json,
        REMOVE,
        OPERATOR,
        r#"{"name":"spare","reason":"retired"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("JSON")["generation"],
        "3"
    );

    assert_eq!(
        by_grpc.file("desired.json.repositories"),
        by_json.file("desired.json.repositories")
    );
    assert!(by_grpc.file("desired.json.repositories").is_some());
    assert_eq!(by_grpc.ops_files(), by_json.ops_files());
}

/// Proves: `Get` and `List` answer the registry over gRPC and the gateway alike, a refusal is its
/// token over both (`unknown_repository` is `NOT_FOUND` and 404), and CI's token and none are
/// `forbidden` and `unauthorized` before the body is read.
///
/// Catches: a refusal that changes with the route, or a read open to a token that may not.
#[tokio::test]
async fn get_and_list_and_their_refusals_are_the_same_on_both_routes() {
    let world = World::new("repositories-reads");
    grpc(&world, PUT, Some(OPERATOR), put_message(komira(1))).await;

    let (code, _, answer) = grpc(
        &world,
        GET,
        Some(OPERATOR),
        proto::GetRepositoryRequest {
            name: "komira".to_owned(),
        },
    )
    .await;
    assert_eq!(code, Code::Ok);
    let got: proto::GetRepositoryResponse = decode(&answer.expect("an answer"));
    assert_eq!(got.repository.expect("held").repository, Some(komira(1)));
    let (status, text) = gateway(&world, GET, OPERATOR, r#"{"name":"komira"}"#).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("JSON")["repository"]["repository"]["name"],
        "komira"
    );
    let (status, text) = gateway(&world, LIST, OPERATOR, "{}").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let listed: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(listed["generation"], "1");
    assert!(
        listed["repositories"][0]["repository"]
            .get("keyring")
            .is_none()
    );

    let (code, message, _) = grpc(
        &world,
        GET,
        Some(OPERATOR),
        proto::GetRepositoryRequest {
            name: "absent".to_owned(),
        },
    )
    .await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "unknown_repository")
    );
    let (status, text) = gateway(&world, GET, OPERATOR, r#"{"name":"absent"}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(text, refused("NOT_FOUND", "unknown_repository"));

    for path in [LIST, GET, PUT, REMOVE] {
        // A CI token is good once: each call has its own.
        let ci = crate::testutil::ci_token();
        let (code, message, _) =
            grpc(&world, path, Some(&ci), proto::ListRepositoriesRequest {}).await;
        assert_eq!(
            (code, message.as_str()),
            (Code::PermissionDenied, "forbidden"),
            "{path}"
        );
        let (code, message, _) = grpc(&world, path, None, proto::ListRepositoriesRequest {}).await;
        assert_eq!(
            (code, message.as_str()),
            (Code::Unauthenticated, "unauthorized"),
            "{path}"
        );
    }
}

/// Proves: a `Put` message of 256 KiB is read over gRPC and one byte more is refused `OUT_OF_RANGE`
/// with the limit in the message; a `Get` of 4 KiB plus one byte is refused at 4 KiB; on the
/// gateway a `Put` body of 256 KiB is read and one byte more is `invalid_request` with nothing put,
/// and a keyring of the 64 KiB the rules allow, which is 87,382 characters of base64, is put.
///
/// Catches: a limit that would refuse the keyring a repository may hold, or one that is not held.
#[tokio::test]
async fn a_put_reads_256_kib_and_every_other_method_4_kib() {
    let world = World::new("repositories-limits");
    let base = put_message(komira(1)).encode_to_vec();
    let (code, message, _) = grpc_bytes(
        &world,
        PUT,
        Some(OPERATOR),
        padded(base.clone(), MAX_SET_DESIRED_BYTES),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (code, message, _) = grpc_bytes(
        &world,
        PUT,
        Some(OPERATOR),
        padded(base, MAX_SET_DESIRED_BYTES + 1),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 262144 bytes"), "{message}");
    let get = proto::GetRepositoryRequest {
        name: "komira".to_owned(),
    }
    .encode_to_vec();
    let (code, message, _) = grpc_bytes(
        &world,
        GET,
        Some(OPERATOR),
        padded(get, MAX_OPERATOR_BYTES + 1),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 4096 bytes"), "{message}");

    let body = put_body(
        &repository("other", "https://o.example.com/", &["x"], 2),
        "second",
    );
    let spaced = |size: usize| format!("{body}{}", " ".repeat(size - body.len()));
    let (status, text) = gateway(&world, PUT, OPERATOR, &spaced(MAX_SET_DESIRED_BYTES + 1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(text, refused("INVALID_ARGUMENT", "invalid_request"));
    let (status, text) = gateway(&world, PUT, OPERATOR, &spaced(MAX_SET_DESIRED_BYTES)).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let mut big = repository("big", "https://big.example.com/", &["x"], 3);
    big.keyring = keyring_of_size(3, registry::MAX_KEYRING);
    assert_eq!(big.keyring.len(), registry::MAX_KEYRING);
    big.fingerprints = vec![fingerprint(&big.keyring)];
    let (status, text) = gateway(&world, PUT, OPERATOR, &put_body(&big, "big")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

/// Proves: a `SetDesired` with a repository entry is refused `repository_unknown` over gRPC
/// (`FAILED_PRECONDITION`) and the gateway (400), with nothing set, while the registry lacks the
/// repository the service names; once it is put the same set lands.
///
/// Catches: a set accepted for a repository no host can be sent, and a refusal that differs by
/// route.
#[tokio::test]
async fn a_set_with_a_repository_entry_is_refused_until_the_repository_is_put() {
    let world = World::new("repositories-set");
    let request = proto::SetDesiredRequest {
        service: LEGACY.to_owned(),
        desired: Some(super::operator::state("2.0", &[])),
        targets: targets_of(vec![repository_entry(LEGACY, "komira-testing")])
            .into_iter()
            .collect(),
        ..proto::SetDesiredRequest::default()
    };
    let (code, message, _) = grpc(&world, SET_DESIRED, Some(OPERATOR), request.clone()).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::FailedPrecondition, "repository_unknown")
    );
    assert!(world.coordinator.desired.get(LEGACY).is_none());

    let (code, _, _) = grpc(&world, PUT, Some(OPERATOR), put_message(komira(1))).await;
    assert_eq!(code, Code::Ok);
    let (code, message, _) = grpc(&world, SET_DESIRED, Some(OPERATOR), request.clone()).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    assert!(world.coordinator.desired.get(LEGACY).is_some());

    let other = World::new("repositories-set-json");
    let body = json_of(&request, "hostkeeper.operator.v1.SetDesiredRequest");
    let (status, text) = gateway(&other, SET_DESIRED, OPERATOR, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(text, refused("FAILED_PRECONDITION", "repository_unknown"));
}

/// `message`, a `name`, as proto3 canonical JSON.
fn json_of(message: &impl prost::Message, name: &str) -> String {
    let pool = prost_reflect::DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("the pool");
    let descriptor = pool.get_message_by_name(name).expect("the message");
    let dynamic =
        prost_reflect::DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice())
            .expect("decoded");
    serde_json::to_string(&dynamic).expect("JSON")
}

fn heartbeat(report: Option<agent::RepositoriesReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        platform: agent::Platform::LinuxArm64 as i32,
        agent: Some(agent::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: ["heartbeat-v2", "repositories-v1"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        services: vec![agent::ServiceBeat {
            name: "kbf".to_owned(),
            version: "2.0".to_owned(),
            health: agent::Health::Healthy as i32,
            ..agent::ServiceBeat::default()
        }],
        repositories: report,
        ..agent::HeartbeatRequest::default()
    }
}

/// Proves: the answer to a heartbeat over gRPC from a daemon that announced `repositories-v1`
/// carries the set its services name once the registry holds it (generation, digest, entries), and
/// not once the daemon's next heartbeat reports that digest in force; the answer to the same
/// daemon's heartbeat over the JSON route never carries it; a heartbeat that does not announce the
/// feature is answered none.
///
/// Catches: a set that goes to the JSON route, to a daemon that did not ask for it, or at every
/// heartbeat.
#[tokio::test]
async fn a_grpc_heartbeat_is_answered_the_set_until_the_daemon_reports_it() {
    let farm = farm("repositories-heartbeat").await;
    let who = updater("d1", "daemon");
    let me = farm.coordinator.caller(Some(OPERATOR)).await;
    farm.coordinator
        .repositories_put(&me, put_message(komira(1)))
        .expect("put");

    let answer: agent::HeartbeatResponse = beat(&farm.coordinator, &who, &heartbeat(None))
        .await
        .decode();
    let set = answer.repositories.expect("the set");
    // The farm's own sets put `komira` at generation 1 (they name its channels); the operator's
    // put replaces it at generation 2.
    assert_eq!(set.generation, 2);
    assert_eq!(set.entries, [komira(1)]);
    assert_eq!(set.digest, registry::digest(&[komira(1)]));

    let held = agent::RepositoriesReport {
        generation: set.generation,
        digest: set.digest,
        refused: None,
    };
    let answer: agent::HeartbeatResponse = beat(&farm.coordinator, &who, &heartbeat(Some(held)))
        .await
        .decode();
    assert_eq!(answer.repositories, None);

    let mut plain = heartbeat(None);
    plain.features = vec!["heartbeat-v2".to_owned()];
    let answer: agent::HeartbeatResponse =
        beat(&farm.coordinator, &updater("d2", "daemon"), &plain)
            .await
            .decode();
    assert_eq!(answer.repositories, None);

    let json = r#"{"platform":"linux-x86_64","version":"2.0","health":"healthy","agent_features":["heartbeat-v2","repositories-v1"]}"#;
    let (code, answered) = json_beat(&farm.coordinator, &updater("d3", "daemon"), json).await;
    assert_eq!(code, 200);
    assert!(answered.get("repositories").is_none(), "{answered}");
}
