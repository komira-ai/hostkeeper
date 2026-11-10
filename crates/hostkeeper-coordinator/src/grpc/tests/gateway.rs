//! The HTTP/JSON gateway of the operator API, driven in process through `crate::grpc::route` on
//! the private surface with the HTTP/1.1 requests a gateway client (`hkctl`, the web app) sends.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Body, Frame};
use hyper::{HeaderMap, Request, Response, StatusCode, Version};
use prost::Message as _;
use prost_reflect::DescriptorPool;
use prost_types::{
    DescriptorProto, FileDescriptorProto, FileDescriptorSet, MethodDescriptorProto,
    ServiceDescriptorProto,
};
use serde_json::{Value, json};
use tonic::body::Body as GrpcBody;
use tonic::{Code, Status};

use crate::grpc::gateway::{
    STATUS_TABLE, answer, find, frame, is_method, pool, refusal, reply, to_json, to_message,
    unframe,
};
use crate::grpc::route;
use crate::http::Surface;
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, Rig, ci_token, scratch};

/// How long a body has in these tests.
const PATIENCE: Duration = Duration::from_secs(30);

/// A request the gateway's client would send, with the bearer and body given.
fn http(
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: impl Into<Bytes>,
) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Full::new(body.into())).expect("a request")
}

/// A gRPC call of `path` with `message`, as an HTTP/2 client sends it.
fn grpc_call(path: &str, bearer: &str, message: &impl prost::Message) -> Request<Full<Bytes>> {
    Request::builder()
        .method("POST")
        .uri(path)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Full::new(frame(&message.encode_to_vec())))
        .expect("a request")
}

/// The whole body of `body`.
async fn bytes_of<B>(body: B) -> Vec<u8>
where
    B: Body,
    B::Error: Debug,
{
    body.collect().await.expect("a body").to_bytes().to_vec()
}

/// A coordinator and the directory of its files.
struct Gateway {
    coordinator: Arc<Coordinator>,
    dir: Scratch,
}

/// What a gateway answered: the status, the headers and the body.
struct Answered {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answered {
    /// The body as JSON.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    /// The body as text.
    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("a text body")
    }
}

impl Gateway {
    fn new(name: &str) -> Self {
        let dir = scratch(name);
        let clock = Arc::new(AtomicU64::new(1_000));
        let coordinator = Arc::new(Rig::start(&dir, &clock));
        Self { coordinator, dir }
    }

    /// The answer of the private listener's entry point to `request`.
    async fn send(&self, request: Request<Full<Bytes>>) -> Answered {
        self.send_on(Surface::Control, request).await
    }

    async fn send_on(&self, surface: Surface, request: Request<Full<Bytes>>) -> Answered {
        let response = route(&self.coordinator, surface, &None, request, PATIENCE).await;
        let (parts, body) = response.into_parts();
        Answered {
            status: parts.status,
            headers: parts.headers,
            body: bytes_of(body).await,
        }
    }

    /// A gateway call with the operator's token.
    async fn operator(&self, path: &str, body: &str) -> Answered {
        self.send(http("POST", path, Some(OPERATOR), body.to_owned()))
            .await
    }

    /// The bytes of the state file and of the ops file, or `None` for a file not written yet.
    fn files(&self) -> [Option<Vec<u8>>; 2] {
        ["desired.json", "desired.json.ops"]
            .map(|name| std::fs::read(self.dir.path().join(name)).ok())
    }
}

const EXCLUDE: &str = "/hostkeeper.operator.v1.Hosts/Exclude";
const INCLUDE: &str = "/hostkeeper.operator.v1.Hosts/Include";
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
const EXCLUDE_BODY: &str = r#"{"service":"kbf","nodeId":"h1","reason":"gone"}"#;
const SET_BODY: &str =
    r#"{"service":"kbf","desired":{"version":"2.0","minUp":{"storage":1,"frontend":0}}}"#;

/// The exact text of a gateway refusal.
fn refused(code: &str, reason: &str) -> String {
    format!(r#"{{"code":"{code}","reason":"{reason}"}}"#)
}

/// Proves: `POST /<package>.<Service>/<Method>` with canonical JSON and CI's token on
/// `Releases/SetDesired` writes the desired file byte for byte as the gRPC call of the same
/// message does on a twin coordinator, and answers 200 with the stored state as JSON; operator
/// methods (`Hosts/Exclude`, `Hosts/Include`) answer 200 with canonical JSON of their response
/// whose entry names the action, the service and the host.
#[tokio::test]
async fn gateway_maps_post_package_service_method_to_the_rpc() {
    let over_http = Gateway::new("gw-set-desired-http");
    let set = over_http
        .send(http("POST", SET_DESIRED, Some(&ci_token()), SET_BODY))
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text());
    assert_eq!(
        set.headers.get("content-type").map(|v| v.as_bytes()),
        Some(&b"application/json"[..])
    );
    assert_eq!(set.json()["desired"]["version"], "2.0");
    assert_eq!(
        set.json()["desired"]["minUp"],
        json!({"storage": 1, "frontend": 0})
    );

    let over_grpc = Gateway::new("gw-set-desired-grpc");
    let message = proto::SetDesiredRequest {
        service: "kbf".to_owned(),
        desired: Some(proto::DesiredState {
            version: "2.0".to_owned(),
            min_up: [("storage".to_owned(), 1), ("frontend".to_owned(), 0)].into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let call = over_grpc
        .send(grpc_call(SET_DESIRED, &ci_token(), &message))
        .await;
    assert_eq!(call.status, StatusCode::OK);
    let written = over_http.files();
    assert!(
        written[0]
            .as_deref()
            .is_some_and(|file| String::from_utf8_lossy(file).contains("2.0")),
        "the gateway wrote no desired state"
    );
    assert_eq!(written[0], over_grpc.files()[0]);

    let operator = Gateway::new("gw-operator-methods");
    let excluded = operator.operator(EXCLUDE, EXCLUDE_BODY).await;
    assert_eq!(excluded.status, StatusCode::OK, "{}", excluded.text());
    let entry = &excluded.json()["entry"];
    assert_eq!(entry["action"], "exclude");
    assert_eq!(entry["service"], "kbf");
    assert_eq!(entry["nodeId"], "h1");
    let included = operator
        .operator(INCLUDE, r#"{"service":"kbf","nodeId":"h1"}"#)
        .await;
    assert_eq!(included.status, StatusCode::OK, "{}", included.text());
    assert_eq!(included.json()["entry"]["action"], "include");
}

/// Proves: the gateway serves nothing but `POST` of the unary methods of the operator package.
/// Any other HTTP method on a method path is a 405, and a path that names no method of that
/// package (the agent's `Heartbeat`, the daemon's local service, a method that does not exist,
/// `/`, `/index.html`, any other path) is a 404, on the private surface; the heartbeat surface
/// never serves a method path. None of it changes the state file or the ops file.
#[tokio::test]
async fn gateway_has_no_other_route() {
    let gateway = Gateway::new("gw-no-other-route");
    let before = gateway.files();
    for method in ["GET", "PUT", "DELETE", "PATCH", "OPTIONS"] {
        let got = gateway
            .send(http(method, EXCLUDE, Some(OPERATOR), EXCLUDE_BODY))
            .await;
        assert_eq!(got.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(got.text(), r#"{"error":"method_not_allowed"}"#, "{method}");
    }
    for path in [
        "/hostkeeper.agent.v1.Coordinator/Heartbeat",
        "/hostkeeper.local.v1.Admin/Status",
        "/hostkeeper.operator.v1.Hosts/Nope",
        "/hostkeeper.operator.v1.Nope/Exclude",
        "/hostkeeper.operator.v1.Hosts/Exclude/extra",
        "/hostkeeper.operator.v1.Hosts",
        "/",
        "/index.html",
        "/v1/anything-else",
    ] {
        for method in ["POST", "GET"] {
            let got = gateway
                .send(http(method, path, Some(OPERATOR), EXCLUDE_BODY))
                .await;
            assert_eq!(got.status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(got.text(), r#"{"error":"not_found"}"#, "{method} {path}");
        }
    }
    let heartbeat_listener = gateway
        .send_on(
            Surface::Heartbeat,
            http("POST", EXCLUDE, Some(OPERATOR), EXCLUDE_BODY),
        )
        .await;
    assert_eq!(heartbeat_listener.status, StatusCode::NOT_FOUND);
    assert_eq!(gateway.files(), before);
}

/// The HTTP status and the name of each gRPC code, written out here and not read from the code
/// under test.
const EXPECTED: [(Code, &str, u16); 17] = [
    (Code::Ok, "OK", 200),
    (Code::Cancelled, "CANCELLED", 499),
    (Code::Unknown, "UNKNOWN", 500),
    (Code::InvalidArgument, "INVALID_ARGUMENT", 400),
    (Code::DeadlineExceeded, "DEADLINE_EXCEEDED", 504),
    (Code::NotFound, "NOT_FOUND", 404),
    (Code::AlreadyExists, "ALREADY_EXISTS", 409),
    (Code::PermissionDenied, "PERMISSION_DENIED", 403),
    (Code::ResourceExhausted, "RESOURCE_EXHAUSTED", 429),
    (Code::FailedPrecondition, "FAILED_PRECONDITION", 400),
    (Code::Aborted, "ABORTED", 409),
    (Code::OutOfRange, "OUT_OF_RANGE", 400),
    (Code::Unimplemented, "UNIMPLEMENTED", 501),
    (Code::Internal, "INTERNAL", 500),
    (Code::Unavailable, "UNAVAILABLE", 503),
    (Code::DataLoss, "DATA_LOSS", 500),
    (Code::Unauthenticated, "UNAUTHENTICATED", 401),
];

/// Proves: `STATUS_TABLE` is the 17 gRPC codes in number order with the names and HTTP statuses
/// above; the refusal of a status of each code is that HTTP status with exactly
/// `{"code":"<NAME>","reason":"<message>"}` as JSON; and a real refusal end to end (an unknown
/// service, a host never heard from) comes out in that shape with its token.
#[tokio::test]
async fn gateway_error_body_maps_every_grpc_code() {
    assert_eq!(STATUS_TABLE, EXPECTED);
    for (n, (code, name, http_status)) in EXPECTED.into_iter().enumerate() {
        assert_eq!(code as usize, n, "{name} is out of number order");
        let reason = format!("reason_{n}");
        let response = refusal(&Status::new(code, reason.clone()));
        assert_eq!(response.status().as_u16(), http_status, "{name}");
        assert_eq!(
            response.headers().get("content-type").map(|v| v.as_bytes()),
            Some(&b"application/json"[..])
        );
        let body = bytes_of(response.into_body()).await;
        assert_eq!(
            String::from_utf8(body).expect("text"),
            refused(name, &reason)
        );
    }
    let quoted = refusal(&Status::aborted("say \"x\""));
    assert_eq!(
        String::from_utf8(bytes_of(quoted.into_body()).await).expect("text"),
        r#"{"code":"ABORTED","reason":"say \"x\""}"#
    );

    let gateway = Gateway::new("gw-error-body");
    let unknown = gateway
        .operator(
            EXCLUDE,
            r#"{"service":"nope","nodeId":"h1","reason":"gone"}"#,
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.text(), refused("NOT_FOUND", "unknown_service"));
    let ghost = gateway
        .operator("/hostkeeper.operator.v1.Hosts/Get", r#"{"nodeId":"ghost"}"#)
        .await;
    assert_eq!(ghost.status, StatusCode::NOT_FOUND);
    assert_eq!(ghost.text(), refused("NOT_FOUND", "unknown_host"));
}

/// Proves: no gateway answer carries a header that starts with `access-control-`, whether it
/// is a success, a refusal of the credential, a refusal of the body, or a 405, and an `OPTIONS`
/// pre-flight request with `Origin` and `Access-Control-Request-Method` is a 405, so a page from
/// another origin cannot call the gateway.
#[tokio::test]
async fn gateway_sends_no_cors_headers_and_refuses_options() {
    let gateway = Gateway::new("gw-no-cors");
    let preflight = Request::builder()
        .method("OPTIONS")
        .uri(EXCLUDE)
        .header("origin", "https://example.org")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "authorization")
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let options = gateway.send(preflight).await;
    assert_eq!(options.status, StatusCode::METHOD_NOT_ALLOWED);
    let with_origin = |request: Request<Full<Bytes>>| {
        let (mut parts, body) = request.into_parts();
        parts
            .headers
            .insert("origin", "https://example.org".parse().expect("a value"));
        Request::from_parts(parts, body)
    };
    let answers = [
        options,
        gateway
            .send(with_origin(http(
                "POST",
                EXCLUDE,
                Some(OPERATOR),
                EXCLUDE_BODY,
            )))
            .await,
        gateway
            .send(with_origin(http("POST", EXCLUDE, None, EXCLUDE_BODY)))
            .await,
        gateway
            .send(with_origin(http("POST", EXCLUDE, Some(OPERATOR), "{")))
            .await,
        gateway
            .send(with_origin(http("GET", EXCLUDE, Some(OPERATOR), "")))
            .await,
    ];
    let statuses: Vec<u16> = answers.iter().map(|a| a.status.as_u16()).collect();
    assert_eq!(statuses, [405, 200, 401, 400, 405]);
    for answer in &answers {
        let cors: Vec<_> = answer
            .headers
            .keys()
            .filter(|name| name.as_str().starts_with("access-control-"))
            .collect();
        assert!(cors.is_empty(), "{} has {cors:?}", answer.status);
    }
}

/// Proves: a gateway POST of each method of `HostGroups` is served, not `UNIMPLEMENTED`: with the
/// operator token it is answered by its handler (the empty body of `List` is 200 with a page of
/// no group, `{}`; that of `Get`, `Put`, `Remove` and `Move` is 400 `invalid_request`), and with
/// no token at all it is 401 `unauthorized`, before the body is read. No refusal changes a file.
#[tokio::test]
async fn gateway_serves_host_groups_and_checks_the_token_first() {
    let gateway = Gateway::new("gw-host-groups");
    let before = gateway.files();
    let unauthorized = refused("UNAUTHENTICATED", "unauthorized");
    let invalid = refused("INVALID_ARGUMENT", "invalid_request");
    for (path, status, body) in [
        ("List", StatusCode::OK, "{}"),
        ("Get", StatusCode::BAD_REQUEST, invalid.as_str()),
        ("Put", StatusCode::BAD_REQUEST, invalid.as_str()),
        ("Remove", StatusCode::BAD_REQUEST, invalid.as_str()),
        ("Move", StatusCode::BAD_REQUEST, invalid.as_str()),
    ] {
        let path = format!("/hostkeeper.operator.v1.HostGroups/{path}");
        let with_token = gateway.operator(&path, "{}").await;
        assert_eq!(with_token.status, status, "{path}");
        assert_eq!(with_token.text(), body, "{path}");
        let without = gateway.send(http("POST", &path, None, "{")).await;
        assert_eq!(without.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(without.text(), unauthorized, "{path}");
    }
    assert_eq!(gateway.files(), before);
}

/// Proves: the caller is checked before the body is read and by the rule of the method: no
/// bearer, a wrong one, a bearer of another scheme and one longer than the most the coordinator
/// reads are 401 `unauthorized`; CI's token on an operator method is 403 `forbidden`; the
/// bearer on `SetDesired`, which takes CI's token or the operator's, is 401 `unauthorized`, even
/// with a body that is not JSON. No refusal changes a file.
#[tokio::test]
async fn gateway_refuses_a_missing_or_wrong_bearer() {
    let gateway = Gateway::new("gw-bearer");
    let before = gateway.files();
    let unauthorized = refused("UNAUTHENTICATED", "unauthorized");
    let long = "x".repeat(crate::service::MAX_BEARER_BYTES + 1);
    let basic = Request::builder()
        .method("POST")
        .uri(EXCLUDE)
        .header("authorization", "Basic abc")
        .body(Full::new(Bytes::from(EXCLUDE_BODY)))
        .expect("a request");
    let cases = [
        http("POST", EXCLUDE, None, EXCLUDE_BODY),
        http("POST", EXCLUDE, Some("wrong"), EXCLUDE_BODY),
        http("POST", EXCLUDE, Some(&long), EXCLUDE_BODY),
        basic,
        http("POST", EXCLUDE, Some("wrong"), "not json"),
        http("POST", SET_DESIRED, Some("wrong"), SET_BODY),
        http("POST", SET_DESIRED, Some("wrong"), "not json"),
        http("POST", SET_DESIRED, None, SET_BODY),
    ];
    for (n, request) in cases.into_iter().enumerate() {
        let got = gateway.send(request).await;
        assert_eq!(got.status, StatusCode::UNAUTHORIZED, "case {n}");
        assert_eq!(got.text(), unauthorized, "case {n}");
    }
    let ci_on_operator = gateway
        .send(http("POST", EXCLUDE, Some(&ci_token()), EXCLUDE_BODY))
        .await;
    assert_eq!(ci_on_operator.status, StatusCode::FORBIDDEN);
    assert_eq!(
        ci_on_operator.text(),
        refused("PERMISSION_DENIED", "forbidden")
    );
    assert_eq!(gateway.files(), before);
}

/// Proves: a body that is not the method's request as JSON is 400 `INVALID_ARGUMENT` with the
/// reason `invalid_request` and changes nothing: text that is not JSON, a document followed by
/// anything, a field the message does not have, a value of the wrong type, and an empty body.
#[tokio::test]
async fn gateway_refuses_bad_json_and_unknown_fields() {
    let gateway = Gateway::new("gw-bad-json");
    let before = gateway.files();
    for (what, body) in [
        ("not JSON", "this is not json"),
        (
            "trailing data",
            r#"{"service":"kbf","nodeId":"h1","reason":"gone"} x"#,
        ),
        ("two documents", r#"{"service":"kbf"}{"service":"kbf"}"#),
        (
            "an unknown field",
            r#"{"service":"kbf","nodeId":"h1","reason":"gone","bogus":1}"#,
        ),
        (
            "a wrong type",
            r#"{"service":5,"nodeId":"h1","reason":"gone"}"#,
        ),
        ("a wrong shape", r#"["kbf"]"#),
        ("an empty body", ""),
    ] {
        let got = gateway.operator(EXCLUDE, body).await;
        assert_eq!(got.status, StatusCode::BAD_REQUEST, "{what}");
        assert_eq!(
            got.text(),
            refused("INVALID_ARGUMENT", "invalid_request"),
            "{what}"
        );
    }
    assert_eq!(gateway.files(), before);
}

/// Proves: a request body of 4096 bytes is read and answered, and the same request padded to
/// 4097 bytes is refused as `invalid_request` before anything is done, so the bound is exactly
/// 4 KiB.
#[tokio::test]
async fn gateway_body_is_bounded() {
    let gateway = Gateway::new("gw-bounded");
    let before = gateway.files();
    let padded = |size: usize| format!("{EXCLUDE_BODY:<size$}");
    assert_eq!(padded(4097).len(), 4097);
    let over = gateway.operator(EXCLUDE, &padded(4097)).await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST);
    assert_eq!(over.text(), refused("INVALID_ARGUMENT", "invalid_request"));
    assert_eq!(gateway.files(), before);
    let at = gateway.operator(EXCLUDE, &padded(4096)).await;
    assert_eq!(at.status, StatusCode::OK, "{}", at.text());
    assert_eq!(at.json()["entry"]["action"], "exclude");
    assert_ne!(gateway.files(), before);
}

/// Proves: `answer` given a path that names no method answers `NOT_FOUND` with an empty reason
/// and does not read the body or check a caller.
#[tokio::test]
async fn gateway_answer_refuses_a_path_that_is_no_method() {
    let gateway = Gateway::new("gw-answer-no-method");
    let got = answer(
        &gateway.coordinator,
        &None,
        http("POST", "/", Some(OPERATOR), "{}"),
        PATIENCE,
    )
    .await;
    assert_eq!(got.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        bytes_of(got.into_body()).await,
        refused("NOT_FOUND", "").into_bytes()
    );
}

/// Proves: the embedded descriptors are the pool of the gateway (`pool` decodes them and refuses
/// bytes that are no descriptor set), and `is_method` finds exactly the unary methods of the
/// operator package.
#[test]
fn gateway_pool_decodes_the_embedded_descriptors_only() {
    assert!(pool(hostkeeper_proto::FILE_DESCRIPTOR_SET).is_some());
    assert!(pool(&[0xff]).is_none());
    for path in [EXCLUDE, SET_DESIRED, "/hostkeeper.operator.v1.Audit/List"] {
        let method = is_method(path).expect("a gateway path");
        assert_eq!(
            format!("/{}/{}", method.parent_service().full_name(), method.name()),
            path
        );
    }
    for path in [
        "/hostkeeper.agent.v1.Coordinator/Heartbeat",
        "/hostkeeper.operator.v1.Hosts/Nope",
        "hostkeeper.operator.v1.Hosts/Exclude",
        "/hostkeeper.operator.v1.Hosts",
        "/nope.Service/Method",
        "",
    ] {
        assert!(is_method(path).is_none(), "{path}");
    }
}

/// A pool whose package `hostkeeper.operator.v1` has the service `S` with one unary method `U`,
/// one client-streaming `C` and one server-streaming `V`.
fn streaming_pool() -> DescriptorPool {
    let method = |name: &str, client: bool, server: bool| MethodDescriptorProto {
        name: Some(name.to_owned()),
        input_type: Some(".hostkeeper.operator.v1.M".to_owned()),
        output_type: Some(".hostkeeper.operator.v1.M".to_owned()),
        client_streaming: Some(client),
        server_streaming: Some(server),
        ..Default::default()
    };
    let file = FileDescriptorProto {
        name: Some("tiny.proto".to_owned()),
        package: Some("hostkeeper.operator.v1".to_owned()),
        syntax: Some("proto3".to_owned()),
        message_type: vec![DescriptorProto {
            name: Some("M".to_owned()),
            ..Default::default()
        }],
        service: vec![ServiceDescriptorProto {
            name: Some("S".to_owned()),
            method: vec![
                method("U", false, false),
                method("C", true, false),
                method("V", false, true),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    DescriptorPool::from_file_descriptor_set(FileDescriptorSet { file: vec![file] })
        .expect("a descriptor pool")
}

/// Proves: `find` serves a unary method of the package asked for and nothing else: not a method
/// of another package, not a missing service or method, not a path without both parts, and
/// neither a client-streaming nor a server-streaming method.
#[test]
fn gateway_finds_only_unary_methods_of_its_package() {
    let tiny = streaming_pool();
    let package = "hostkeeper.operator.v1";
    let found = find(&tiny, package, "/hostkeeper.operator.v1.S/U").expect("the unary method");
    assert_eq!(found.name(), "U");
    assert!(find(&tiny, "other.package", "/hostkeeper.operator.v1.S/U").is_none());
    assert!(find(&tiny, package, "/hostkeeper.operator.v1.S/C").is_none());
    assert!(find(&tiny, package, "/hostkeeper.operator.v1.S/V").is_none());
    assert!(find(&tiny, package, "/hostkeeper.operator.v1.S/Missing").is_none());
    assert!(find(&tiny, package, "/hostkeeper.operator.v1.Missing/U").is_none());
    assert!(find(&tiny, package, "/hostkeeper.operator.v1.S").is_none());
    assert!(find(&tiny, package, "hostkeeper.operator.v1.S/U").is_none());
}

/// Proves: `unframe` returns the message of exactly one uncompressed gRPC message and nothing
/// for fewer than five bytes, a set compression flag, or a length that is not the bytes that
/// follow (longer or shorter); `frame` writes a zero flag, the length as four big-endian bytes
/// and the message, and `unframe` reads it back.
#[test]
fn gateway_frames_and_unframes_one_grpc_message() {
    assert_eq!(unframe(&[0, 0, 0, 0, 1, 7]), Some(&[7][..]));
    assert_eq!(unframe(&[0, 0, 0, 0, 0]), Some(&[][..]));
    assert_eq!(unframe(&[0, 0, 0, 0]), None);
    assert_eq!(unframe(&[]), None);
    assert_eq!(unframe(&[1, 0, 0, 0, 1, 7]), None);
    assert_eq!(unframe(&[0, 0, 0, 0, 2, 7]), None);
    assert_eq!(unframe(&[0, 0, 0, 0, 0, 7]), None);
    assert_eq!(unframe(&[0, 0, 0, 1, 0, 7]), None);
    assert_eq!(&frame(&[9, 8, 7])[..], &[0, 0, 0, 0, 3, 9, 8, 7]);
    assert_eq!(unframe(&frame(&[9, 8, 7])), Some(&[9, 8, 7][..]));
}

/// Proves: `to_message` reads one JSON document of the method's request and refuses malformed
/// JSON, trailing data and an unknown field; `to_json` turns one framed response message into
/// its canonical JSON (64-bit numbers as strings) and refuses bytes that are no frame and a
/// frame that is no message.
#[test]
fn gateway_converts_between_json_and_messages() {
    let method = is_method(EXCLUDE).expect("a method");
    let message = to_message(&method, EXCLUDE_BODY.as_bytes()).expect("a request");
    let decoded =
        proto::ExcludeRequest::decode(message.encode_to_vec().as_slice()).expect("decode");
    assert_eq!(
        (
            decoded.service.as_str(),
            decoded.node_id.as_str(),
            decoded.reason.as_str()
        ),
        ("kbf", "h1", "gone")
    );
    let malformed: [&[u8]; 4] = [b"{", b"{} {}", br#"{"bogus":1}"#, b""];
    for bad in malformed {
        assert!(to_message(&method, bad).is_none(), "{bad:?}");
    }

    let response = proto::ExcludeResponse {
        entry: Some(proto::AuditEntry {
            seq: 3,
            action: "exclude".to_owned(),
            node_id: Some("h1".to_owned()),
            ..Default::default()
        }),
    };
    let json = to_json(&method, &frame(&response.encode_to_vec())).expect("JSON");
    assert_eq!(
        serde_json::from_slice::<Value>(&json).expect("JSON"),
        json!({"entry": {"seq": "3", "action": "exclude", "nodeId": "h1"}})
    );
    assert!(to_json(&method, &[0, 0, 0]).is_none());
    assert!(to_json(&method, &frame(&[0xff])).is_none());
}

/// A gRPC response of `frames`.
fn grpc_response(
    headers: &[(&str, &str)],
    frames: Vec<Result<Frame<Bytes>, Status>>,
) -> Response<GrpcBody> {
    let mut builder = Response::builder();
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(GrpcBody::new(StreamBody::new(futures::stream::iter(
            frames,
        ))))
        .expect("a response")
}

/// The trailers of a call that ended with `status` and `message`.
fn trailers(status: &str, message: Option<&str>) -> Result<Frame<Bytes>, Status> {
    let mut map = HeaderMap::new();
    map.insert("grpc-status", status.parse().expect("a value"));
    if let Some(message) = message {
        map.insert("grpc-message", message.parse().expect("a value"));
    }
    Ok(Frame::trailers(map))
}

/// Proves: `reply` takes the status of a call from its trailers when it has them, else from the
/// headers of a response that has no body, and is `INTERNAL` with no message when it has neither;
/// an OK call is the canonical JSON of its message, `INTERNAL` when the message does not decode;
/// a status that is not OK is returned whatever the body holds; a body that fails is its error.
#[tokio::test]
async fn gateway_reply_reads_the_status_from_trailers_then_headers() {
    let method = is_method(EXCLUDE).expect("a method");
    let message = proto::ExcludeResponse {
        entry: Some(proto::AuditEntry {
            action: "exclude".to_owned(),
            ..Default::default()
        }),
    };
    let data = || Ok(Frame::data(frame(&message.encode_to_vec())));

    let ok = reply(
        &method,
        grpc_response(&[], vec![data(), trailers("0", None)]),
    )
    .await
    .expect("OK");
    assert_eq!(
        serde_json::from_slice::<Value>(&ok).expect("JSON"),
        json!({"entry": {"action": "exclude"}})
    );

    let from_trailers = reply(
        &method,
        grpc_response(
            &[("grpc-status", "0")],
            vec![data(), trailers("5", Some("unknown_service"))],
        ),
    )
    .await
    .expect_err("refused");
    assert_eq!(from_trailers.code(), Code::NotFound);
    assert_eq!(from_trailers.message(), "unknown_service");

    let from_headers = reply(
        &method,
        grpc_response(
            &[("grpc-status", "7"), ("grpc-message", "forbidden")],
            vec![],
        ),
    )
    .await
    .expect_err("refused");
    assert_eq!(from_headers.code(), Code::PermissionDenied);
    assert_eq!(from_headers.message(), "forbidden");

    let neither = reply(&method, grpc_response(&[], vec![data()]))
        .await
        .expect_err("no status");
    assert_eq!(neither.code(), Code::Internal);
    assert_eq!(neither.message(), "");

    let garbled = reply(
        &method,
        grpc_response(
            &[],
            vec![Ok(Frame::data(frame(&[0xff]))), trailers("0", None)],
        ),
    )
    .await
    .expect_err("not a message");
    assert_eq!(garbled.code(), Code::Internal);
    assert_eq!(garbled.message(), "");

    let empty = reply(&method, grpc_response(&[], vec![trailers("0", None)]))
        .await
        .expect_err("no message");
    assert_eq!(empty.code(), Code::Internal);

    let failed = reply(
        &method,
        grpc_response(&[], vec![Err(Status::data_loss("cut off"))]),
    )
    .await
    .expect_err("a failed body");
    assert_eq!(failed.code(), Code::DataLoss);
    assert_eq!(failed.message(), "cut off");
}

/// Proves: the status table in `docs/gateway.md`, between its markers, lists the same codes with
/// the same HTTP statuses in the same order as `STATUS_TABLE`.
#[test]
fn the_gateway_doc_status_table_equals_the_code() {
    let text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/gateway.md"
    ));
    let (_, after) = text
        .split_once("<!-- status-table -->")
        .expect("the opening marker");
    let (table, _) = after
        .split_once("<!-- /status-table -->")
        .expect("the closing marker");
    let rows: Vec<(String, u16)> = table
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            let [name, status] = cells[..] else {
                return None;
            };
            let name = name.strip_prefix('`')?.strip_suffix('`')?;
            Some((name.to_owned(), status.parse().ok()?))
        })
        .collect();
    let code: Vec<(String, u16)> = STATUS_TABLE
        .iter()
        .map(|(_, name, status)| ((*name).to_owned(), *status))
        .collect();
    assert_eq!(rows, code);
}
