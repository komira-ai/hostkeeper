//! `Releases.SetDesired` with targets and `Releases.GetRelease` on the operator API: the 256 KiB
//! limit of a set on the gRPC route and the gateway (and 4 KiB for every other method and for
//! `POST /v1/desired`), the gateway's null step, the refusals the adapter maps, and who may read a
//! release. Driven in process through `crate::grpc::route`; no socket.

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use hostkeeper_proto::hostkeeper::release::v1::TargetList;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde_json::{Value, json};
use tonic::{Code, Request as GrpcRequest};

use super::operator::{World, post, state};
use super::support::{GET_RELEASE, SET_DESIRED, call, grpc_request, raw};
use crate::grpc::gateway::without_null_targets;
use crate::grpc::operator::{ReleasesService, set_refusal};
use crate::grpc::{Admitted, operator_limit};
use crate::http::{MAX_DESIRED_BYTES, MAX_OPERATOR_BYTES, MAX_SET_DESIRED_BYTES, Surface};
use crate::releases_tests::{entry, hex, list, targets};
use crate::service::SetError;
use crate::testutil::{LEGACY, OPERATOR, Rig, ci_token};

const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";

fn set_request(version: &str, with: &[&str]) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: LEGACY.to_owned(),
        desired: Some(state(version, &[])),
        targets: with
            .iter()
            .map(|platform| ((*platform).to_owned(), targets(LEGACY)[*platform].clone()))
            .collect(),
        ..proto::SetDesiredRequest::default()
    }
}

/// `message` followed by one field it does not define, as long as it takes to be `total` bytes.
fn padded(mut message: Vec<u8>, total: usize) -> Vec<u8> {
    fn varint(mut value: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = u8::try_from(value & 0x7F).expect("seven bits");
            value >>= 7;
            if value == 0 {
                bytes.push(low);
                return bytes;
            }
            bytes.push(low | 0x80);
        }
    }
    let need = total.checked_sub(message.len()).expect("room");
    // Field 15 (length-delimited): one tag byte, the length, and the bytes it says.
    let length = (0..need)
        .find(|n| 1 + varint(*n).len() + n == need)
        .expect("a padding of that size");
    message.push(0x7A);
    message.extend(varint(length));
    message.extend(std::iter::repeat_n(0u8, length));
    message
}

/// A gRPC call of `path` with `bytes` as the one message.
async fn grpc_bytes(
    world: &World,
    path: &str,
    bearer: Option<&str>,
    bytes: Vec<u8>,
) -> (Code, String, Option<Bytes>) {
    let request = super::support::grpc_request_raw(path, super::support::framed(&bytes), bearer);
    call(&world.coordinator, Surface::Control, &None, request).await
}

async fn grpc(
    world: &World,
    path: &str,
    bearer: Option<&str>,
    message: impl prost::Message,
) -> (Code, String, Option<Bytes>) {
    call(
        &world.coordinator,
        Surface::Control,
        &None,
        grpc_request(path, message, bearer),
    )
    .await
}

/// A gateway call of `path` with `body`; the status, the body text.
async fn gateway(world: &World, path: &str, bearer: &str, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let (status, _, bytes) = raw(&world.coordinator, Surface::Control, &None, request).await;
    (status, String::from_utf8(bytes.to_vec()).expect("text"))
}

fn refused(code: &str, reason: &str) -> String {
    format!(r#"{{"code":"{code}","reason":"{reason}"}}"#)
}

/// `list` as proto3 canonical JSON, as the gateway reads it.
fn list_json(list: &TargetList) -> Value {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the pool");
    let descriptor = pool
        .get_message_by_name("hostkeeper.release.v1.TargetList")
        .expect("the message");
    let dynamic =
        DynamicMessage::decode(descriptor, list.encode_to_vec().as_slice()).expect("decoded");
    serde_json::to_value(&dynamic).expect("JSON")
}

fn get(service: &str, version: &str) -> proto::GetReleaseRequest {
    proto::GetReleaseRequest {
        service: service.to_owned(),
        version: version.to_owned(),
    }
}

/// Proves: the operator limit of a path is 256 KiB for `SetDesired` and 4 KiB for every other
/// method, `GetRelease` and an unknown path included.
#[test]
fn only_set_desired_reads_256_kib() {
    assert_eq!(MAX_SET_DESIRED_BYTES, 256 * 1024);
    assert_eq!(MAX_OPERATOR_BYTES, 4096);
    assert_eq!(operator_limit(SET_DESIRED), 262_144);
    for path in [
        CONVERGE,
        GET_RELEASE,
        "/hostkeeper.operator.v1.Audit/List",
        "",
    ] {
        assert_eq!(operator_limit(path), 4096, "{path}");
    }
}

/// Proves: over gRPC a `SetDesired` message of 256 KiB is read and one byte more is refused
/// `OUT_OF_RANGE` with the limit in the message, while a `GetRelease` of 4 KiB plus one byte is
/// refused at the 4 KiB limit of every other method.
#[tokio::test]
async fn a_set_desired_message_is_read_up_to_256_kib_on_grpc() {
    let world = World::new("targets-grpc-limit");
    let base = set_request("2.0", &[]).encode_to_vec();
    let (code, message, _) = grpc_bytes(
        &world,
        SET_DESIRED,
        Some(OPERATOR),
        padded(base.clone(), MAX_SET_DESIRED_BYTES),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (code, message, _) = grpc_bytes(
        &world,
        SET_DESIRED,
        Some(OPERATOR),
        padded(base, MAX_SET_DESIRED_BYTES + 1),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 262144 bytes"), "{message}");

    let (code, message, _) = grpc_bytes(
        &world,
        GET_RELEASE,
        Some(OPERATOR),
        padded(get(LEGACY, "2.0").encode_to_vec(), MAX_OPERATOR_BYTES + 1),
    )
    .await;
    assert_eq!(code, Code::OutOfRange);
    assert!(message.contains("the limit is: 4096 bytes"), "{message}");
}

/// Proves: on the gateway a `SetDesired` body of 256 KiB is read and one byte more is refused
/// `invalid_request` with nothing set; the frozen `POST /v1/desired` still reads 4 KiB and
/// refuses 4 KiB plus one byte `bad_request`.
#[tokio::test]
async fn a_set_desired_body_is_read_up_to_256_kib_on_the_gateway() {
    let world = World::new("targets-gateway-limit");
    let body =
        r#"{"service":"kbf","desired":{"version":"2.0","minUp":{"storage":1,"frontend":0}}}"#;
    let spaced = |size: usize| format!("{body}{}", " ".repeat(size - body.len()));
    let (status, text) = gateway(
        &world,
        SET_DESIRED,
        OPERATOR,
        &spaced(MAX_SET_DESIRED_BYTES + 1),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(text, refused("INVALID_ARGUMENT", "invalid_request"));
    assert!(world.coordinator.desired.get(LEGACY).is_none());
    let (status, text) = gateway(
        &world,
        SET_DESIRED,
        OPERATOR,
        &spaced(MAX_SET_DESIRED_BYTES),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(world.coordinator.desired.get(LEGACY).is_some());

    let frozen = r#"{"version":"3.0","min_up":{"storage":0,"frontend":0}}"#;
    let (status, answer) = post(
        &world.coordinator,
        crate::http::DESIRED_PATH,
        &ci_token(),
        &format!("{frozen:<size$}", size = MAX_DESIRED_BYTES + 1),
    )
    .await;
    assert_eq!(
        (status, answer),
        (StatusCode::BAD_REQUEST, json!({"error": "bad_request"}))
    );
    let (status, _) = post(
        &world.coordinator,
        crate::http::DESIRED_PATH,
        &ci_token(),
        &format!("{frozen:<size$}", size = MAX_DESIRED_BYTES),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

fn bytes_of(text: &str) -> Bytes {
    Bytes::from(text.to_owned())
}

/// Proves: the null step passes a body that is not JSON, JSON that is not an object, an object
/// with no `targets`, a `targets` that is not an object, and a `targets` object with no `null`,
/// each unchanged byte for byte (odd spacing and key order included).
#[test]
fn the_null_step_passes_every_body_it_has_nothing_to_drop_from_unchanged() {
    for text in [
        "not json",
        "",
        "[1, 2]",
        "{\"service\": \"kbf\"}",
        "{\"targets\": null}",
        "{\"targets\": [null]}",
        "{\"targets\":  \"linux-x86_64\"}",
        "{ \"targets\" : { \"macos-arm64\" : { \"entries\" : [ ] } } , \"service\":\"kbf\" }",
    ] {
        let given = bytes_of(text);
        assert_eq!(
            without_null_targets(&given).expect("passes"),
            given,
            "{text:?}"
        );
    }
}

/// Proves: the null step drops the platforms of `targets` whose value is `null` and nothing
/// else: the rest of the body and the other platforms are as they were.
#[test]
fn the_null_step_drops_the_null_platforms_and_nothing_else() {
    let given = bytes_of(
        r#"{"service":"kbf","targets":{"linux-arm64":null,"macos-arm64":{"entries":[]},"linux-x86_64":null},"desired":{"version":"2.0"}}"#,
    );
    let out = without_null_targets(&given).expect("passes");
    let value: Value = serde_json::from_slice(&out).expect("JSON");
    assert_eq!(
        value,
        json!({
            "service": "kbf",
            "targets": {"macos-arm64": {"entries": []}},
            "desired": {"version": "2.0"},
        })
    );
}

/// Proves: a `targets` object that is empty from the start, or empty once its `null`s are dropped,
/// is refused `target_invalid` as `INVALID_ARGUMENT`.
#[test]
fn the_null_step_refuses_targets_that_name_no_platform() {
    for text in [
        r#"{"targets":{}}"#,
        r#"{"targets":{"linux-x86_64":null}}"#,
        r#"{"service":"kbf","targets":{"linux-arm64":null,"macos-arm64":null}}"#,
    ] {
        let refused = without_null_targets(&bytes_of(text)).expect_err(text);
        assert_eq!(refused.code(), Code::InvalidArgument, "{text}");
        assert_eq!(refused.message(), "target_invalid", "{text}");
    }
}

/// Proves: on the gateway a set whose `targets` has a `null` platform is set with the other
/// platforms (the release read back holds only them), and one whose `targets` is `{}` or all
/// `null` is refused `target_invalid` with nothing set.
#[tokio::test]
async fn the_gateway_drops_a_null_platform_and_refuses_targets_of_none() {
    let world = World::new("targets-gateway-null");
    let macos = targets(LEGACY)["macos-arm64"].clone();
    let body = json!({
        "service": "kbf",
        "desired": {"version": "2.0", "minUp": {"storage": 1, "frontend": 0}},
        "targets": {"linux-arm64": null, "macos-arm64": list_json(&macos)},
    })
    .to_string();
    let (status, text) = gateway(&world, SET_DESIRED, OPERATOR, &body).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let kept = world
        .coordinator
        .release_verified(LEGACY, "2.0")
        .expect("a release");
    assert_eq!(kept.targets().keys().collect::<Vec<_>>(), ["macos-arm64"]);
    assert_eq!(kept.targets()["macos-arm64"], macos);

    for none in [
        json!({}),
        json!({"linux-x86_64": null, "macos-arm64": null}),
    ] {
        let body = json!({
            "service": "kbf",
            "desired": {"version": "3.0", "minUp": {"storage": 1, "frontend": 0}},
            "targets": none,
        })
        .to_string();
        let (status, text) = gateway(&world, SET_DESIRED, OPERATOR, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert_eq!(text, refused("INVALID_ARGUMENT", "target_invalid"));
    }
    assert!(world.coordinator.release_verified(LEGACY, "3.0").is_err());
    assert_eq!(
        world
            .coordinator
            .desired
            .get(LEGACY)
            .expect("a state")
            .version
            .as_str(),
        "2.0"
    );
}

/// Proves: the adapter maps a set's refusals by token and status: targets that break a rule are
/// `INVALID_ARGUMENT` `target_invalid`, a version set as another release `FAILED_PRECONDITION`
/// `release_exists`; a set with an empty `targets` map is a set without targets (no release kept).
#[tokio::test]
async fn a_set_with_targets_is_refused_and_read_back_over_grpc() {
    let world = World::new("targets-grpc-refusals");
    let (code, message, _) =
        grpc(&world, SET_DESIRED, Some(OPERATOR), set_request("1.0", &[])).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    assert!(world.coordinator.release_verified(LEGACY, "1.0").is_err());

    let mut broken = set_request("2.0", &["macos-arm64"]);
    broken.targets.insert(
        "linux-arm64".to_owned(),
        list(vec![entry(LEGACY, "pkg", 'e', 0)]),
    );
    let (code, message, _) = grpc(&world, SET_DESIRED, Some(&ci_token()), broken).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::InvalidArgument, "target_invalid")
    );

    let (code, message, _) = grpc(
        &world,
        SET_DESIRED,
        Some(OPERATOR),
        set_request("2.0", &["macos-arm64"]),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let (code, message, _) = grpc(
        &world,
        SET_DESIRED,
        Some(&ci_token()),
        set_request("2.0", &["macos-arm64", "linux-x86_64"]),
    )
    .await;
    assert_eq!(
        (code, message.as_str()),
        (Code::FailedPrecondition, "release_exists")
    );
}

/// Proves: `set_refusal` maps the two refusals of a set's targets to their tokens.
#[test]
fn the_refusals_of_targets_map_to_their_tokens() {
    assert_eq!(
        set_refusal(SetError::TargetInvalid(
            hostkeeper_release::TargetError::NoPlatform
        )),
        proto::ReleasesRefusal::TargetInvalid
    );
    assert_eq!(
        set_refusal(SetError::ReleaseExists),
        proto::ReleasesRefusal::ReleaseExists
    );
}

/// Proves: `GetRelease` answers the targets in the order set and the index digest, to the
/// operator's token and to CI's, and refuses with the tokens `release_unknown` (`NOT_FOUND`) for a
/// version never set with targets, `unknown_service` (`NOT_FOUND`) for a service not managed, and
/// `unauthorized` to a call no token admits; an empty service is the legacy service.
#[tokio::test]
async fn get_release_answers_to_both_tokens_and_refuses_by_token() {
    let world = World::new("targets-get-release");
    let (code, message, _) = grpc(
        &world,
        SET_DESIRED,
        Some(&ci_token()),
        set_request("2.0", &["linux-x86_64", "macos-arm64"]),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let expected = set_request("2.0", &["linux-x86_64", "macos-arm64"]).targets;
    for (bearer, service) in [
        (OPERATOR.to_owned(), LEGACY),
        (ci_token(), LEGACY),
        (OPERATOR.to_owned(), ""),
    ] {
        let (code, message, body) =
            grpc(&world, GET_RELEASE, Some(&bearer), get(service, "2.0")).await;
        assert_eq!((code, message.as_str()), (Code::Ok, ""));
        let release =
            proto::GetReleaseResponse::decode(&body.expect("a body")[..]).expect("decodes");
        assert_eq!(release.targets, expected);
        assert_eq!(release.release_index_digest, None);
        let order: Vec<_> = release.targets["linux-x86_64"]
            .entries
            .iter()
            .map(|e| e.installer.as_str())
            .collect();
        assert_eq!(order, ["deb", "tarball"]);
    }
    for (service, version, token, status) in [
        (LEGACY, "9.9", "release_unknown", Code::NotFound),
        ("nobody", "2.0", "unknown_service", Code::NotFound),
    ] {
        let (code, message, _) =
            grpc(&world, GET_RELEASE, Some(OPERATOR), get(service, version)).await;
        assert_eq!((code, message.as_str()), (status, token));
    }
    let (code, message, _) = grpc(&world, GET_RELEASE, None, get(LEGACY, "2.0")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
    let (code, message, _) = grpc(&world, GET_RELEASE, Some("wrong"), get(LEGACY, "2.0")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::Unauthenticated, "unauthorized")
    );
}

/// Proves: with no legacy service an empty service in `GetRelease` is `invalid_request`, and a
/// call the router did not admit to the adapter (no caller learned) is `unauthorized`.
#[tokio::test]
async fn get_release_without_a_service_or_a_caller_is_refused() {
    let rig = Rig::new("targets-get-release-no-legacy");
    let started = Rig::start(&rig.dir, &rig.clock).with_legacy_service(None);
    let world = World::over(rig, started);
    let (code, message, _) = grpc(&world, GET_RELEASE, Some(OPERATOR), get("", "2.0")).await;
    assert_eq!(
        (code, message.as_str()),
        (Code::InvalidArgument, "invalid_request")
    );

    let service = ReleasesService::new(world.coordinator.clone());
    let unadmitted = service
        .get_release(GrpcRequest::new(get(LEGACY, "2.0")))
        .await
        .expect_err("no caller");
    assert_eq!(
        (unadmitted.code(), unadmitted.message()),
        (Code::Unauthenticated, "unauthorized")
    );
    let mut as_host = GrpcRequest::new(get(LEGACY, "2.0"));
    as_host
        .extensions_mut()
        .insert(Admitted::Peer(Some(std::sync::Arc::from(vec![1u8]))));
    let refused = service.get_release(as_host).await.expect_err("no operator");
    assert_eq!(refused.message(), "unauthorized");
}

/// Proves: the frozen `POST /v1/desired` answers `409 release_exists` for a set that names a
/// package digest of a version that has a release, and a plain set of that version is accepted.
#[tokio::test]
async fn the_frozen_route_answers_409_for_a_digest_of_a_version_with_a_release() {
    let world = World::new("targets-frozen-409");
    let (code, message, _) = grpc(
        &world,
        SET_DESIRED,
        Some(OPERATOR),
        set_request("2.0", &["macos-arm64"]),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let named = json!({
        "version": "2.0", "min_up": {"storage": 0, "frontend": 0}, "release_digest": hex('1'),
    })
    .to_string();
    let (status, answer) = post(
        &world.coordinator,
        crate::http::DESIRED_PATH,
        &ci_token(),
        &named,
    )
    .await;
    assert_eq!(
        (status, answer),
        (StatusCode::CONFLICT, json!({"error": "release_exists"}))
    );
    let plain = json!({"version": "2.0", "min_up": {"storage": 0, "frontend": 0}}).to_string();
    let (status, _) = post(
        &world.coordinator,
        crate::http::DESIRED_PATH,
        &ci_token(),
        &plain,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(world.coordinator.release_verified(LEGACY, "2.0").is_ok());
}
