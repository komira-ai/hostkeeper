//! Every call the web app makes, as `ui/calls.json` lists it, sent to the real coordinator: the
//! one `hostkeeper_coordinator::config::build` makes from `serve`'s flags, as the binary makes
//! it, called through `hostkeeper_coordinator::grpc::route`, the entry point of its listeners.
//! The page tests run the pages against a fake gateway; this is the check that the coordinator
//! serves each of those calls, and serves it to the credential the app sends: an operator's
//! token from the tokens file, as `Authorization: Bearer <token>`.
//!
//! Each call goes twice, each time to a coordinator of its own (so no call sees what another
//! changed): as the app sends it, a `POST` of the method's path with the request as JSON, and as
//! a gRPC call of the same path with the same request as protobuf. A call fails when it is
//! answered `UNIMPLEMENTED` (no handler), `PERMISSION_DENIED` or `UNAUTHENTICATED` (the
//! credential is refused), `UNKNOWN`, `INTERNAL`, `UNAVAILABLE` or `DATA_LOSS` (a fault, not an
//! answer), or when the gateway cannot read the request this file sends for it
//! (`invalid_request`: then the test proves nothing about the method). Any other answer, `OK`
//! or a refusal about the data (`NOT_FOUND` for a host no heartbeat named, say), passes: the
//! method is served and the credential is allowed.
//!
//! [`REQUESTS`] holds one request for each method, in the shape the page that calls it sends.
//! A method of `calls.json` with no request there fails, and so does a request for a method
//! `calls.json` no longer lists, so a call the app adds is covered when it is added.
//!
//! Nothing is fetched and no socket is opened: the router is called in process, the TLS files
//! are read and never served, and the bearer that is not an operator's has no `.` in it, so it
//! is refused as no JWT before any issuer key is looked for.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use bytes::Bytes;
use clap::Parser as _;
use hostkeeper_coordinator::config::{Cli, Command, Parts, build};
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use hostkeeper_ui_tests::{UiFile, listed_calls};
use http_body_util::{BodyExt as _, Either, Full};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, TE};
use hyper::{Method, Request, Uri, Version};
use prost::Message as _;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, MethodDescriptor};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};
use tonic::{Code, Status};

/// The operator token the app sends. The coordinator holds only its SHA-256, in the tokens file.
const OPERATOR_TOKEN: &str = "web-app-operator-token-of-the-test";
/// The name the tokens file gives [`OPERATOR_TOKEN`].
const TOKEN_NAME: &str = "web-app";
/// A bearer the coordinator does not hold. It has no `.`, so it is no JWT either.
const STRANGER_TOKEN: &str = "a-token-no-file-names";

/// How long the router waits for a request's body; every body here is whole when it is sent.
const PATIENCE: Duration = Duration::from_secs(30);

/// The reason the gateway gives a body that is not the method's request as JSON.
const INVALID_REQUEST: &str = "invalid_request";

/// The reason a bearer is given for a method only a signed-in person's session may call.
const SESSION_REQUIRED: &str = "session_required";

/// The methods of `ui/calls.json` that are session-only (`grpc::SESSION_ONLY`): the writes of the
/// MDM setup. The app calls them with the person's session, which this file has none of, so a
/// bearer is answered `PERMISSION_DENIED` `session_required` and that answer is the proof that the
/// coordinator serves the method and keeps it from a token. Any other method answered so fails.
const SESSION_ONLY_CALLS: &[&str] = &[
    "hostkeeper.operator.v1.Mdm/CreatePushRequest",
    "hostkeeper.operator.v1.Mdm/GetAbmPublicKey",
    "hostkeeper.operator.v1.Mdm/PutAbmToken",
    "hostkeeper.operator.v1.Mdm/PutName",
    "hostkeeper.operator.v1.Mdm/PutPushCertificate",
    "hostkeeper.operator.v1.Mdm/PutPushReply",
    "hostkeeper.operator.v1.Mdm/Verify",
    "hostkeeper.operator.v1.Mdm/PutSecret",
];

/// One request for each method of `ui/calls.json`, as proto3 JSON, in the shape the page that
/// calls the method sends it (`ui/pages/*.js`), with values the coordinator takes: a service, a
/// role, a host id, a reason. A host or request id no heartbeat or
/// enrollment named is answered `NOT_FOUND` or the like, which passes.
const REQUESTS: &[(&str, &str)] = &[
    ("hostkeeper.operator.v1.Alerts/List", "{}"),
    (
        "hostkeeper.operator.v1.ApiTokens/Create",
        r#"{"name": "build-script"}"#,
    ),
    ("hostkeeper.operator.v1.ApiTokens/List", "{}"),
    (
        "hostkeeper.operator.v1.ApiTokens/Revoke",
        r#"{"name": "build-script"}"#,
    ),
    (
        "hostkeeper.operator.v1.Audit/List",
        r#"{"sinceSeq": "0", "limit": 100, "service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/AddRecord",
        r#"{"hostId": "storage-1", "role": "storage", "dataMacs": ["02:00:00:00:00:01"],
            "bmcMac": "", "serial": "", "notes": "racked", "approveForSecs": "7200"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/Approve",
        r#"{"requestId": "1111111111111111111111111111111111111111111111111111111111111111",
            "keySha256": "2222222222222222222222222222222222222222222222222222222222222222",
            "hostId": "storage-1", "role": "storage", "services": ["kbf"], "replace": false,
            "reason": "racked and checked"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/List",
        r#"{"limit": 100, "pageToken": ""}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/ListRecords",
        r#"{"limit": 100, "pageToken": ""}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/OpenRecord",
        r#"{"hostId": "storage-1", "approveForSecs": "3600", "reason": "rebuilt"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/Reject",
        r#"{"requestId": "1111111111111111111111111111111111111111111111111111111111111111",
            "reason": "not one of ours"}"#,
    ),
    (
        "hostkeeper.operator.v1.Enrollment/RemoveRecord",
        r#"{"hostId": "storage-1", "reason": "decommissioned"}"#,
    ),
    (
        "hostkeeper.operator.v1.HostGroups/Get",
        r#"{"hostGroup": "storage", "limit": 50}"#,
    ),
    ("hostkeeper.operator.v1.HostGroups/List", "{}"),
    (
        "hostkeeper.operator.v1.HostGroups/Move",
        r#"{"nodeId": "storage-1", "hostGroup": "storage", "reason": "rebuilt"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Abort",
        r#"{"nodeId": "storage-1", "service": "kbf", "reason": "stuck"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Exclude",
        r#"{"service": "kbf", "nodeId": "storage-1", "reason": "disk swap", "abandon": false}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Get",
        r#"{"nodeId": "storage-1"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/Include",
        r#"{"service": "kbf", "nodeId": "storage-1"}"#,
    ),
    ("hostkeeper.operator.v1.Hosts/List", r#"{}"#),
    (
        "hostkeeper.operator.v1.Hosts/Retire",
        r#"{"nodeId": "storage-1", "reason": "board failed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Hosts/SetLabels",
        r#"{"nodeId": "storage-1", "labels": {"rack": "r1"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Mdm/CreatePushRequest",
        r#"{"email": "ops@example.org", "submit": false}"#,
    ),
    ("hostkeeper.operator.v1.Mdm/ListDevices", "{}"),
    ("hostkeeper.operator.v1.Mdm/GetAbmPublicKey", "{}"),
    ("hostkeeper.operator.v1.Mdm/ListSettings", "{}"),
    (
        "hostkeeper.operator.v1.Mdm/PutAbmToken",
        r#"{"tokenJson": "e30="}"#,
    ),
    (
        "hostkeeper.operator.v1.Mdm/PutName",
        r#"{"name": "mdm.example.com"}"#,
    ),
    (
        "hostkeeper.operator.v1.Mdm/PutPushCertificate",
        r#"{"certificatePem": "MDBmZg=="}"#,
    ),
    (
        "hostkeeper.operator.v1.Mdm/PutPushReply",
        r#"{"reply": "MDBmZg=="}"#,
    ),
    (
        "hostkeeper.operator.v1.Mdm/PutSecret",
        r#"{"kind": "abm_key", "material": "MDBmZg=="}"#,
    ),
    ("hostkeeper.operator.v1.Mdm/Status", "{}"),
    (
        "hostkeeper.operator.v1.Mdm/Verify",
        r#"{"enrollmentId": "mac-1"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/ClearHalt",
        r#"{"service": "kbf", "reason": "fixed"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Converge",
        r#"{"service": "kbf"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Force",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "hotfix", "includeHeld": false,
            "fleet": {"confirmService": "kbf"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/GetRelease",
        r#"{"service": "kbf", "version": "1.2.3"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/Halt",
        r#"{"service": "kbf", "reason": "bad canary"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/MarkBad",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "crashes"}"#,
    ),
    (
        "hostkeeper.operator.v1.Releases/SetDesired",
        r#"{"service": "kbf", "desired": {"version": "1.2.3"}}"#,
    ),
    ("hostkeeper.operator.v1.Releases/Status", r#"{}"#),
    (
        "hostkeeper.operator.v1.Releases/UnmarkBad",
        r#"{"service": "kbf", "version": "1.2.3", "reason": "was the disk"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Advance",
        r#"{"service": "kbf", "reason": "canary looks fine"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Pause",
        r#"{"service": "kbf", "reason": "freeze for audit"}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Resume",
        r#"{"service": "kbf", "reason": "audit done"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Assign",
        r#"{"service": "tierstore", "role": "storage"}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Hold",
        r#"{"scope": {"host": "storage-1"}, "service": "kbf", "reason": "disk swap"}"#,
    ),
    ("hostkeeper.operator.v1.Services/List", r#"{}"#),
    (
        "hostkeeper.operator.v1.Services/SetMinVersion",
        r#"{"service": "kbf", "minVersion": {"role": "storage", "version": "1.0.0"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Unassign",
        r#"{"service": "tierstore", "role": "storage", "uninstall": false}"#,
    ),
    (
        "hostkeeper.operator.v1.Services/Unhold",
        r#"{"scope": {"host": "storage-1"}, "service": "kbf", "reason": "swapped"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Create",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Delete",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Disable",
        r#"{"name": "alice"}"#,
    ),
    (
        "hostkeeper.operator.v1.Users/Enable",
        r#"{"name": "alice"}"#,
    ),
    ("hostkeeper.operator.v1.Users/List", "{}"),
    (
        "hostkeeper.operator.v1.Users/ResetPassword",
        r#"{"name": "alice"}"#,
    ),
];

/// The calls sent, with the operator's token over the gateway, before the request of a method
/// that needs them: what an operator has done before the page offers the action. A halt stands at
/// a service's desired version, so a service with none has nothing to halt (`invalid_request`);
/// and a plan has a version only once one is set, so an Advance meets a plan that does not wait
/// (`not_waiting`) rather than a service with no desired state (`no_desired`).
const BEFORE: &[(&str, &str, &str)] = &[
    (
        "hostkeeper.operator.v1.Releases/Halt",
        "hostkeeper.operator.v1.Releases/SetDesired",
        r#"{"service": "kbf", "desired": {"version": "1.2.3"}}"#,
    ),
    (
        "hostkeeper.operator.v1.Rollouts/Advance",
        "hostkeeper.operator.v1.Releases/SetDesired",
        r#"{"service": "kbf", "desired": {"version": "1.2.3"}}"#,
    ),
];

/// The group a coordinator is given before a method is sent to it: the coordinator manages no
/// service of its own, so the two services the requests name are packages of this group.
const GROUP: (&str, &str) = (
    "hostkeeper.operator.v1.HostGroups/Put",
    r#"{"hostGroup": {"name": "storage", "packages": ["hostkeeper", "kbf", "tierstore"]},
        "reason": "the packages the requests name"}"#,
);

/// Every method `ui/calls.json` lists.
fn listed() -> Vec<String> {
    let file = UiFile::named("calls.json").expect("ui/calls.json is a file of the app");
    listed_calls(file.text)
        .expect("ui/calls.json parses")
        .into_iter()
        .map(|call| call.method)
        .collect()
}

/// The methods [`REQUESTS`] has a request for.
fn built() -> Vec<String> {
    REQUESTS
        .iter()
        .map(|(method, _)| (*method).to_owned())
        .collect()
}

/// What keeps `requests` from covering `listed` exactly: each listed method with no request,
/// and each request for a method not listed, as a line naming the method.
fn uncovered(listed: &[String], requests: &[String]) -> Vec<String> {
    let listed: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
    let requests: BTreeSet<&str> = requests.iter().map(String::as_str).collect();
    let missing = listed
        .difference(&requests)
        .map(|method| format!("{method}: in ui/calls.json, with no request in REQUESTS"));
    let extra = requests
        .difference(&listed)
        .map(|method| format!("{method}: a request in REQUESTS, not in ui/calls.json"));
    missing.chain(extra).collect()
}

/// The request [`REQUESTS`] holds for `method`.
fn request_for(method: &str) -> Option<&'static str> {
    REQUESTS
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, json)| *json)
}

/// The descriptors of every message and service.
static POOL: LazyLock<DescriptorPool> = LazyLock::new(|| {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the descriptors decode")
});

/// The method `method` (`<package>.<Service>/<Method>`) names.
fn method_of(method: &str) -> MethodDescriptor {
    let (service, name) = method.split_once('/').expect("a method has a /");
    POOL.get_service_by_name(service)
        .and_then(|service| service.methods().find(|m| m.name() == name))
        .unwrap_or_else(|| panic!("{method} is no method of the descriptors"))
}

/// The SHA-256 of `text` as lowercase hex, as the tokens file holds a token.
fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A self-signed certificate and its key, as PEM, and the certificate as DER.
struct Issued {
    cert_pem: String,
    key_pem: String,
    der: Vec<u8>,
}

/// A self-signed certificate for `localhost`; a CA's when `ca`.
fn issue(ca: bool) -> Issued {
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
    if ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    }
    let key = KeyPair::generate().expect("a key");
    let cert = params.self_signed(&key).expect("a certificate");
    Issued {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        der: cert.der().to_vec(),
    }
}

/// A coordinator built from `serve`'s flags over files in a directory of its own, and the
/// directory, kept for as long as the coordinator is.
struct Served {
    parts: Parts,
    _dir: Scratch,
}

impl Served {
    /// What `hostkeeper-coordinator serve` builds from flags naming files written in a new
    /// directory: the operator tokens file with [`OPERATOR_TOKEN`] named [`TOKEN_NAME`], and the coordinator as the hosts' CA (`--enrollment-dir`), so
    /// the enrollment calls are served as a deployed coordinator serves them.
    fn start(name: &str) -> Self {
        let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("a scratch directory");
        let put = |file: &str, text: &str, mode: u32| -> PathBuf {
            let path = dir.path().join(file);
            std::fs::write(&path, text).expect("written");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
            path
        };
        let server = issue(false);
        let clients = issue(true);
        let tokens = format!(
            "[[token]]\nname = \"{TOKEN_NAME}\"\nsha256 = \"{}\"\n",
            sha256_hex(OPERATOR_TOKEN)
        );
        let path = |p: PathBuf| p.display().to_string();
        let pairs = [
            ("--heartbeat-listen", "192.0.2.1:1".to_owned()),
            ("--private-listen", "192.0.2.1:2".to_owned()),
            ("--tls-cert", path(put("tls.pem", &server.cert_pem, 0o644))),
            ("--tls-key", path(put("tls.key", &server.key_pem, 0o600))),
            ("--client-ca", path(put("ca.pem", &clients.cert_pem, 0o644))),
            ("--state-file", path(dir.path().join("desired.json"))),
            (
                "--oidc-config",
                path(put("oidc.toml", &config_text(STATIC_ISSUER), 0o644)),
            ),
            ("--tokens-file", path(put("tokens.toml", &tokens, 0o600))),
            ("--legacy-service", "kbf".to_owned()),
            ("--enrollment-dir", path(dir.path().join("enrollment"))),
            ("--ca-key-file", path(dir.path().join("ca.key"))),
            ("--server-cert-file", path(dir.path().join("server.pem"))),
            ("--server-name", "coordinator.example".to_owned()),
        ];
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        for (flag, value) in pairs {
            args.push(flag.into());
            args.push(value.into());
        }
        args.push("--ca-init".into());
        let Command::Serve(flags) = Cli::try_parse_from(args).expect("the flags parse").command
        else {
            panic!("the verb is serve");
        };
        // The roots the issuer's keys would be fetched trusting: a certificate of the test, so
        // nothing reads the host's store. No key is fetched here.
        let roots = Roots::Given(vec![CertificateDer::from(clients.der)]);
        let parts = build(&flags, roots).expect("the coordinator starts");
        Self { parts, _dir: dir }
    }
}

/// How a call travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    /// As the web app sends it: `POST /<method>` with the request as JSON.
    Gateway,
    /// A gRPC call of the same path over HTTP/2, with the request as protobuf.
    Grpc,
}

impl Transport {
    /// Every transport, in the order the calls are sent.
    const ALL: [Self; 2] = [Self::Gateway, Self::Grpc];

    fn name(self) -> &'static str {
        match self {
            Self::Gateway => "the gateway",
            Self::Grpc => "gRPC",
        }
    }
}

/// How the coordinator answered one call: the gRPC code, and the reason (the message of the
/// status, or the gateway's `reason`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answer {
    code: Code,
    reason: String,
}

/// The gRPC code a gateway refusal names by its `code`.
fn code_named(name: &str) -> Code {
    const CODES: [(&str, Code); 17] = [
        ("OK", Code::Ok),
        ("CANCELLED", Code::Cancelled),
        ("UNKNOWN", Code::Unknown),
        ("INVALID_ARGUMENT", Code::InvalidArgument),
        ("DEADLINE_EXCEEDED", Code::DeadlineExceeded),
        ("NOT_FOUND", Code::NotFound),
        ("ALREADY_EXISTS", Code::AlreadyExists),
        ("PERMISSION_DENIED", Code::PermissionDenied),
        ("RESOURCE_EXHAUSTED", Code::ResourceExhausted),
        ("FAILED_PRECONDITION", Code::FailedPrecondition),
        ("ABORTED", Code::Aborted),
        ("OUT_OF_RANGE", Code::OutOfRange),
        ("UNIMPLEMENTED", Code::Unimplemented),
        ("INTERNAL", Code::Internal),
        ("UNAVAILABLE", Code::Unavailable),
        ("DATA_LOSS", Code::DataLoss),
        ("UNAUTHENTICATED", Code::Unauthenticated),
    ];
    CODES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, code)| *code)
        .unwrap_or_else(|| panic!("{name} is no gRPC code name"))
}

/// The answer of `served` to `method` sent over `transport` with the JSON request `json` and
/// `bearer` as the token: over gRPC, `json` is sent as the method's request message.
async fn send(
    served: &Served,
    transport: Transport,
    method: &str,
    json: &str,
    bearer: &str,
) -> Answer {
    let body = match transport {
        Transport::Gateway => Bytes::from(json.to_owned()),
        Transport::Grpc => {
            let mut reader = serde_json::Deserializer::from_str(json);
            let message = DynamicMessage::deserialize_with_options(
                method_of(method).input(),
                &mut reader,
                &DeserializeOptions::new(),
            )
            .unwrap_or_else(|e| panic!("the request for {method} is not its message: {e}"));
            framed(&message.encode_to_vec())
        }
    };
    send_body(served, transport, method, body, bearer).await
}

/// The answer of `served` to `method` sent over `transport` with `body` as the body (JSON for
/// the gateway, a framed message for gRPC) and `bearer` as the token.
async fn send_body(
    served: &Served,
    transport: Transport,
    method: &str,
    body: Bytes,
    bearer: &str,
) -> Answer {
    let path = format!("/{method}");
    let mut request = Request::new(Full::new(body));
    match transport {
        Transport::Gateway => {
            request
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        Transport::Grpc => {
            *request.version_mut() = Version::HTTP_2;
            let headers = request.headers_mut();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
            headers.insert(TE, HeaderValue::from_static("trailers"));
        }
    }
    *request.method_mut() = Method::POST;
    *request.uri_mut() = path.parse::<Uri>().expect("a path");
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}")).expect("a header value"),
    );

    let coordinator = Arc::clone(&served.parts.coordinator);
    let response = route(&coordinator, Surface::Control, &None, request, PATIENCE).await;
    let (head, body) = response.into_parts();
    match body {
        Either::Left(body) => {
            let bytes = body.collect().await.expect("a body").to_bytes();
            if head.status.as_u16() == 200 {
                return Answer {
                    code: Code::Ok,
                    reason: String::new(),
                };
            }
            let refusal: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "{method} over {}: the {} answer is no refusal ({e}): {}",
                    transport.name(),
                    head.status,
                    String::from_utf8_lossy(&bytes)
                )
            });
            Answer {
                code: code_named(refusal["code"].as_str().unwrap_or_default()),
                reason: refusal["reason"].as_str().unwrap_or_default().to_owned(),
            }
        }
        Either::Right(body) => {
            let collected = body.collect().await.expect("a body");
            let status = collected
                .trailers()
                .and_then(Status::from_header_map)
                .or_else(|| Status::from_header_map(&head.headers))
                .unwrap_or_else(|| panic!("{method} over gRPC: an answer with no status"));
            Answer {
                code: status.code(),
                reason: status.message().to_owned(),
            }
        }
    }
}

/// `message` as one uncompressed gRPC message: a zero flag, its length as four big-endian
/// bytes, and the message.
fn framed(message: &[u8]) -> Bytes {
    let length = u32::try_from(message.len()).expect("a short message");
    let mut bytes = vec![0];
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(message);
    Bytes::from(bytes)
}

/// Why `answer` fails the call of `method` over `transport`, as a line naming both, or `None`
/// when the method is served and the credential allowed.
fn failure(method: &str, transport: Transport, answer: &Answer) -> Option<String> {
    let why = match answer.code {
        Code::PermissionDenied
            if answer.reason == SESSION_REQUIRED && SESSION_ONLY_CALLS.contains(&method) =>
        {
            return None;
        }
        Code::Unimplemented => "UNIMPLEMENTED: the coordinator has no handler for it",
        Code::PermissionDenied => "PERMISSION_DENIED: the app's credential is forbidden it",
        Code::Unauthenticated => "UNAUTHENTICATED: the app's credential is refused",
        Code::InvalidArgument if answer.reason == INVALID_REQUEST => {
            "invalid_request: the request REQUESTS holds for it is not its message"
        }
        Code::Unknown | Code::Internal | Code::Unavailable | Code::DataLoss => {
            "a fault of the coordinator, not an answer about the data"
        }
        _ => return None,
    };
    Some(format!(
        "{method} over {}: {why} (reason {:?})",
        transport.name(),
        answer.reason
    ))
}

/// Sends every method of `methods` over every transport with `bearer`, each call to a new
/// coordinator, and answers the failures, one line each.
async fn failures_of(methods: &[String], bearer: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for (n, method) in methods.iter().enumerate() {
        let Some(json) = request_for(method) else {
            failures.push(format!("{method}: no request in REQUESTS"));
            continue;
        };
        for transport in Transport::ALL {
            let served = Served::start(&format!("real-router-{n}-{transport:?}"));
            let put = send(
                &served,
                Transport::Gateway,
                GROUP.0,
                GROUP.1,
                OPERATOR_TOKEN,
            )
            .await;
            assert_eq!(put.code, Code::Ok, "{put:?}");
            for (_, before, request) in BEFORE.iter().filter(|(m, _, _)| m == method) {
                let answer =
                    send(&served, Transport::Gateway, before, request, OPERATOR_TOKEN).await;
                assert_eq!(
                    answer.code,
                    Code::Ok,
                    "{before}, before {method}: {answer:?}"
                );
            }
            let answer = send(&served, transport, method, json, bearer).await;
            failures.extend(failure(method, transport, &answer));
        }
    }
    failures
}

/// Catches: a call of the web app that the coordinator does not serve (`UNIMPLEMENTED`), or
/// refuses to the app's credential (`PERMISSION_DENIED`, `UNAUTHENTICATED`), over the gateway
/// the app calls or over gRPC, and a request in `REQUESTS` the gateway cannot read. Every method
/// of `ui/calls.json` is served to an operator's token from the tokens file, none exempted.
#[tokio::test]
async fn every_call_of_the_app_is_served_to_the_operators_token() {
    let methods = listed();
    assert!(!methods.is_empty(), "ui/calls.json lists no call");
    let failures = failures_of(&methods, OPERATOR_TOKEN).await;
    assert!(
        failures.is_empty(),
        "{} of the {} calls of ui/calls.json fail against the real coordinator:\n{}",
        failures.len(),
        methods.len() * Transport::ALL.len(),
        failures.join("\n")
    );
}

/// Catches: the session-only exemption passing more than it should. A bearer refused
/// `session_required` is a pass for a method of `SESSION_ONLY_CALLS` and a failure, naming the
/// refusal, for any other method; the same refusal of another reason, or another code, fails
/// the session-only method too.
#[test]
fn a_session_required_refusal_passes_only_for_a_session_only_method() {
    let refused = |code, reason: &str| Answer {
        code,
        reason: reason.to_owned(),
    };
    let session_only = "hostkeeper.operator.v1.Mdm/PutPushReply";
    let other = "hostkeeper.operator.v1.Mdm/Status";
    let transport = Transport::Gateway;
    assert_eq!(
        failure(
            session_only,
            transport,
            &refused(Code::PermissionDenied, "session_required")
        ),
        None
    );
    assert_eq!(
        failure(
            other,
            transport,
            &refused(Code::PermissionDenied, "session_required")
        ),
        Some(format!(
            "{other} over the gateway: PERMISSION_DENIED: the app's credential is forbidden it \
             (reason \"session_required\")"
        ))
    );
    for answer in [
        refused(Code::PermissionDenied, "forbidden"),
        refused(Code::Unauthenticated, "unauthorized"),
        refused(Code::Unimplemented, ""),
    ] {
        assert!(
            failure(session_only, transport, &answer).is_some(),
            "{answer:?}"
        );
    }
    for method in SESSION_ONLY_CALLS {
        assert!(
            listed().iter().any(|m| m == method),
            "{method} is not a call of the app"
        );
    }
}

/// Catches: a call added to `ui/calls.json` with no request here (it would go unchecked), a
/// request kept here for a call the app no longer makes, and a call of `BEFORE` kept for a
/// method the app no longer calls or sent ahead of a method with no request.
#[test]
fn every_call_of_the_app_has_exactly_one_request() {
    let methods = built();
    let distinct: BTreeSet<&String> = methods.iter().collect();
    assert_eq!(distinct.len(), methods.len(), "a method has two requests");
    let listed = listed();
    assert_eq!(uncovered(&listed, &methods), Vec::<String>::new());
    for (method, before, _) in BEFORE {
        assert!(
            listed.iter().any(|m| m == method),
            "{method} is in BEFORE, not in ui/calls.json"
        );
        assert!(
            request_for(before).is_some(),
            "{before}, sent before {method}, has no request"
        );
    }
}

/// Catches: the cover check passing a method of `calls.json` with no request, or a request for
/// a method `calls.json` does not list; each is named.
#[test]
fn a_call_with_no_request_and_a_request_with_no_call_are_both_named() {
    let listed = vec![
        "hostkeeper.operator.v1.Hosts/List".to_owned(),
        "hostkeeper.operator.v1.Hosts/Rename".to_owned(),
    ];
    let requests = vec![
        "hostkeeper.operator.v1.Hosts/List".to_owned(),
        "hostkeeper.operator.v1.Hosts/Forget".to_owned(),
    ];
    assert_eq!(
        uncovered(&listed, &requests),
        vec![
            "hostkeeper.operator.v1.Hosts/Rename: in ui/calls.json, with no request in REQUESTS"
                .to_owned(),
            "hostkeeper.operator.v1.Hosts/Forget: a request in REQUESTS, not in ui/calls.json"
                .to_owned(),
        ]
    );
    assert_eq!(uncovered(&listed, &listed), Vec::<String>::new());
}

/// Catches: the check not seeing a refused credential. A token the tokens file does not hold is
/// refused `UNAUTHENTICATED` for every call of the app, over both transports, and each refusal
/// is a failure that names its method and transport.
#[tokio::test]
async fn every_call_with_a_token_the_coordinator_does_not_hold_fails_unauthenticated() {
    let methods = listed();
    let failures = failures_of(&methods, STRANGER_TOKEN).await;
    let expected: Vec<String> = methods
        .iter()
        .flat_map(|method| {
            Transport::ALL.into_iter().map(move |transport| {
                format!(
                    "{method} over {}: UNAUTHENTICATED: the app's credential is refused \
                     (reason \"unauthorized\")",
                    transport.name()
                )
            })
        })
        .collect();
    assert_eq!(failures, expected);
}

/// Catches: the check not seeing a method the coordinator has no handler for. A method of the
/// operator API's package that no service has is answered `UNIMPLEMENTED` over gRPC, and that
/// fails the call, naming it.
#[tokio::test]
async fn a_method_the_coordinator_does_not_serve_fails_unimplemented() {
    let served = Served::start("real-router-unserved");
    let method = "hostkeeper.operator.v1.Hosts/NoSuchMethod";
    let answer = send_body(
        &served,
        Transport::Grpc,
        method,
        framed(&[]),
        OPERATOR_TOKEN,
    )
    .await;
    assert_eq!(answer.code, Code::Unimplemented);
    assert_eq!(
        failure(method, Transport::Grpc, &answer),
        Some(format!(
            "{method} over gRPC: UNIMPLEMENTED: the coordinator has no handler for it (reason {:?})",
            answer.reason
        ))
    );
}

/// Catches: a request the gateway cannot read passing as served. A field the message does not
/// have is refused `invalid_request`, and that fails the call, naming it.
#[tokio::test]
async fn a_request_the_gateway_cannot_read_fails() {
    let served = Served::start("real-router-unreadable");
    let method = "hostkeeper.operator.v1.Hosts/Get";
    let answer = send(
        &served,
        Transport::Gateway,
        method,
        r#"{"hostname": "storage-1"}"#,
        OPERATOR_TOKEN,
    )
    .await;
    assert_eq!(
        answer,
        Answer {
            code: Code::InvalidArgument,
            reason: INVALID_REQUEST.to_owned(),
        }
    );
    assert_eq!(
        failure(method, Transport::Gateway, &answer),
        Some(format!(
            "{method} over the gateway: invalid_request: the request REQUESTS holds for it is \
             not its message (reason \"invalid_request\")"
        ))
    );
}

/// Catches: the verdict passing a refusal of the credential or a fault, or failing a refusal
/// about the data. Of the seventeen gRPC codes, exactly `UNIMPLEMENTED`, `PERMISSION_DENIED`,
/// `UNAUTHENTICATED` and the faults `UNKNOWN`, `INTERNAL`, `UNAVAILABLE` and `DATA_LOSS` fail a
/// call; `INVALID_ARGUMENT` fails it only with the gateway's reason `invalid_request`.
#[test]
fn only_an_unserved_method_a_refused_credential_a_fault_or_an_unreadable_request_fails() {
    let failing: Vec<Code> = (0..=16)
        .map(Code::from_i32)
        .filter(|code| {
            let answer = Answer {
                code: *code,
                reason: "unknown_host".to_owned(),
            };
            failure("m", Transport::Gateway, &answer).is_some()
        })
        .collect();
    assert_eq!(
        failing,
        vec![
            Code::Unknown,
            Code::PermissionDenied,
            Code::Unimplemented,
            Code::Internal,
            Code::Unavailable,
            Code::DataLoss,
            Code::Unauthenticated
        ]
    );
    let unreadable = Answer {
        code: Code::InvalidArgument,
        reason: INVALID_REQUEST.to_owned(),
    };
    assert!(failure("m", Transport::Grpc, &unreadable).is_some());
    let forbidden = Answer {
        code: Code::PermissionDenied,
        reason: "forbidden".to_owned(),
    };
    assert_eq!(
        failure("m", Transport::Grpc, &forbidden),
        Some(
            "m over gRPC: PERMISSION_DENIED: the app's credential is forbidden it \
             (reason \"forbidden\")"
                .to_owned()
        )
    );
}
