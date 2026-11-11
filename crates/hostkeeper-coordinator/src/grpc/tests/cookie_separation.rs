//! A cookie is ambient authority and a bearer header is not (docs/design/operator-login.md,
//! section 5.3), so the two credentials are kept apart: a gRPC call reads the `Authorization`
//! header and never a cookie, and an answer to a bearer call never sets one. These tests were
//! written, and passed, before the coordinator learned of sessions, and they hold after it.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{COOKIE, HeaderValue, SET_COOKIE};
use hyper::{HeaderMap, Request};
use tonic::Code;

use super::credentials::operator_paths;
use super::support::{PATIENCE, call, grpc_request, served};
use crate::grpc::route;
use crate::http::Surface;
use crate::service::Coordinator;
use crate::testutil::OPERATOR;

/// The cookie the login route sets, with a value shaped like a session id.
const COOKIE_NAME: &str = "__Host-hk_session";
const SESSION_ID: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn with_cookie<B>(mut request: Request<B>) -> Request<B> {
    request.headers_mut().insert(
        COOKIE,
        HeaderValue::from_str(&format!("{COOKIE_NAME}={SESSION_ID}")).expect("a header value"),
    );
    request
}

/// The headers and trailers of what `route` answered `request` with.
async fn everything_said(
    coordinator: &Arc<Coordinator>,
    request: Request<Full<Bytes>>,
) -> Vec<HeaderMap> {
    let response = route(coordinator, Surface::Control, &None, request, PATIENCE).await;
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("a response body");
    let mut said = vec![parts.headers];
    said.extend(collected.trailers().cloned());
    said
}

fn sets_a_cookie(said: &[HeaderMap]) -> bool {
    said.iter().any(|headers| headers.contains_key(SET_COOKIE))
}

/// Proves: a gRPC call that carries a session cookie and no `Authorization` header is refused
/// as a call with no credential, on every method of the operator API: the cookie is not read on
/// this path.
#[tokio::test]
async fn a_grpc_call_with_only_a_session_cookie_is_refused_on_every_operator_method() {
    let served = served("cookie-separation-grpc");
    for path in operator_paths() {
        let request = with_cookie(grpc_request(path, (), None));
        let answer = call(&served.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::Unauthenticated, "unauthorized".to_owned(), None),
            "{path}"
        );
    }
}

/// Proves: a cookie beside a wrong bearer changes nothing: the call is refused as the wrong
/// bearer is, and beside the operator's token it is answered as the token's call.
#[tokio::test]
async fn a_cookie_beside_a_bearer_changes_neither_the_refusal_nor_the_answer() {
    let served = served("cookie-separation-beside");
    let path = "/hostkeeper.operator.v1.Hosts/List";
    let wrong = with_cookie(grpc_request(path, (), Some("not-a-token")));
    let answer = call(&served.coordinator, Surface::Control, &None, wrong).await;
    assert_eq!(
        answer,
        (Code::Unauthenticated, "unauthorized".to_owned(), None)
    );
    let right = with_cookie(grpc_request(path, (), Some(OPERATOR)));
    let (code, message, _) = call(&served.coordinator, Surface::Control, &None, right).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
}

/// Proves: an answer to a bearer call never sets a cookie, whether the call was answered or
/// refused, on the gRPC path, the JSON gateway and the frozen JSON routes: no `Set-Cookie` in
/// its headers or its trailers.
#[tokio::test]
async fn an_answer_to_a_bearer_call_never_sets_a_cookie() {
    let bearer = |token: &str| format!("Bearer {token}");
    let json = |method: &str, path: &str, token: &str, body: &str| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", bearer(token))
            .body(Full::new(Bytes::from(body.to_owned())))
            .expect("a request")
    };
    let served = served("cookie-separation-said");
    for token in [OPERATOR, "not-a-token"] {
        let requests = [
            grpc_request("/hostkeeper.operator.v1.Hosts/List", (), Some(token)),
            json("POST", "/hostkeeper.operator.v1.Hosts/List", token, "{}"),
            json("POST", "/v1/operator", token, r#"{"action":"nothing"}"#),
            json("GET", "/v1/status", token, ""),
        ];
        for request in requests {
            let what = format!("{} {} with {token}", request.method(), request.uri());
            let said = everything_said(&served.coordinator, request).await;
            assert!(!sets_a_cookie(&said), "{what}: {said:?}");
        }
    }
}
