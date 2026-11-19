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
use tokio::io::AsyncWriteExt as _;

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

/// `browser_headers(browser)` without the headers named `without`, and with `extra`.
fn browser_changed(
    browser: &(String, String),
    without: &[&str],
    extra: &[(&'static str, &str)],
) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = browser_headers(browser)
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

/// Asks with `headers` over a served connection: the head and body of the answer.
async fn asked(live: &Live, session: &str, headers: &[(&'static str, String)]) -> String {
    let mut stream = live.connection();
    send(&mut stream, &upgrade(session, headers)).await
}

/// Checks that `answered` is the refusal `status` with `code`, carrying no subprotocol and none of
/// the `secrets`.
fn assert_refused(label: &str, answered: &str, status: StatusCode, code: &str, secrets: &[&str]) {
    let line = format!("HTTP/1.1 {} ", status.as_u16());
    assert!(answered.starts_with(&line), "{label}: {answered}");
    assert!(
        answered.ends_with(&format!("\r\n\r\n{}", error(code))),
        "{label}: {answered}"
    );
    assert!(
        !answered
            .to_ascii_lowercase()
            .contains("sec-websocket-protocol"),
        "{label}: {answered}"
    );
    for secret in secrets {
        assert!(!answered.contains(secret), "{label}: {answered}");
    }
}

/// Proves: a signed-in browser (a session cookie of the session's opener, and its CSRF token as a
/// `csrf.` subprotocol, no bearer) attaches: the `101` selects only `hostkeeper.console.v1`,
/// carries neither the CSRF token nor the cookie, and the stream carries the device's bytes.
///
/// Catches: a console the web UI cannot attach to, and a CSRF token echoed as the selected
/// subprotocol.
#[tokio::test]
async fn a_signed_in_browser_attaches_with_its_session_and_csrf_offer() {
    let live = Live::signed_in("route-browser");
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let browser = live.browser("alice", false);
    let mut stream = live.connection();
    let head = send(&mut stream, &upgrade(&session, &browser_headers(&browser))).await;
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
        !head.contains(&browser.1) && !head.contains(&browser.0),
        "{head}"
    );
    let mut client = super::attach_support::Client { stream };
    mac.write_all(b"hi").await.expect("written");
    assert_eq!(client.output(2).await, b"hi");
}

/// Proves: a browser's session without a `csrf.` offer, with a different one (of the same length,
/// of another length, and longer than any token) or with the offer of another session is 403
/// `csrf` with no subprotocol and none of the secrets in the answer, and leaves the session
/// unattached, so the correct attach that follows is a `101`.
///
/// Catches: a cross-site page that rides the cookie to attach a console, and a refusal that
/// echoes the offered token.
#[tokio::test]
async fn a_browser_without_the_right_csrf_offer_is_refused_and_not_attached() {
    let live = Live::signed_in("route-csrf");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let browser = live.browser("alice", false);
    let other = live.browser("alice", false);
    let mut same_length = browser.1.clone();
    same_length.replace_range(..1, if browser.1.starts_with('A') { "B" } else { "A" });
    let too_long = format!("hostkeeper.console.v1, csrf.{}", "A".repeat(16 * 1024 + 1));
    let offers = [
        ("none", "hostkeeper.console.v1".to_owned()),
        (
            "another of the same length",
            format!("hostkeeper.console.v1, csrf.{same_length}"),
        ),
        (
            "a prefix of it",
            format!("hostkeeper.console.v1, csrf.{}", &browser.1[..10]),
        ),
        (
            "another session's",
            format!("hostkeeper.console.v1, csrf.{}", other.1),
        ),
        ("one too long", too_long),
    ];
    for (label, offer) in offers {
        let headers = browser_changed(
            &browser,
            &["Sec-WebSocket-Protocol"],
            &[("Sec-WebSocket-Protocol", &offer)],
        );
        let answered = asked(&live, &session, &headers).await;
        assert_refused(
            label,
            &answered,
            StatusCode::FORBIDDEN,
            "csrf",
            &[&browser.1, &browser.0, &other.1],
        );
    }
    assert!(
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(MAC)
            .is_some_and(|session| session.attachment.is_none())
    );
    let _client = live.attach_browser(&session, &browser).await;
}

/// Proves: a browser's session is held to the checks after its credential, in order: from another
/// origin it is 403 `bad_origin` (before its CSRF offer is looked at); one whose password must
/// change is 403 `password_change_required`; one of a user whose name is no token name is 403
/// `forbidden`; one of a user the operators file does not name is 403 `no_console_access`; one
/// of another user than the session's opener is 403 `forbidden`, leaving the session for its
/// opener. None carries a subprotocol or a secret.
///
/// Catches: a console attached from a foreign page, by a user that must change its password, or
/// by a user other than the one that opened it.
#[tokio::test]
async fn a_browser_session_is_held_to_origin_password_name_and_opener() {
    let live = Live::signed_in("route-browser-checks");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let alice = live.browser("alice", false);
    let must_change = live.browser("alice", true);
    let stranger = live.browser("Not A Name", false);
    let carol = live.browser("carol", false);
    let bob = live.browser("bob", false);
    let foreign = browser_changed(
        &alice,
        &["Origin", "Sec-WebSocket-Protocol"],
        &[
            ("Origin", "https://evil.test"),
            ("Sec-WebSocket-Protocol", "hostkeeper.console.v1"),
        ],
    );
    let cases = [
        (
            "another origin",
            foreign,
            StatusCode::FORBIDDEN,
            "bad_origin",
        ),
        (
            "a password to change",
            browser_headers(&must_change),
            StatusCode::FORBIDDEN,
            "password_change_required",
        ),
        (
            "a name that is no token name",
            browser_headers(&stranger),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            "a user the operators file lacks",
            browser_headers(&carol),
            StatusCode::FORBIDDEN,
            "no_console_access",
        ),
        (
            "another user than the opener",
            browser_headers(&bob),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
    ];
    for (label, headers, status, code) in cases {
        let answered = asked(&live, &session, &headers).await;
        assert_refused(
            label,
            &answered,
            status,
            code,
            &[&alice.0, &alice.1, &must_change.1, &bob.1, &carol.1],
        );
    }
    let _client = live.attach_browser(&session, &alice).await;
}

/// Proves: a `bearer.` offer is the credential and the cookie is not read: a bearer that is no
/// operator's token is 401 `unauthorized` though a live session's cookie and CSRF offer are
/// there; with no bearer, a cookie of no live session (unknown, or a session that was ended), no
/// cookie at all, and a cookie beside an `Authorization` header are 401 `unauthorized` too; and a
/// valid bearer beside a live session's cookie attaches as a token.
///
/// Catches: a session that rescues a bad token, and a bearer that needs the CSRF token.
#[tokio::test]
async fn a_bearer_offer_is_read_before_the_cookie_and_no_credential_is_401() {
    let live = Live::signed_in("route-browser-order");
    let _mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let browser = live.browser("alice", false);
    let ended = live.browser("alice", false);
    live.sign_out(&ended.0);
    let bogus = format!(
        "hostkeeper.console.v1, csrf.{}, bearer.{}",
        browser.1,
        encoded("no-such-token-0123456789abcdef")
    );
    let unknown = ("cookie", "__Host-hk_session=unknown".to_owned());
    let cases = [
        (
            "an invalid bearer beside a live session",
            browser_changed(
                &browser,
                &["Sec-WebSocket-Protocol"],
                &[("Sec-WebSocket-Protocol", &bogus)],
            ),
        ),
        ("an ended session", browser_headers(&ended)),
        (
            "an unknown cookie",
            browser_changed(&browser, &["Cookie"], &[("Cookie", &unknown.1)]),
        ),
        ("no cookie", browser_changed(&browser, &["Cookie"], &[])),
        (
            "an Authorization header beside the cookie",
            browser_changed(
                &browser,
                &[],
                &[("Authorization", &format!("Bearer {ALICE}"))],
            ),
        ),
    ];
    for (label, headers) in cases {
        let answered = asked(&live, &session, &headers).await;
        assert_refused(
            label,
            &answered,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[&browser.0, &browser.1],
        );
    }
    let with_both = browser_changed(
        &browser,
        &["Sec-WebSocket-Protocol"],
        &[(
            "Sec-WebSocket-Protocol",
            &format!("hostkeeper.console.v1, bearer.{}", encoded(ALICE)),
        )],
    );
    let head = asked(&live, &session, &with_both).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
}

/// Proves: on a coordinator with no sign-in a cookie is no credential: with or without a `csrf.`
/// offer, no bearer is 401 `unauthorized`.
///
/// Catches: a cookie read where no sign-in exists.
#[tokio::test]
async fn a_coordinator_with_no_sign_in_takes_no_cookie() {
    let live = Live::new("route-no-sign-in");
    let session = live.open("alice", MAC);
    let browser = ("some-session".to_owned(), "some-csrf".to_owned());
    let answered = asked(&live, &session, &browser_headers(&browser)).await;
    assert_refused(
        "no sign-in",
        &answered,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        &[],
    );
}
