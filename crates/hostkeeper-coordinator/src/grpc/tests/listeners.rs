//! What each listener serves, and how the router tells a gRPC call from the JSON routes: the
//! heartbeat listener serves the agent's service only, the private one the operator API only,
//! the daemon's local service neither; HTTP/1.1 and a call with no gRPC content type are JSON
//! routes; and a gRPC body that never finishes is a deadline exceeded.

use std::time::Duration;

use bytes::Bytes;
use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_wire::wire::HEARTBEAT_PATH;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, StatusCode, Version};
use tonic::{Code, Status};

use super::support::{
    HEARTBEAT, Served, Stalled, call, call_in, framed, grpc_request, grpc_request_raw, grpc_with,
    raw, served, updater,
};
use crate::grpc::{Admitted, serve_admitted};
use crate::http::{Peer, Surface};
use crate::testutil::OPERATOR;

/// The path of `Hosts.Abort`, an operator method with a handler.
const ABORT: &str = "/hostkeeper.operator.v1.Hosts/Abort";

/// The answer of a gRPC service that is not served where it was called.
fn not_served() -> (Code, &'static str, Option<Bytes>) {
    (Code::Unimplemented, "", None)
}

/// The path of each RPC of `package` in the command table.
fn paths_of(package: &str) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|command| command.package == package)
        .map(|command| command.path)
        .collect()
}

/// `request` as a client of `version` sends it, with `content_type` (none when `None`).
fn sent_as<B>(
    mut request: Request<B>,
    version: Version,
    content_type: Option<&'static str>,
) -> Request<B> {
    *request.version_mut() = version;
    request.headers_mut().remove(CONTENT_TYPE);
    if let Some(content_type) = content_type {
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    request
}

/// Calls `path` on `surface` as a caller holding both an updater's certificate and the
/// operator token, and returns the status.
async fn called_on(
    served: &Served,
    surface: Surface,
    certificate: &Peer,
    path: &str,
) -> (Code, String, Option<Bytes>) {
    let request = grpc_request(path, (), Some(OPERATOR));
    call(&served.coordinator, surface, certificate, request).await
}

/// Proves: the heartbeat listener answers `UNIMPLEMENTED` with no message to every method of
/// the operator API, to a caller with an updater's certificate and the operator token.
#[tokio::test]
async fn the_heartbeat_listener_serves_no_operator_method() {
    let served = served("grpc-listen-heartbeat");
    let host = updater("host-1", "storage");
    let paths = paths_of("hostkeeper.operator.v1");
    assert!(!paths.is_empty());
    for path in paths {
        let (code, message, body) = called_on(&served, Surface::Heartbeat, &host, path).await;
        assert_eq!((code, message.as_str(), body), not_served(), "{path}");
    }
}

/// Proves: the private listener answers `UNIMPLEMENTED` with no message to every method of the
/// agent's service, `Heartbeat` included, to a caller with an updater's certificate and the
/// operator token.
#[tokio::test]
async fn the_private_listener_serves_no_agent_method() {
    let served = served("grpc-listen-private");
    let host = updater("host-1", "storage");
    let paths = paths_of("hostkeeper.agent.v1");
    assert!(paths.contains(&HEARTBEAT));
    for path in paths {
        let (code, message, body) = called_on(&served, Surface::Control, &host, path).await;
        assert_eq!((code, message.as_str(), body), not_served(), "{path}");
    }
}

/// Proves: the daemon's local service is served on neither listener, and neither is a service
/// that does not exist or a path with no method: each answers `UNIMPLEMENTED` with no message.
#[tokio::test]
async fn the_daemons_local_service_is_served_on_neither_listener() {
    let served = served("grpc-listen-local");
    let host = updater("host-1", "storage");
    for path in [
        "/hostkeeper.local.v1.Admin/Status",
        "/hostkeeper.local.v1.Admin/Abort",
        "/hostkeeper.local.v1.Admin/ClearRecord",
        "/no.such.v1.Service/Method",
        "/nomethod",
        "/",
    ] {
        for surface in [Surface::Heartbeat, Surface::Control] {
            let (code, message, body) = called_on(&served, surface, &host, path).await;
            assert_eq!(
                (code, message.as_str(), body),
                not_served(),
                "{path} on {surface:?}"
            );
        }
    }
}

/// Proves: a gRPC content type over HTTP/1.1 does not make a gRPC call, since gRPC is HTTP/2
/// only: on the heartbeat listener the heartbeat's gRPC path is a path the JSON routes do not
/// have (404 `not_found`, as JSON), and the same request over HTTP/2 is a gRPC call and gets
/// the gRPC refusal.
#[tokio::test]
async fn a_grpc_call_over_http_1_1_goes_to_the_json_routes() {
    let served = served("grpc-listen-h1-heartbeat");
    let request = || grpc_request_raw(HEARTBEAT, framed(b""), None);

    let over_h1 = sent_as(request(), Version::HTTP_11, Some("application/grpc"));
    let (status, headers, body) =
        raw(&served.coordinator, Surface::Heartbeat, &None, over_h1).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    assert!(headers.get("grpc-status").is_none());
    assert_eq!(body, Bytes::from_static(br#"{"error":"not_found"}"#));

    let answer = call(&served.coordinator, Surface::Heartbeat, &None, request()).await;
    assert_eq!(
        answer,
        (Code::PermissionDenied, "forbidden".to_owned(), None)
    );
}

/// Proves: on the private listener a gRPC content type over HTTP/1.1 is read by the JSON
/// gateway, not served as gRPC: the framed body is not the method's request as JSON, so the
/// answer is a 400 JSON refusal `INVALID_ARGUMENT` `invalid_request`; without a token it is a
/// 401 `UNAUTHENTICATED` `unauthorized`; and a frozen JSON route answers as it always did.
#[tokio::test]
async fn a_grpc_call_over_http_1_1_on_the_private_listener_is_the_gateways() {
    let served = served("grpc-listen-h1-private");
    let abort = |token| {
        sent_as(
            grpc_request_raw(ABORT, framed(b""), token),
            Version::HTTP_11,
            Some("application/grpc"),
        )
    };

    let (status, headers, body) = raw(
        &served.coordinator,
        Surface::Control,
        &None,
        abort(Some(OPERATOR)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    assert!(headers.get("grpc-status").is_none());
    assert_eq!(
        body,
        Bytes::from_static(br#"{"code":"INVALID_ARGUMENT","reason":"invalid_request"}"#)
    );

    let (status, _, body) = raw(&served.coordinator, Surface::Control, &None, abort(None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        Bytes::from_static(br#"{"code":"UNAUTHENTICATED","reason":"unauthorized"}"#)
    );

    let mut status_route = grpc_request_raw("/v1/status", Bytes::new(), Some(OPERATOR));
    *status_route.method_mut() = Method::GET;
    let status_route = sent_as(status_route, Version::HTTP_11, Some("application/grpc"));
    let (status, headers, _) =
        raw(&served.coordinator, Surface::Control, &None, status_route).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_TYPE], "application/json");
}

/// Proves: over HTTP/2 only a content type that begins `application/grpc` makes a gRPC call:
/// `application/grpc+proto` does, and no content type or `application/json` leaves the request
/// to the JSON routes (the heartbeat route's 403 `forbidden` for a caller with no certificate).
#[tokio::test]
async fn only_a_grpc_content_type_over_http_2_is_a_grpc_call() {
    let served = served("grpc-listen-content-type");
    let coordinator = &served.coordinator;

    let proto = sent_as(
        grpc_request_raw(HEARTBEAT, framed(b""), None),
        Version::HTTP_2,
        Some("application/grpc+proto"),
    );
    let answer = call(coordinator, Surface::Heartbeat, &None, proto).await;
    assert_eq!(
        answer,
        (Code::PermissionDenied, "forbidden".to_owned(), None)
    );

    for content_type in [None, Some("application/json")] {
        let json = grpc_request_raw(HEARTBEAT_PATH, Bytes::new(), None);
        let json = sent_as(json, Version::HTTP_2, content_type);
        let (status, headers, body) = raw(coordinator, Surface::Heartbeat, &None, json).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{content_type:?}");
        assert!(headers.get("grpc-status").is_none());
        assert_eq!(body, Bytes::from_static(br#"{"error":"forbidden"}"#));
    }
}

/// Proves: on the private listener a method of a path of the operator API other than `POST` is
/// a 405 `method_not_allowed` with no `Access-Control-*` header, while a path of the agent's or
/// the local package is no gateway path (404 `not_found`), and on the heartbeat listener no
/// path of the operator API is a route (404 `not_found`).
#[tokio::test]
async fn only_a_post_of_an_operator_method_is_a_gateway_path() {
    let served = served("grpc-listen-gateway-paths");
    let coordinator = &served.coordinator;
    let plain = |method: Method, path: &str| {
        let mut request = grpc_request_raw(path, Bytes::new(), Some(OPERATOR));
        *request.method_mut() = method;
        sent_as(request, Version::HTTP_11, Some("application/json"))
    };

    for method in [Method::GET, Method::OPTIONS] {
        let (status, headers, body) = raw(
            coordinator,
            Surface::Control,
            &None,
            plain(method.clone(), ABORT),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert!(
            headers
                .keys()
                .all(|name| !name.as_str().starts_with("access-control-"))
        );
        assert_eq!(
            body,
            Bytes::from_static(br#"{"error":"method_not_allowed"}"#)
        );
    }

    for (surface, path) in [
        (Surface::Control, HEARTBEAT),
        (Surface::Control, "/hostkeeper.local.v1.Admin/Status"),
        (Surface::Heartbeat, ABORT),
    ] {
        let (status, _, body) = raw(coordinator, surface, &None, plain(Method::POST, path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} on {surface:?}");
        assert_eq!(body, Bytes::from_static(br#"{"error":"not_found"}"#));
    }
}

/// Proves: a gRPC call whose body has not arrived whole after the patience is `DEADLINE_EXCEEDED`
/// with no message, and a caller refused before its body is read is refused as before: the body
/// is a frame that promises ten bytes and gives none more, the clock is paused so the patience
/// passes by the clock's own advance, and a call that arrives whole is answered, not timed out.
#[tokio::test(start_paused = true)]
async fn a_grpc_body_that_never_finishes_is_deadline_exceeded() {
    let served = served("grpc-listen-patience");
    let patience = Duration::from_secs(5);
    let coordinator = &served.coordinator;
    let promised_ten = Bytes::from_static(&[0, 0, 0, 0, 10, 1, 2]);

    let stalled = grpc_with(ABORT, Stalled::after(promised_ten.clone()), Some(OPERATOR));
    let answer = call_in(coordinator, Surface::Control, &None, stalled, patience).await;
    assert_eq!(answer, (Code::DeadlineExceeded, String::new(), None));

    let stalled = grpc_with(ABORT, Stalled::after(promised_ten), None);
    let answer = call_in(coordinator, Surface::Control, &None, stalled, patience).await;
    assert_eq!(
        answer,
        (Code::Unauthenticated, "unauthorized".to_owned(), None),
        "the caller is checked before the body is waited for"
    );

    let whole = grpc_request(ABORT, (), Some(OPERATOR));
    let (code, _, _) = call_in(coordinator, Surface::Control, &None, whole, patience).await;
    assert_ne!(code, Code::DeadlineExceeded);
}

/// Proves: a gateway body that has not arrived whole after the patience is a 400 JSON
/// `INVALID_ARGUMENT` `invalid_request`, never a hang.
#[tokio::test(start_paused = true)]
async fn a_gateway_body_that_never_finishes_is_an_invalid_request() {
    let served = served("grpc-listen-gateway-patience");
    let stalled = grpc_with(
        ABORT,
        Stalled::after(Bytes::from_static(b"{")),
        Some(OPERATOR),
    );
    let stalled = sent_as(stalled, Version::HTTP_11, Some("application/json"));
    let (status, _, body) = raw(&served.coordinator, Surface::Control, &None, stalled).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        Bytes::from_static(br#"{"code":"INVALID_ARGUMENT","reason":"invalid_request"}"#)
    );
}

/// Proves: the in-process server a gateway call and a gRPC call share answers `UNIMPLEMENTED`
/// with no message for a path of a service it does not have, even for a caller already admitted.
#[tokio::test]
async fn the_in_process_server_has_no_service_the_listeners_do_not_serve() {
    let served = served("grpc-listen-in-process");
    let request = grpc_request("/hostkeeper.local.v1.Admin/Status", (), None);
    let response = serve_admitted(&served.coordinator, Admitted::Peer(None), request).await;
    let status = Status::from_header_map(response.headers()).expect("a gRPC status");
    assert_eq!((status.code(), status.message()), (Code::Unimplemented, ""));
}
