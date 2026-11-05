//! `hostkeeper ctl` (the `hostkeeper-ctl` library) run against an in-process coordinator, once
//! over each transport it has: `--transport grpc` and `--transport gateway`. The transports are
//! stood in for by a [`Bridge`], which does what the network transports of ctl do on the wire
//! (a gRPC frame over HTTP/2, a canonical JSON `POST` over HTTP/1.1, and the same reading of the
//! answer) but hands the request to [`crate::grpc::route`] instead of a socket. Nothing here
//! opens one.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_ctl::codes;
use hostkeeper_ctl::transport::ConnectError;
use hostkeeper_ctl::{Connect, CtlTransport, Io, Target, TransportKind};
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode, Version};
use prost::Message as _;
use prost_reflect::{DynamicMessage, SerializeOptions};
use serde_json::Value;
use tokio::runtime::Runtime;
use tonic::Status;
use tonic::metadata::MetadataMap;

use crate::assignment::Entry;
use crate::grpc::gateway::is_method;
use crate::grpc::route;
use crate::http::Surface;
use crate::marks_support::{digest, rig};
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, ci_token, scratch};

/// How long the router waits for a request body; the bodies here are in memory.
const PATIENCE: Duration = Duration::from_secs(30);

/// The path of `Releases.SetDesired`.
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";

/// The path of `Releases.Force`.
const FORCE: &str = "/hostkeeper.operator.v1.Releases/Force";

/// The path of `Services.Assign`.
const ASSIGN: &str = "/hostkeeper.operator.v1.Services/Assign";

/// The path of `Services.Unassign`.
const UNASSIGN: &str = "/hostkeeper.operator.v1.Services/Unassign";

/// A `Connect` whose transport calls the router of `coordinator` in process, the way the
/// transport named by `kind` would over the network. It refuses a `Target` that names the other
/// transport, so a run that reads `--transport` wrongly fails.
#[derive(Clone)]
struct Bridge {
    runtime: Arc<Runtime>,
    coordinator: Arc<Coordinator>,
    kind: TransportKind,
    /// The path of every call, in order.
    paths: Arc<Mutex<Vec<String>>>,
}

impl Bridge {
    /// A bridge of the transport `kind` to `coordinator`, on a runtime of its own: ctl's
    /// transports block the calling thread, so the router runs on another thread's runtime.
    fn new(kind: TransportKind, coordinator: &Arc<Coordinator>) -> Self {
        Self {
            runtime: Arc::new(Runtime::new().expect("a runtime")),
            coordinator: Arc::clone(coordinator),
            kind,
            paths: Arc::default(),
        }
    }

    /// The flag value that selects this bridge's transport.
    fn flag(&self) -> &'static str {
        match self.kind {
            TransportKind::Grpc => "grpc",
            TransportKind::Gateway => "gateway",
        }
    }

    /// The paths called so far.
    fn paths(&self) -> Vec<String> {
        self.paths.lock().expect("the paths").clone()
    }

    /// The call as ctl's gRPC transport makes it: one length-prefixed message in an HTTP/2
    /// request with a gRPC content type; the status from the trailers (or the headers of an
    /// answer with no body); the message of the answer.
    fn over_grpc(&self, path: &str, request: &[u8], bearer: &str) -> Result<Bytes, Status> {
        let mut framed = vec![0u8];
        framed.extend_from_slice(&u32::try_from(request.len()).expect("a short").to_be_bytes());
        framed.extend_from_slice(request);
        let call = Request::builder()
            .method("POST")
            .uri(path)
            .version(Version::HTTP_2)
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .header("authorization", bearer)
            .body(Full::new(Bytes::from(framed)))
            .expect("a request");
        let response = self.runtime.block_on(route(
            &self.coordinator,
            Surface::Control,
            &None,
            call,
            PATIENCE,
        ));
        let (parts, body) = response.into_parts();
        let collected = self.runtime.block_on(body.collect()).expect("the body");
        let status = collected
            .trailers()
            .and_then(Status::from_header_map)
            .or_else(|| Status::from_header_map(&parts.headers))
            .expect("a grpc-status");
        if status.code() != tonic::Code::Ok {
            return Err(status);
        }
        let bytes = collected.to_bytes();
        let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        assert_eq!(
            usize::try_from(length).expect("a length"),
            bytes.len() - 5,
            "one whole message"
        );
        Ok(bytes.slice(5..))
    }

    /// The call as ctl's gateway client makes it: the request as canonical JSON in a `POST` of
    /// the path over HTTP/1.1; a 200 is the response as JSON, anything else is
    /// `{"code": NAME, "reason": TOKEN}`.
    fn over_gateway(&self, path: &str, request: &[u8], bearer: &str) -> Result<Bytes, Status> {
        let method = is_method(path).expect("a path of the gateway");
        let message = DynamicMessage::decode(method.input(), request).expect("a request");
        let mut json = serde_json::Serializer::new(Vec::new());
        message
            .serialize_with_options(&mut json, &SerializeOptions::new())
            .expect("canonical JSON");
        let call = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", bearer)
            .body(Full::new(Bytes::from(json.into_inner())))
            .expect("a request");
        let response = self.runtime.block_on(route(
            &self.coordinator,
            Surface::Control,
            &None,
            call,
            PATIENCE,
        ));
        let status = response.status();
        let body = self
            .runtime
            .block_on(response.into_body().collect())
            .expect("the body")
            .to_bytes();
        if status == StatusCode::OK {
            let mut reader = serde_json::Deserializer::from_slice(&body);
            let reply = DynamicMessage::deserialize(method.output(), &mut reader)
                .expect("canonical JSON of the response");
            Ok(Bytes::from(reply.encode_to_vec()))
        } else {
            let error: Value = serde_json::from_slice(&body).expect("a JSON error");
            let code = codes::from_name(error["code"].as_str().expect("a code name"))
                .expect("a code ctl knows");
            Err(Status::new(
                code,
                error["reason"].as_str().expect("a reason"),
            ))
        }
    }
}

impl Connect for Bridge {
    fn connect(&self, target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError> {
        assert_eq!(target.transport, self.kind, "the flag --transport");
        Ok(Box::new(self.clone()))
    }
}

impl CtlTransport for Bridge {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        self.paths.lock().expect("the paths").push(path.to_owned());
        let bearer = metadata
            .get("authorization")
            .expect("ctl sends the token")
            .to_str()
            .expect("visible ASCII");
        match self.kind {
            TransportKind::Grpc => self.over_grpc(path, &request, bearer),
            TransportKind::Gateway => self.over_gateway(path, &request, bearer),
        }
    }
}

/// What a run of ctl printed and returned.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Runs `hostkeeper ctl releases <rpc>` over `bridge` with `token` as the bearer and
/// `request` as the request file, in a scratch directory of its own.
fn ctl(bridge: &Bridge, token: &str, rpc: &[&str], request: &str) -> Ran {
    let files = scratch("ctl-request");
    let request_file = files.path().join("request.json");
    fs::write(&request_file, request).expect("request");
    let mut command: Vec<OsString> = vec!["releases".into()];
    command.extend(rpc.iter().map(OsString::from));
    command.push("--request".into());
    command.push(request_file.into_os_string());
    run_ctl(bridge, token, command)
}

/// Runs `hostkeeper ctl <command>` over `bridge` with `token` as the bearer, its CA and token
/// files in a scratch directory of its own.
fn run_ctl(bridge: &Bridge, token: &str, command: Vec<OsString>) -> Ran {
    let files = scratch("ctl-files");
    let ca = files.path().join("ca.pem");
    let token_file = files.path().join("token");
    // The bridge makes no TLS: it reads none of these, but ctl needs the flags.
    fs::write(&ca, "the bridge reads no CA").expect("ca");
    fs::write(&token_file, token).expect("token");
    let mut args: Vec<OsString> = vec![
        "hostkeeper".into(),
        "--endpoint".into(),
        "https://coordinator.test:8443".into(),
        "--ca-file".into(),
        ca.into_os_string(),
        "--token-file".into(),
        token_file.into_os_string(),
        "--transport".into(),
        bridge.flag().into(),
        "--output".into(),
        "json".into(),
    ];
    args.extend(command);
    let mut stdin = io::empty();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = hostkeeper_ctl::run(
        args,
        bridge,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    Ran {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8"),
        stderr: String::from_utf8(stderr).expect("UTF-8"),
    }
}

/// The bytes of the desired-state file under `dir`, if there is one.
fn desired_file(dir: &Path) -> Option<Vec<u8>> {
    fs::read(dir.join("desired.json")).ok()
}

/// Proves: `releases set-desired` run with `--transport grpc` against one coordinator and with
/// `--transport gateway` against another that started the same changes the same state. Both
/// exit 0 with nothing on stderr and print the same response (version 2.0 and the package
/// digest), each calls `Releases.SetDesired` once, and the two desired-state files and ops
/// files hold the same bytes.
///
/// Catches: a gateway or a gRPC adapter that sets a different desired state from the other, or
/// an answer that differs by transport.
#[test]
fn ctl_over_grpc_and_ctl_over_the_gateway_change_the_same_state() {
    let request = r#"{"desired":{"version":"2.0"}}"#;
    let package = digest(7);
    let rpc = [
        "set-desired",
        "--service",
        "web",
        "--package-digest",
        package.as_str(),
    ];
    let mut ran = Vec::new();
    let mut states = Vec::new();
    for (name, kind) in [
        ("ctl-same-grpc", TransportKind::Grpc),
        ("ctl-same-gateway", TransportKind::Gateway),
    ] {
        let farm = rig(name, "");
        assert_eq!(desired_file(farm.dir.path()), None, "nothing is set yet");
        let coordinator = Arc::new(farm.coordinator);
        let bridge = Bridge::new(kind, &coordinator);
        ran.push(ctl(&bridge, &ci_token(), &rpc, request));
        assert_eq!(bridge.paths(), [SET_DESIRED]);
        states.push((desired_file(farm.dir.path()), ops_bytes_of(farm.dir.path())));
    }

    let (grpc, gateway) = (&ran[0], &ran[1]);
    assert_eq!((grpc.code, grpc.stderr.as_str()), (0, ""), "grpc");
    assert_eq!((gateway.code, gateway.stderr.as_str()), (0, ""), "gateway");
    assert_eq!(grpc.stdout, gateway.stdout, "the same response");
    let response: Value = serde_json::from_str(&grpc.stdout).expect("JSON");
    assert_eq!(response["desired"]["version"], "2.0");
    assert_eq!(response["desired"]["packageDigest"], package.as_str());

    let written = states[0].0.as_deref().expect("a desired-state file");
    assert!(
        String::from_utf8_lossy(written).contains("2.0"),
        "the file names the version"
    );
    assert_eq!(states[0], states[1], "the same files on disk");
}

/// The bytes of the ops file under `dir`, if there is one.
fn ops_bytes_of(dir: &Path) -> Option<Vec<u8>> {
    fs::read(dir.join("desired.json.ops")).ok()
}

/// Proves: a force below the service's `min_version` is refused as `below_min_version` through
/// `--transport grpc` and through `--transport gateway`: ctl exits 2, prints nothing on stdout,
/// and prints exactly `refused: below_min_version` on stderr, and the refusal changes neither
/// the desired state nor the ops file.
///
/// Catches: a refusal whose token is lost on one transport (the gRPC message, the gateway's
/// `reason`), a refusal that maps to another exit code, or a refused force that writes.
#[test]
fn a_refusal_reaches_the_command_line_as_exit_2_with_the_token() {
    let farm = rig("ctl-refusal", "min_version = \"1.5\"");
    let dir = farm.dir.path().to_owned();
    let coordinator = Arc::new(farm.coordinator);
    let before = (desired_file(&dir), ops_bytes_of(&dir));
    let rpc = [
        "force",
        "--service",
        "web",
        "--version",
        "1.4",
        "--reason",
        "fix the farm",
    ];
    for kind in [TransportKind::Grpc, TransportKind::Gateway] {
        let bridge = Bridge::new(kind, &coordinator);
        let ran = ctl(
            &bridge,
            OPERATOR,
            &rpc,
            r#"{"fleet":{"confirmService":"web"}}"#,
        );
        assert_eq!(bridge.paths(), [FORCE], "{kind:?}");
        assert_eq!(ran.code, 2, "{kind:?}");
        assert_eq!(ran.stdout, "", "{kind:?}");
        assert_eq!(ran.stderr, "refused: below_min_version\n", "{kind:?}");
    }
    assert_eq!(before, (desired_file(&dir), ops_bytes_of(&dir)));
}

/// Proves: a token that is no operator's reaches the command line as exit 3 and exactly
/// `refused: unauthorized`, through both transports: the credential is checked before the
/// request is read, and the refusal's token survives both the gRPC status and the gateway's
/// error body.
///
/// Catches: a transport that turns an unauthenticated call into a generic failure (exit 1),
/// or a token that is dropped from the message.
#[test]
fn a_token_no_one_holds_reaches_the_command_line_as_exit_3_with_unauthorized() {
    let farm = rig("ctl-denied", "");
    let coordinator = Arc::new(farm.coordinator);
    let rpc = [
        "force",
        "--service",
        "web",
        "--version",
        "1.4",
        "--reason",
        "fix the farm",
    ];
    for kind in [TransportKind::Grpc, TransportKind::Gateway] {
        let bridge = Bridge::new(kind, &coordinator);
        let ran = ctl(
            &bridge,
            "not-an-operator-token-0123456789",
            &rpc,
            r#"{"fleet":{"confirmService":"web"}}"#,
        );
        assert_eq!(ran.code, 3, "{kind:?}");
        assert_eq!(ran.stdout, "", "{kind:?}");
        assert_eq!(ran.stderr, "refused: unauthorized\n", "{kind:?}");
    }
}

/// Proves: `services unassign --role` and `services assign --role`, parsed and run by ctl
/// through `--transport grpc` and `--transport gateway`, carry the role to the coordinator:
/// each exits 0 and prints the audit entry of its scope (`role storage`), the unassign leaves
/// the role's exclusion of `web` in the ops file, and the assign takes it off again.
///
/// Catches: a `--role` flag dropped or sent as the node id by either transport, so that the
/// call is refused for want of a scope, or changes another scope than the role.
#[test]
fn services_assign_and_unassign_by_role_run_through_ctl() {
    for (name, kind) in [
        ("ctl-role-grpc", TransportKind::Grpc),
        ("ctl-role-gateway", TransportKind::Gateway),
    ] {
        let farm = rig(name, "");
        let coordinator = Arc::new(farm.coordinator);
        let bridge = Bridge::new(kind, &coordinator);
        let by_role = |verb: &str| -> Vec<OsString> {
            ["services", verb, "--service", "web", "--role", "storage"]
                .iter()
                .map(OsString::from)
                .collect()
        };

        let ran = run_ctl(&bridge, OPERATOR, by_role("unassign"));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{kind:?}");
        let response: Value = serde_json::from_str(&ran.stdout).expect("JSON");
        assert_eq!(response["entry"]["action"], "unassign", "{kind:?}");
        assert_eq!(response["entry"]["reason"], "role storage", "{kind:?}");
        let excluded = coordinator.ops().state().assignments;
        assert_eq!(
            excluded["web"].roles.get("storage"),
            Some(&Entry::Exclude),
            "{kind:?}"
        );

        let ran = run_ctl(&bridge, OPERATOR, by_role("assign"));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{kind:?}");
        let response: Value = serde_json::from_str(&ran.stdout).expect("JSON");
        assert_eq!(response["entry"]["action"], "assign", "{kind:?}");
        assert_eq!(response["entry"]["reason"], "role storage", "{kind:?}");
        assert!(
            !coordinator.ops().state().assignments.contains_key("web"),
            "{kind:?}: the rollout file's role again"
        );
        assert_eq!(bridge.paths(), [UNASSIGN, ASSIGN], "{kind:?}");
    }
}
