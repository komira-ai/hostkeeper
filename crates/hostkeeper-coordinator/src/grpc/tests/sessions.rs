//! The session cookie of a signed-in browser on the JSON gateway: who the call is made as, what
//! stops a forged one, and that nothing but the gateway reads the cookie. Each test signs in
//! through the router, as a browser does, and calls through it.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};
use tonic::Code;

use super::credentials::operator_paths;
use super::support::{call, grpc_with};
use crate::grpc::{Admitted, Transport, admit_via};
use crate::http::Surface;
use crate::login_support::{HOST, PASSWORD, Signed, World, counting, settings};
use crate::operator::Caller;
use crate::testutil::{OPERATOR, Rig, ci_token};

const LIST: &str = "/hostkeeper.operator.v1.Hosts/List";
const INCLUDE: &str = "/hostkeeper.operator.v1.Hosts/Include";
const EXCLUDE: &str = "/hostkeeper.operator.v1.Hosts/Exclude";
const STATUS: &str = "/hostkeeper.operator.v1.Releases/Status";
const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";
const INCLUDE_BODY: &str = r#"{"service":"kbf","nodeId":"h1"}"#;
const EXCLUDE_BODY: &str = r#"{"service":"kbf","nodeId":"h1","reason":"gone"}"#;

fn refusal(code: &str, reason: &str) -> Value {
    json!({ "code": code, "reason": reason })
}

fn with_bearer(mut request: Request<Full<Bytes>>, token: &str) -> Request<Full<Bytes>> {
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("a value"),
    );
    request
}

/// Proves: a gateway call with the cookie of a live session and its CSRF header is made as the
/// signed-in user, on every kind of operator method the router has: the audit holds the user's
/// name as `by` for what it changed, the status and the lists answer, and the methods CI may also
/// call (`Converge`) take the session first.
#[tokio::test]
async fn a_gateway_call_with_a_session_is_made_as_the_user() {
    let world = World::new("gateway-session");
    let alice = world.sign_in("alice", PASSWORD).await;
    for (path, body) in [
        (LIST, "{}"),
        (STATUS, "{}"),
        (INCLUDE, INCLUDE_BODY),
        (EXCLUDE, EXCLUDE_BODY),
        (CONVERGE, r#"{"service":"kbf"}"#),
    ] {
        let sent = world.send(alice.post(path, body)).await;
        assert_ne!(sent.status, StatusCode::UNAUTHORIZED, "{path}: {sent:?}");
        assert_ne!(sent.status, StatusCode::FORBIDDEN, "{path}: {sent:?}");
        if path != CONVERGE {
            assert_eq!(sent.status, StatusCode::OK, "{path}: {sent:?}");
        }
        assert!(
            sent.header("set-cookie").is_none(),
            "{path}: an answer to a call sets no cookie"
        );
    }
    let said: Vec<(String, String)> = world
        .said()
        .into_iter()
        .map(|(by, action, _, _)| (by, action))
        .collect();
    assert_eq!(
        said,
        [
            ("alice".to_owned(), "login".to_owned()),
            ("alice".to_owned(), "include".to_owned()),
            ("alice".to_owned(), "exclude".to_owned()),
        ]
    );
}

/// Proves: a gateway call with a live cookie that fails a check of a session's call is
/// `PERMISSION_DENIED` `csrf` and changes nothing: no CSRF header, a wrong one, another `Origin`,
/// `Sec-Fetch-Site: cross-site`, and a body not said to be JSON.
#[tokio::test]
async fn a_forged_session_call_is_refused_and_changes_nothing() {
    let world = World::new("gateway-csrf");
    let alice = world.sign_in("alice", PASSWORD).await;
    let mutations: [(&str, Option<&str>); 6] = [
        ("x-hk-csrf", None),
        ("x-hk-csrf", Some("wrong")),
        ("origin", Some("https://evil.test")),
        ("origin", Some("null")),
        ("sec-fetch-site", Some("cross-site")),
        ("content-type", Some("text/plain")),
    ];
    for (name, value) in mutations {
        let mut request = alice.post(INCLUDE, INCLUDE_BODY);
        request.headers_mut().remove(name);
        if let Some(value) = value {
            request
                .headers_mut()
                .insert(name, value.parse().expect("a value"));
        }
        let sent = world.send(request).await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{name}: {value:?}");
        assert_eq!(sent.json(), refusal("PERMISSION_DENIED", "csrf"));
    }
    assert_eq!(world.said().len(), 1, "only the login is on the audit");
}

/// Proves: a call with an `Authorization` header is the bearer's alone: the cookie of another
/// user beside the operator's token is made as the token's name, and beside a wrong token the call
/// is refused as a wrong token is, whatever the cookie holds.
#[tokio::test]
async fn a_bearer_beside_a_cookie_is_the_bearers_call() {
    let world = World::new("gateway-bearer");
    let alice = world.sign_in("alice", PASSWORD).await;
    let sent = world
        .send(with_bearer(alice.post(INCLUDE, INCLUDE_BODY), OPERATOR))
        .await;
    assert_eq!(sent.status, StatusCode::OK, "{sent:?}");
    assert_eq!(world.said()[1].0, "operator");
    let mut wrong = with_bearer(alice.post(INCLUDE, INCLUDE_BODY), "not-a-token");
    wrong.headers_mut().remove("x-hk-csrf");
    let sent = world.send(wrong).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), refusal("UNAUTHENTICATED", "unauthorized"));
    assert_eq!(world.said().len(), 2);
    // CI's token beside the cookie is CI's: refused an operator method as it always is.
    let sent = world
        .send(with_bearer(alice.post(INCLUDE, INCLUDE_BODY), &ci_token()))
        .await;
    assert_eq!(sent.json(), refusal("PERMISSION_DENIED", "forbidden"));
}

/// Proves: a session that must choose a new password calls no operator method: 403
/// `password_change_required`, and it can call them once it has.
#[tokio::test]
async fn a_session_that_must_change_its_password_calls_nothing() {
    let world = World::new("gateway-must-change");
    let newbie = world.sign_in("newbie", PASSWORD).await;
    let sent = world.send(newbie.post(LIST, "{}")).await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN);
    assert_eq!(
        sent.json(),
        refusal("PERMISSION_DENIED", "password_change_required")
    );
    let body = json!({ "current": PASSWORD, "new": "a brand new password" }).to_string();
    assert_eq!(
        world.send(newbie.post("/v1/password", &body)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        world.send(newbie.post(LIST, "{}")).await.status,
        StatusCode::OK
    );
}

/// Proves: a cookie that names no live session is no credential: an id nobody was given, a session
/// past its idle deadline and one that signed out are each 401 `unauthorized`, as a call with no
/// credential is.
#[tokio::test]
async fn a_cookie_that_names_no_live_session_is_no_credential() {
    let world = World::new("gateway-dead");
    let unknown = Signed {
        id: "y".repeat(43),
        csrf: "z".repeat(43),
    };
    let idle = world.sign_in("alice", PASSWORD).await;
    let gone = world.sign_in("bob", PASSWORD).await;
    assert_eq!(
        world.send(gone.post("/v1/logout", "{}")).await.status,
        StatusCode::OK
    );
    world.at(crate::login_support::START + 1800);
    for session in [&unknown, &idle, &gone] {
        let sent = world.send(session.post(LIST, "{}")).await;
        assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
        assert_eq!(sent.json(), refusal("UNAUTHENTICATED", "unauthorized"));
    }
}

/// Proves: a coordinator with no sign-in reads no cookie: a call with a cookie and no bearer is
/// refused as one with no credential.
#[tokio::test]
async fn a_coordinator_without_a_sign_in_reads_no_cookie() {
    let bare = World::build("gateway-bare", settings(), counting(), |_, rig| {
        Rig::start(&rig.dir, &rig.clock)
    });
    let session = Signed {
        id: "y".repeat(43),
        csrf: "z".repeat(43),
    };
    let sent = bare.send(session.post(LIST, "{}")).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
}

/// Proves: only the gateway reads the cookie: a gRPC call over HTTP/2 with the cookie, the CSRF
/// header and the origin of a live session, and no `Authorization` header, is refused as a call
/// with no credential on every method of the operator API, and does nothing.
#[tokio::test]
async fn a_grpc_call_with_a_live_session_cookie_is_refused() {
    let world = World::new("grpc-session");
    let alice = world.sign_in("alice", PASSWORD).await;
    for path in operator_paths() {
        let mut request = grpc_with(path, Full::new(Bytes::from_static(&[0, 0, 0, 0, 0])), None);
        let from_page = alice.post(path, "");
        for name in ["cookie", "x-hk-csrf", "origin", "host", "sec-fetch-site"] {
            request
                .headers_mut()
                .insert(name, from_page.headers()[name].clone());
        }
        let answer = call(&world.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::Unauthenticated, "unauthorized".to_owned(), None),
            "{path}"
        );
    }
    assert_eq!(world.said().len(), 1, "only the login is on the audit");
}

/// The headers of a call a page of the listener makes with the session of `signed`.
fn headers_of(signed: &Signed) -> HeaderMap {
    signed.post(LIST, "{}").headers().clone()
}

/// Proves: a method of the session-only list refuses a bearer `PERMISSION_DENIED`
/// `session_required` (the operator's token and CI's alike) and serves a session; a method not on
/// the list serves the bearer.
#[tokio::test]
async fn a_session_only_method_refuses_a_bearer_and_serves_a_session() {
    let world = World::new("gateway-session-only");
    let alice = world.sign_in("alice", PASSWORD).await;
    let session = headers_of(&alice);
    let bearer = |token: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {token}").parse().expect("a value"),
        );
        headers
    };
    let gateway = Transport::Gateway {
        authority: Some(HOST),
    };
    let admit = |headers: HeaderMap, path: &'static str, only: &'static [&'static str]| {
        let coordinator = world.coordinator.clone();
        async move {
            admit_via(
                &coordinator,
                Surface::Control,
                path,
                &None,
                &headers,
                gateway,
                only,
            )
            .await
        }
    };
    let only: &'static [&'static str] = &[LIST];
    let refused = admit(bearer(OPERATOR), LIST, only)
        .await
        .expect_err("refused");
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::PermissionDenied, "session_required")
    );
    let served = admit(session, LIST, only).await.expect("admitted");
    assert!(matches!(
        served,
        Admitted::Operator { caller: Caller::Operator(ref name) } if name.as_str() == "alice"
    ));
    assert!(admit(bearer(OPERATOR), INCLUDE, only).await.is_ok());
    let ci = admit(bearer(&ci_token()), CONVERGE, &[CONVERGE])
        .await
        .expect_err("refused");
    assert_eq!(ci.message(), "session_required");
}
