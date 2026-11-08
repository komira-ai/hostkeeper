//! What the tests of `SetDesired` with targets share: a release that passes every rule, one
//! entry per installer, and the two routes of the operator API: a gRPC call through the
//! listeners' router, read from its trailers, and the JSON gateway.

use std::collections::BTreeMap;

use bytes::Bytes;
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Hook, InstallerBounds, Location, Manifest, StepBounds, Target, TargetList,
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Version};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};

use super::rig::{PATIENCE, Rig};

/// The path of `Releases.SetDesired`.
pub const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
/// The path of `Releases.GetRelease`.
pub const GET_RELEASE: &str = "/hostkeeper.operator.v1.Releases/GetRelease";

/// A digest of 64 lower-case hex characters, all `c`.
pub fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

/// A location in the store `github`.
pub fn location(locator: &str) -> Location {
    Location {
        store: "github".to_owned(),
        locator: locator.to_owned(),
    }
}

/// A manifest that passes every rule a manifest keeps, for the service `service` on Linux.
pub fn manifest(service: &str, installer: &str) -> Manifest {
    if installer == "hostkeeper-archive" {
        return Manifest {
            min_hostkeeper: "0.4.0".to_owned(),
            ..Manifest::default()
        };
    }
    Manifest {
        package: (installer == "deb" || installer == "rpm").then(|| service.to_owned()),
        hook: Some(Hook {
            command: vec![format!("bin/{service}"), "update-hook".to_owned()],
            timeout_secs: Some(StepBounds {
                prepare: 0,
                cutover: 10,
                verify: 20,
                rollback: 30,
                cleanup: 40,
                uninstall: None,
            }),
            verify_deadline_secs: 50,
            ..Hook::default()
        }),
        run: vec![format!("{service}.service")],
        min_hostkeeper: "0.4.0".to_owned(),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 120,
            remove_secs: 300,
        }),
        ..Manifest::default()
    }
}

/// An entry of `installer` for `service` whose artifact's digest is all `c` and whose size is
/// `bytes`.
pub fn entry(service: &str, installer: &str, c: char, bytes: u64) -> Target {
    Target {
        installer: installer.to_owned(),
        locations: vec![location(&format!(
            "example-org/{service}/releases/download/v2.0/{service}-{installer}"
        ))],
        digest: digest(c),
        bytes,
        attestation: Some(Blob {
            digest: digest('e'),
            bytes: 7311,
            locations: vec![location(&format!(
                "example-org/{service}/releases/download/v2.0/{service}.sigstore.json"
            ))],
        }),
        manifest: Some(manifest(service, installer)),
    }
}

/// The list of `entries`.
pub fn list(entries: Vec<Target>) -> TargetList {
    TargetList { entries }
}

/// Targets of `service`: on `linux-x86_64` a `deb`, an `rpm` and a `tarball` entry, in that
/// order; on `macos-arm64` a `tarball`; `linux-arm64` left out.
pub fn targets(service: &str) -> BTreeMap<String, TargetList> {
    BTreeMap::from([
        (
            "linux-x86_64".to_owned(),
            list(vec![
                entry(service, "deb", 'a', 48_812_032),
                entry(service, "rpm", 'b', 48_812_100),
                entry(service, "tarball", 'c', 40_000_000),
            ]),
        ),
        (
            "macos-arm64".to_owned(),
            list(vec![entry(service, "tarball", 'd', 39_000_000)]),
        ),
    ])
}

/// A set of `service` at `version` with `targets` and nothing else of a release.
pub fn set(
    service: &str,
    version: &str,
    targets: BTreeMap<String, TargetList>,
) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(proto::DesiredState {
            version: version.to_owned(),
            ..proto::DesiredState::default()
        }),
        targets,
        ..proto::SetDesiredRequest::default()
    }
}

/// What a gRPC call answered: its status code (0 for `OK`), its message, and the response
/// message's bytes when it was answered.
#[derive(Debug, PartialEq, Eq)]
pub struct Answered {
    pub code: i32,
    pub message: String,
    pub body: Option<Vec<u8>>,
}

impl Answered {
    /// The response, decoded; panics when the call was refused.
    pub fn decoded<M: Message + Default>(&self) -> M {
        let body = self.body.as_ref().unwrap_or_else(|| panic!("{self:?}"));
        M::decode(body.as_slice()).expect("the response decodes")
    }

    /// The refusal: the status code and the token.
    pub fn refusal(&self) -> (i32, &str) {
        (self.code, self.message.as_str())
    }
}

/// `bytes` as one gRPC message.
pub fn framed(bytes: &[u8]) -> Bytes {
    let mut out = vec![0u8];
    out.extend(u32::try_from(bytes.len()).expect("fits").to_be_bytes());
    out.extend(bytes);
    Bytes::from(out)
}

/// Calls `path` over gRPC, through the listeners' router, with `bearer`, sending `bytes` as the
/// request message.
pub async fn grpc_bytes(rig: &Rig, path: &str, bearer: Option<&str>, bytes: &[u8]) -> Answered {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder.body(Full::new(framed(bytes))).expect("a request");
    let response = route(&rig.coordinator, Surface::Control, &None, request, PATIENCE).await;
    let collected = response.into_body().collect().await.expect("a body");
    let trailers = collected.trailers().cloned().unwrap_or_default();
    let data = collected.to_bytes();
    let code = trailers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let message = trailers
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = (code == 0 && data.len() >= 5).then(|| data[5..].to_vec());
    Answered {
        code,
        message,
        body,
    }
}

/// Calls `path` over gRPC with `message`.
pub async fn grpc<M: Message>(
    rig: &Rig,
    path: &str,
    bearer: Option<&str>,
    message: &M,
) -> Answered {
    grpc_bytes(rig, path, bearer, &message.encode_to_vec()).await
}

/// `message`, the message `name` names (`hostkeeper.operator.v1.SetDesiredRequest`), as the
/// proto3 canonical JSON the gateway reads.
pub fn json_of<M: Message>(message: &M, name: &str) -> serde_json::Value {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the pool");
    let descriptor = pool.get_message_by_name(name).expect("the message");
    let dynamic =
        DynamicMessage::decode(descriptor, message.encode_to_vec().as_slice()).expect("decoded");
    serde_json::to_value(&dynamic).expect("JSON")
}

/// `request` as the JSON body of a gateway `SetDesired`.
pub fn set_json(request: &proto::SetDesiredRequest) -> serde_json::Value {
    json_of(request, "hostkeeper.operator.v1.SetDesiredRequest")
}

/// Posts `body` to the gateway path `path` with `bearer`: the HTTP status and the JSON answer.
pub async fn gateway(
    rig: &Rig,
    path: &str,
    bearer: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value) {
    let (code, text) = rig.control("POST", path, bearer, body).await;
    let answer = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (code, answer)
}
