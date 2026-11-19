//! `GET /console/v1/stream/<session>` before the upgrade: each refusal with its status and exact
//! body, carrying neither the token nor the bearer's subprotocol; the `101` and the subprotocol
//! it selects; one attach per session, by its opener's token, from the listener's own origin.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode, Version};

use super::attach_support::{
    ALICE, BOB, CAROL, Live, browser_headers, encoded, good_headers, send, upgrade,
};
use super::support::{MAC, NODE, caller};
use crate::http::{Surface, handle};
use crate::testutil::Log;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

const PATIENCE: Duration = Duration::from_secs(10);

/// A refused request: what it is, its headers, the session its path names, and its answer's
/// status and code.
type Case<'a> = (
    &'a str,
    Vec<(&'static str, String)>,
    &'a str,
    StatusCode,
    &'a str,
);

fn error(code: &str) -> String {
    format!(r#"{{"error":"{code}"}}"#)
}

/// `good_headers(token)` without the headers named `without`, and with `extra`.
fn changed(
    token: &str,
    without: &[&str],
    extra: &[(&'static str, &str)],
) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = good_headers(token)
        .into_iter()
        .filter(|(name, _)| !without.contains(name))
        .collect();
    headers.extend(
        extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned())),
    );
    headers
}

/// A request for the stream of `session` with `headers`, as no listener made it.
fn request(
    method: Method,
    session: &str,
    headers: &[(&'static str, String)],
) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/console/v1/stream/{session}"));
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    builder.body(Full::new(Bytes::new())).expect("a request")
}

/// The status, the headers as text, and the body of the answer to `request`.
async fn answer(live: &Live, request: Request<Full<Bytes>>) -> (StatusCode, String, String) {
    let response = handle(
        &live.coordinator,
        Surface::Control,
        &None,
        request,
        PATIENCE,
    )
    .await;
    let status = response.status();
    let headers = format!("{:?}", response.headers());
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (
        status,
        headers,
        String::from_utf8(body.to_vec()).expect("UTF-8"),
    )
}

/// Asks for each of `cases` over a served connection, and checks its answer: its status, its exact
/// body, no `Sec-WebSocket-Protocol` header, and neither the token nor its base64url anywhere.
async fn refused(live: &Live, cases: Vec<Case<'_>>) {
    for (label, headers, target, status, code) in cases {
        let mut stream = live.connection();
        let answered = send(&mut stream, &upgrade(target, &headers)).await;
        let line = format!("HTTP/1.1 {} ", status.as_u16());
        assert!(answered.starts_with(&line), "{label}: {answered}");
        assert!(
            answered.ends_with(&format!("\r\n\r\n{}", error(code))),
            "{label}: {answered}"
        );
        for secret in [ALICE.to_owned(), encoded(ALICE)] {
            assert!(!answered.contains(&secret), "{label}: {answered}");
        }
        let lower = answered.to_ascii_lowercase();
        assert!(
            !lower.contains("sec-websocket-protocol"),
            "{label}: {answered}"
        );
    }
}

/// Proves: each refusal before the upgrade is a plain answer with its status and exact body, in
/// the order of the checks; none carries a `Sec-WebSocket-Protocol` header, the token or its
/// base64url, and none attaches the session or writes an audit line: an opening that is not a
/// WebSocket's (each header missing or wrong, HTTP/2) is 400 `bad_request`; no bearer offer,
/// one longer than a bearer can be, not base64url, not UTF-8 or no operator's token, and a bearer in `Authorization` only, are 401 `unauthorized`; a token whose name the
/// operators file does not list is 403 `no_console_access`; an id that is not 64 lower-case hex
/// digits or no session's is 404 `no_session`; a request no listener handed the coordinator is
/// 500 `internal`.
///
/// Catches: an upgrade for a token that is not an operator's or for a session that is not open,
/// and a refusal that echoes the token or the bearer back.
#[tokio::test]
async fn a_rejected_upgrade_401_403_404_carries_neither_the_token_nor_the_bearer_subprotocol() {
    let live = Live::new("route-refusals");
    let session = live.open("alice", NODE);
    let lines = live.lines();
    let upper = "AB".repeat(32);
    let too_long = format!(
        "hostkeeper.console.v1, bearer.{}",
        "A".repeat(16 * 1024 + 1)
    );
    let nobodys = "ab".repeat(32);
    let not_utf8 = format!(
        "hostkeeper.console.v1, bearer.{}",
        encoded_bytes(&[0xff, 0xfe])
    );
    let cases: Vec<Case<'_>> = vec![
        (
            "no Upgrade",
            changed(ALICE, &["Upgrade"], &[]),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "no Connection",
            changed(ALICE, &["Connection"], &[]),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "version 12",
            changed(
                ALICE,
                &["Sec-WebSocket-Version"],
                &[("Sec-WebSocket-Version", "12")],
            ),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "no key",
            changed(ALICE, &["Sec-WebSocket-Key"], &[]),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "no bearer offer",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", "hostkeeper.console.v1")],
            ),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "a bearer in Authorization only",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[
                    ("Sec-WebSocket-Protocol", "hostkeeper.console.v1"),
                    ("Authorization", &format!("Bearer {ALICE}")),
                ],
            ),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "a bearer that is not base64url",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[(
                    "Sec-WebSocket-Protocol",
                    "hostkeeper.console.v1, bearer.***",
                )],
            ),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "a bearer that is not UTF-8",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", &not_utf8)],
            ),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "a bearer longer than any token",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", &too_long)],
            ),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "an unknown token",
            good_headers("no-such-token-0123456789abcdef"),
            &session,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "a name the operators file lacks",
            good_headers(CAROL),
            &session,
            StatusCode::FORBIDDEN,
            "no_console_access",
        ),
        (
            "an id in upper case",
            good_headers(ALICE),
            &upper,
            StatusCode::NOT_FOUND,
            "no_session",
        ),
        (
            "a short id",
            good_headers(ALICE),
            "abc",
            StatusCode::NOT_FOUND,
            "no_session",
        ),
        (
            "no session's id",
            good_headers(ALICE),
            &nobodys,
            StatusCode::NOT_FOUND,
            "no_session",
        ),
    ];
    refused(&live, cases).await;
    let (status, _, body) =
        answer(&live, request(Method::GET, &session, &good_headers(ALICE))).await;
    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, error("internal"))
    );
    let mut http2 = request(Method::GET, &session, &good_headers(ALICE));
    *http2.version_mut() = Version::HTTP_2;
    let (status, _, body) = answer(&live, http2).await;
    assert_eq!(
        (status, body),
        (StatusCode::BAD_REQUEST, error("bad_request"))
    );
    let posted = request(Method::POST, &session, &good_headers(ALICE));
    let (status, _, body) = answer(&live, posted).await;
    assert_eq!(
        (status, body),
        (StatusCode::METHOD_NOT_ALLOWED, error("method_not_allowed"))
    );
    let beat = request(Method::GET, &session, &good_headers(ALICE));
    let response = handle(&live.coordinator, Surface::Heartbeat, &None, beat, PATIENCE).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(live.lines(), lines);
    let listed = live
        .coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed");
    assert!(!listed.sessions[0].attached);
}

/// Proves: an opening that does not offer `hostkeeper.console.v1` (the bearer alone, or another
/// version beside it) is 400 `bad_request` before the bearer is looked at, with no subprotocol
/// selected and neither the token nor its base64url in the answer; the session stays unattached.
///
/// Catches: a stream opened for a client that speaks another protocol, and a refusal that echoes
/// the offered bearer.
#[tokio::test]
async fn an_offer_without_hostkeeper_console_v1_is_refused_and_echoes_nothing() {
    let live = Live::new("route-no-protocol");
    let session = live.open("alice", NODE);
    let bearer = format!("bearer.{}", encoded(ALICE));
    let other = format!("hostkeeper.console.v2, {bearer}");
    let cases: Vec<Case<'_>> = vec![
        (
            "the bearer alone",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", &bearer)],
            ),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            "another version of the protocol",
            changed(
                ALICE,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", &other)],
            ),
            &session,
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
    ];
    refused(&live, cases).await;
    assert!(
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(NODE)
            .is_some_and(|session| session.attachment.is_none())
    );
}

/// Proves: an attach whose `Origin` is missing, another site's, or this listener's over plain
/// http, or that a browser marks cross-site, is 403 `bad_origin` though its token is good; the
/// session stays unattached.
///
/// Catches: a page of another origin that holds a token (or any WebSocket client without the
/// listener's origin) attaching to a console.
#[tokio::test]
async fn an_origin_that_is_not_the_listeners_is_refused() {
    let live = Live::new("route-origin");
    let session = live.open("alice", NODE);
    let cases: Vec<Case<'_>> = vec![
        (
            "no origin",
            changed(ALICE, &["Origin"], &[]),
            &session,
            StatusCode::FORBIDDEN,
            "bad_origin",
        ),
        (
            "a foreign origin",
            changed(ALICE, &["Origin"], &[("Origin", "https://evil.test")]),
            &session,
            StatusCode::FORBIDDEN,
            "bad_origin",
        ),
        (
            "an origin over http",
            changed(ALICE, &["Origin"], &[("Origin", "http://hk.test")]),
            &session,
            StatusCode::FORBIDDEN,
            "bad_origin",
        ),
        (
            "a cross-site fetch",
            changed(ALICE, &[], &[("Sec-Fetch-Site", "cross-site")]),
            &session,
            StatusCode::FORBIDDEN,
            "bad_origin",
        ),
    ];
    refused(&live, cases).await;
    assert!(
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(NODE)
            .is_some_and(|session| session.attachment.is_none())
    );
}

/// `bytes` in base64url.
fn encoded_bytes(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Proves: the `101` answers the accept key and selects exactly `hostkeeper.console.v1`, whatever
/// order the client offers in and with other protocols offered beside, in one header or in two;
/// no header of it holds the token or its base64url.
///
/// Catches: the bearer's subprotocol echoed as the one selected (a browser takes the first it
/// offered), which would put the token in the answer.
#[tokio::test]
async fn the_101_selects_only_hostkeeper_console_v1() {
    for (n, offers) in [
        vec![format!("bearer.{}, hostkeeper.console.v1", encoded(ALICE))],
        vec![
            "chat".to_owned(),
            format!("bearer.{}", encoded(ALICE)),
            "hostkeeper.console.v1".to_owned(),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let live = Live::new(&format!("route-101-{n}"));
        let _mac = live.fixture.dials.connect();
        let session = live.open("alice", MAC);
        let mut headers = changed(ALICE, &["Sec-WebSocket-Protocol"], &[]);
        headers.extend(
            offers
                .iter()
                .map(|offer| ("Sec-WebSocket-Protocol", offer.clone())),
        );
        let mut stream = live.connection();
        let head = send(&mut stream, &upgrade(&session, &headers)).await;
        assert!(
            head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
            "{head}"
        );
        let lower = head.to_ascii_lowercase();
        assert!(
            lower.contains("\r\nsec-websocket-protocol: hostkeeper.console.v1\r\n"),
            "{head}"
        );
        assert_eq!(lower.matches("sec-websocket-protocol").count(), 1, "{head}");
        assert!(
            lower.contains("\r\nsec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo=\r\n"),
            "{head}"
        );
        assert!(lower.contains("\r\ncache-control: no-store\r\n"), "{head}");
        assert!(
            !head.contains(ALICE) && !head.contains(&encoded(ALICE)),
            "{head}"
        );
    }
}

/// Proves: the bearer is read from its subprotocol, base64url with its padding or without, and
/// attaches the session; the `101` holds neither the token nor its base64url.
///
/// Catches: a bearer read only in one spelling of base64url, which a client's encoder decides.
#[tokio::test]
async fn the_bearer_in_the_subprotocol_is_read_and_never_echoed() {
    for (n, padded) in [false, true].into_iter().enumerate() {
        let live = Live::new(&format!("route-bearer-{n}"));
        let _mac = live.fixture.dials.connect();
        let session = live.open("alice", MAC);
        // Alice's token is 38 bytes: its base64url has two bytes of padding.
        let bearer = if padded {
            format!("{}==", encoded(ALICE))
        } else {
            encoded(ALICE)
        };
        let headers = changed(
            ALICE,
            &["Sec-WebSocket-Protocol"],
            &[(
                "Sec-WebSocket-Protocol",
                &format!("hostkeeper.console.v1, bearer.{bearer}"),
            )],
        );
        let mut stream = live.connection();
        let head = send(&mut stream, &upgrade(&session, &headers)).await;
        assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
        assert!(
            !head.contains(ALICE) && !head.contains(&encoded(ALICE)),
            "{head}"
        );
    }
    assert_eq!(ALICE.len() % 3, 2);
}

/// Proves: the token is in no log line, nor its base64url: not of an attach, of a refusal
/// (another operator's token, a foreign origin), or of a stream that failed after its upgrade
/// (whose own line is there, so the log was read).
///
/// Catches: a token logged with the request's headers, which would put every operator's
/// credential in the coordinator's logs.
#[tokio::test]
async fn the_token_is_in_no_log_line() {
    let live = Live::new("route-log");
    // After the rig's own subscriber, which would otherwise take every line.
    let log = Log::default();
    let _guard = log.install();
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&session, &good_headers(ALICE))).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let refused = changed(ALICE, &["Origin"], &[("Origin", "https://evil.test")]);
    let mut other = live.connection();
    let head = send(&mut other, &upgrade(&session, &refused)).await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    let mut other = live.connection();
    let head = send(&mut other, &upgrade(&session, &good_headers(BOB))).await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    drop(stream);
    live.gone(MAC).await;
    live.fixture
        .tool
        .refuse(hostkeeper_console::ipmi::ShellError::Start);
    let serial = live.open("alice", NODE);
    let mut client = live.attach(&serial, ALICE).await;
    client.closed(4013, "bmc_failed").await;
    live.gone(NODE).await;
    let text = log.text();
    assert!(
        text.contains("a serial console was not activated"),
        "{text}"
    );
    for secret in [ALICE, BOB, &encoded(ALICE), &encoded(BOB)] {
        assert!(!text.contains(secret), "{text}");
    }
}

/// Proves: another operator's token on a session is 403 `forbidden` and leaves it unattached, so
/// its opener still attaches.
///
/// Catches: one operator's session taken by another's token, which would hand the console to a
/// name the audit's `console_open` does not show.
#[tokio::test]
async fn a_token_of_another_name_cannot_attach() {
    let live = Live::new("route-other-name");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&session, &good_headers(BOB))).await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert!(head.ends_with(&error("forbidden")), "{head}");
    let _client = live.attach(&session, ALICE).await;
}

/// Proves: once a stream is attached, a second attach of the same session, by the same token, is
/// 403 `already_attached`; after the session ended its id is 404 `no_session`; and a session whose
/// window to be attached has passed is 404 `no_session` before the sweep ends it.
///
/// Catches: a session id replayed for a second stream to the same device, and an attach after
/// `--console-attach-s`.
#[tokio::test]
async fn a_second_attach_is_refused() {
    let live = Live::new("route-second");
    let mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let _client = live.attach(&session, ALICE).await;
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&session, &good_headers(ALICE))).await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert!(head.ends_with(&error("already_attached")), "{head}");
    drop(mac);
    live.gone(MAC).await;
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&session, &good_headers(ALICE))).await;
    assert!(head.ends_with(&error("no_session")), "{head}");

    let late = live.open("alice", NODE);
    live.fixture.rig.at(2_030);
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&late, &good_headers(ALICE))).await;
    assert!(head.starts_with("HTTP/1.1 404 "), "{head}");
    assert!(head.ends_with(&error("no_session")), "{head}");
}

/// Proves: an attached stream holds its session's slot: with one session allowed, an open of
/// another host while a stream is attached is `too_many_sessions`.
///
/// Catches: an upgraded connection that leaves the session cap, so streams grow past
/// `--console-max-sessions`.
#[tokio::test]
async fn an_attached_stream_counts_against_the_session_cap() {
    let live = Live::with("route-cap", 1, Duration::from_secs(30));
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let _client = live.attach(&session, ALICE).await;
    let opened = live.coordinator.console_open(
        &caller("alice"),
        &proto::OpenConsoleRequest {
            host_id: NODE.to_owned(),
            reason: "a second one".to_owned(),
        },
    );
    assert_eq!(opened.err(), Some(proto::ConsoleRefusal::TooManySessions));
}

/// Proves: a `101` whose connection is never handed over (the request came in by no connection
/// that can upgrade) ends its session at once, with a close line `not_upgraded` by `coordinator`.
///
/// Catches: a session attached by a request whose stream never starts, holding the host until a
/// restart.
#[tokio::test]
async fn a_connection_that_is_not_handed_over_ends_its_session() {
    let live = Live::new("route-not-upgraded");
    let session = live.open("alice", MAC);
    let mut asked = request(Method::GET, &session, &good_headers(ALICE));
    asked
        .extensions_mut()
        .insert(crate::ssh_terminal_route::Shared(std::sync::Arc::clone(
            &live.coordinator,
        )));
    let (status, _, _) = answer(&live, asked).await;
    assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
    live.gone(MAC).await;
    let (by, action, ..) = live.lines().pop().expect("a line");
    assert_eq!(
        (by.as_str(), action.as_str()),
        ("coordinator", "console_close")
    );
    assert!(live.last_reason().contains("cause=not_upgraded;"));
    assert_eq!(live.fixture.dials.dialled(), Vec::<String>::new());
}

/// Asks for the stream of `session` over a served connection with `headers`: the answer's head
/// and body as text.
async fn asked(live: &Live, session: &str, headers: &[(&'static str, String)]) -> String {
    let mut stream = live.connection();
    send(&mut stream, &upgrade(session, headers)).await
}

/// Proves: a browser session whose user opened the console session attaches with the session's
/// CSRF token as a `csrf.` subprotocol beside its cookie and no bearer: the `101` selects
/// `hostkeeper.console.v1` alone and carries neither the token nor the cookie's value, the
/// session shows attached, and a second attach by the same session is 403 `already_attached`.
///
/// Catches: a web app that cannot attach to the console session it opened, and a `101` that
/// echoes the CSRF token as the selected subprotocol.
#[tokio::test]
async fn a_browser_session_attaches_with_its_csrf_in_the_subprotocol() {
    let live = Live::signed_in("route-browser");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("alice", false);
    let offer = format!("csrf.{csrf}");
    let headers = browser_headers(&id, &[&offer]);
    let head = asked(&live, &session, &headers).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains("\r\nsec-websocket-protocol: hostkeeper.console.v1\r\n"),
        "{head}"
    );
    assert_eq!(lower.matches("sec-websocket-protocol").count(), 1, "{head}");
    for secret in [csrf.as_str(), id.as_str()] {
        assert!(!head.contains(secret), "{head}");
    }
    let listed = live
        .coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed");
    assert!(listed.sessions[0].attached);
    let again = asked(&live, &session, &headers).await;
    assert!(again.starts_with("HTTP/1.1 403 "), "{again}");
    assert!(again.ends_with(&error("already_attached")), "{again}");
}

/// Proves: a live browser session that offers no `csrf.` subprotocol, one that is another
/// session's token, one that is longer than any token, or an empty one, is 403 `csrf` with the
/// exact body, no `Sec-WebSocket-Protocol` header and neither token in the answer; the console
/// session stays unattached, so the right token then attaches. Only the first `csrf.` offer is
/// read: the right token after a wrong one is refused.
///
/// Catches: a stream attached without proof that the page holds the session's CSRF token, and a
/// refusal that echoes the offered token.
#[tokio::test]
async fn a_session_without_its_csrf_or_with_another_is_refused_403_csrf() {
    let live = Live::signed_in("route-csrf");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("alice", false);
    let (_, other) = live.browser("alice", false);
    let wrong = format!("csrf.{other}");
    let long = format!("csrf.{}", "A".repeat(4096));
    let right = format!("csrf.{csrf}");
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("no offer", vec![]),
        ("another session's token", vec![&wrong]),
        ("a token longer than any", vec![&long]),
        ("an empty token", vec!["csrf."]),
        ("the right token second", vec![&wrong, &right]),
    ];
    for (label, offers) in cases {
        let answered = asked(&live, &session, &browser_headers(&id, &offers)).await;
        assert!(answered.starts_with("HTTP/1.1 403 "), "{label}: {answered}");
        assert!(
            answered.ends_with(&format!("\r\n\r\n{}", error("csrf"))),
            "{label}: {answered}"
        );
        assert!(
            !answered
                .to_ascii_lowercase()
                .contains("sec-websocket-protocol"),
            "{label}: {answered}"
        );
        for secret in [csrf.as_str(), other.as_str(), id.as_str()] {
            assert!(!answered.contains(secret), "{label}: {answered}");
        }
    }
    let listed = live
        .coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed");
    assert!(!listed.sessions[0].attached);
    let head = asked(&live, &session, &browser_headers(&id, &[&right])).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
}

/// Proves: with no `bearer.` offer, a request with no cookie, a cookie that names no live
/// session, or a cookie while the coordinator has no sign-in, is 401 `unauthorized` however good
/// its `csrf.` offer.
///
/// Catches: an attach with no credential at all, and a coordinator with no sign-in that answers
/// anything else.
#[tokio::test]
async fn no_session_cookie_and_no_bearer_is_unauthorized() {
    let live = Live::signed_in("route-no-cookie");
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("alice", false);
    let offer = format!("csrf.{csrf}");
    let mut no_cookie = browser_headers(&id, &[&offer]);
    no_cookie.retain(|(name, _)| *name != "Cookie");
    let unknown = browser_headers("no-such-session", &[&offer]);
    for (label, headers) in [("no cookie", no_cookie), ("an unknown cookie", unknown)] {
        let answered = asked(&live, &session, &headers).await;
        assert!(answered.starts_with("HTTP/1.1 401 "), "{label}: {answered}");
        assert!(
            answered.ends_with(&error("unauthorized")),
            "{label}: {answered}"
        );
    }
    let plain = Live::new("route-no-sign-in");
    let session = plain.open("alice", MAC);
    let answered = asked(&plain, &session, &browser_headers(&id, &[&offer])).await;
    assert!(answered.starts_with("HTTP/1.1 401 "), "{answered}");
    assert!(answered.ends_with(&error("unauthorized")), "{answered}");
}

/// Proves: a browser session that must choose a new password first is 403
/// `password_change_required`, and one whose user is not a valid operator name is 403
/// `forbidden`, each with the right CSRF token; neither attaches the session.
///
/// Catches: a console attached from a session that may do nothing else yet, and a session user
/// that is no operator name taken as one.
#[tokio::test]
async fn a_session_that_owes_a_new_password_cannot_attach() {
    let live = Live::signed_in("route-must-change");
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("alice", true);
    let offer = format!("csrf.{csrf}");
    let answered = asked(&live, &session, &browser_headers(&id, &[&offer])).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    assert!(
        answered.ends_with(&error("password_change_required")),
        "{answered}"
    );
    let (id, csrf) = live.browser("not a name!", false);
    let offer = format!("csrf.{csrf}");
    let answered = asked(&live, &session, &browser_headers(&id, &[&offer])).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    assert!(answered.ends_with(&error("forbidden")), "{answered}");
    let listed = live
        .coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed");
    assert!(!listed.sessions[0].attached);
}

/// Proves: a browser session of another operator name than the session's opener is 403
/// `forbidden` and leaves the session unattached, so its opener's session still attaches.
///
/// Catches: one operator's console session taken by another's browser.
#[tokio::test]
async fn a_session_of_another_name_cannot_attach_to_the_session() {
    let live = Live::signed_in("route-browser-other");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("bob", false);
    let offer = format!("csrf.{csrf}");
    let answered = asked(&live, &session, &browser_headers(&id, &[&offer])).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    assert!(answered.ends_with(&error("forbidden")), "{answered}");
    let (id, csrf) = live.browser("alice", false);
    let offer = format!("csrf.{csrf}");
    let head = asked(&live, &session, &browser_headers(&id, &[&offer])).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
}

/// Proves: a request that offers a `bearer.` beside a live session's cookie and its CSRF token is
/// judged by the bearer alone: a bad bearer is 401 `unauthorized` although the session and its
/// CSRF token are good, another operator's bearer is 403 `forbidden` on a session of the cookie's
/// user, and the opener's bearer attaches, with the `101` selecting the protocol alone.
///
/// Catches: a cookie that overrides or rescues a bearer, which would let a request pick which
/// credential is judged.
#[tokio::test]
async fn a_bearer_offer_beside_a_cookie_is_judged_as_a_bearer() {
    let live = Live::signed_in("route-bearer-and-cookie");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let (id, csrf) = live.browser("alice", false);
    let csrf_offer = format!("csrf.{csrf}");
    let bad = format!("bearer.{}", encoded("no-such-token-0123456789abcdef"));
    let both = browser_headers(&id, &[&csrf_offer, &bad]);
    let answered = asked(&live, &session, &both).await;
    assert!(answered.starts_with("HTTP/1.1 401 "), "{answered}");
    assert!(answered.ends_with(&error("unauthorized")), "{answered}");
    let bob = format!("bearer.{}", encoded(BOB));
    let both = browser_headers(&id, &[&csrf_offer, &bob]);
    let answered = asked(&live, &session, &both).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    assert!(answered.ends_with(&error("forbidden")), "{answered}");
    let good = format!("bearer.{}", encoded(ALICE));
    let both = browser_headers(&id, &[&csrf_offer, &good]);
    let head = asked(&live, &session, &both).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let lower = head.to_ascii_lowercase();
    assert!(
        lower.contains("\r\nsec-websocket-protocol: hostkeeper.console.v1\r\n"),
        "{head}"
    );
    assert!(!head.contains(&csrf), "{head}");
}

/// Proves: the session's CSRF token and its cookie are in no log line: not of an attach, of a
/// refusal (a wrong token, a foreign origin), or of a stream that failed after its upgrade (whose
/// own line is there, so the log was read).
///
/// Catches: a CSRF token or session id logged with the request's headers.
#[tokio::test]
async fn the_csrf_is_in_no_log_line() {
    let live = Live::signed_in("route-csrf-log");
    let log = Log::default();
    let _guard = log.install();
    let session = live.open("alice", NODE);
    let (id, csrf) = live.browser("alice", false);
    let offer = format!("csrf.{csrf}");
    let wrong = "csrf.not-the-token-0123456789".to_owned();
    let answered = asked(&live, &session, &browser_headers(&id, &[&wrong])).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    let mut foreign = browser_headers(&id, &[&offer]);
    foreign.retain(|(name, _)| *name != "Origin");
    foreign.push(("Origin", "https://evil.test".to_owned()));
    let answered = asked(&live, &session, &foreign).await;
    assert!(answered.starts_with("HTTP/1.1 403 "), "{answered}");
    live.fixture
        .tool
        .refuse(hostkeeper_console::ipmi::ShellError::Start);
    let mut stream = live.connection();
    let head = send(
        &mut stream,
        &upgrade(&session, &browser_headers(&id, &[&offer])),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let mut client = super::attach_support::Client { stream };
    client.closed(4013, "bmc_failed").await;
    live.gone(NODE).await;
    let text = log.text();
    assert!(
        text.contains("a serial console was not activated"),
        "{text}"
    );
    for secret in [csrf.as_str(), id.as_str(), "not-the-token-0123456789"] {
        assert!(!text.contains(secret), "{text}");
    }
}
