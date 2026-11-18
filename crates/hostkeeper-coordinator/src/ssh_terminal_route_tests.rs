//! `GET /v1/ssh/terminal`: each refusal before the upgrade, with its exact status and body, and the
//! upgrade itself, over a real HTTP/1.1 connection of hyper and over the listener's own
//! connection (TLS, HTTP/1.1) in memory.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, StatusCode, Version};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::Shared;
use crate::http::{Surface, handle};
use crate::listen::tests::{connector, tls_material};
use crate::listen::{CLIENT_PATIENCE, accept_each, connection};
use crate::login::Login;
use crate::login_support::{Seconds, counting, settings};
use crate::service::Coordinator;
use crate::ssh_host_tests::{Behavior, Browser, Kind, Sent, Site, host_fingerprint};
use crate::testutil::Rig as ScratchRig;
use crate::websocket::tests::client_frame;

const PATIENCE: Duration = Duration::from_secs(10);

/// The key of the RFC's sample handshake and the accept key it gets.
const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

/// The headers of a good request of a browser at `https://hk.test` with the session cookie `id`.
fn good(id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("host", "hk.test".to_owned()),
        ("origin", "https://hk.test".to_owned()),
        ("cookie", format!("__Host-hk_session={id}")),
        ("upgrade", "websocket".to_owned()),
        ("connection", "Upgrade".to_owned()),
        ("sec-websocket-version", "13".to_owned()),
        ("sec-websocket-key", KEY.to_owned()),
    ]
}

/// A request for `target` with `headers`.
fn request(
    method: Method,
    target: &str,
    headers: &[(&'static str, String)],
) -> Request<Full<Bytes>> {
    let mut builder = Request::builder().method(method).uri(target);
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    builder.body(Full::new(Bytes::new())).expect("a request")
}

/// The headers without the ones named `without`, and with `extra`.
fn changed(
    id: &str,
    without: &[&str],
    extra: &[(&'static str, &str)],
) -> Vec<(&'static str, String)> {
    let mut headers: Vec<_> = good(id)
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

/// The status and body of the answer to `request`.
async fn answer(coordinator: &Coordinator, request: Request<Full<Bytes>>) -> (StatusCode, String) {
    let response = handle(coordinator, Surface::Control, &None, request, PATIENCE).await;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, String::from_utf8(body.to_vec()).expect("UTF-8"))
}

fn error(code: &str) -> String {
    format!(r#"{{"error":"{code}"}}"#)
}

/// A request that is refused: what it is, its headers and target, and the answer it gets.
type Case = (
    &'static str,
    Vec<(&'static str, String)>,
    &'static str,
    StatusCode,
    String,
);

const TARGET: &str = "/v1/ssh/terminal?host=node-07";

/// Proves: each refusal before the upgrade is a plain answer with its status and its exact body,
/// in the order of the checks: a bearer beside the cookie, or a bearer alone, is 403 `forbidden`;
/// no cookie, an unknown one is 401 `sign_in_required`; a missing `Origin`, a foreign one, a
/// scheme that is not https and a `Sec-Fetch-Site` that is not `same-origin` is 403 `csrf`; a
/// session that must change its password is 403 `password_change_required`; a user whose name is
/// no operator's is 403 `forbidden`; a request that is no HTTP/1.1 WebSocket opening (each header
/// missing or wrong, and HTTP/2), or names no host, is 400 `bad_request`; a coordinator with no
/// SSH CA is 404 `not_configured`; a request that no listener handed its coordinator to is 500
/// `internal`. Nothing is signed or opened by any of them.
///
/// Catches: an upgrade accepted for a bearer, a page of another origin, a session that must
/// change its password, or a request that is no WebSocket; and a body that differs from the
/// closed set of codes.
#[tokio::test]
async fn each_refusal_before_the_upgrade_has_its_status_and_its_body() {
    let site = Site::new("route-refusals", Kind::Ssh(Behavior::good()), None).await;
    let coordinator = &site.rig.coordinator;
    let id = site.session.0.clone();
    let sessions = &coordinator.login().expect("a sign-in").sessions;
    let (must_change, _) = sessions
        .create("operator", true, Some(1))
        .expect("a session");
    let (stranger, _) = sessions
        .create("Not A Name", false, None)
        .expect("a session");
    let cases: Vec<Case> = vec![
        (
            "a bearer beside the cookie",
            changed(&id, &[], &[("authorization", "Bearer x")]),
            TARGET,
            StatusCode::FORBIDDEN,
            error("forbidden"),
        ),
        (
            "no cookie",
            changed(&id, &["cookie"], &[]),
            TARGET,
            StatusCode::UNAUTHORIZED,
            error("sign_in_required"),
        ),
        (
            "an unknown cookie",
            changed(
                "unknown",
                &["cookie"],
                &[("cookie", "__Host-hk_session=unknown")],
            ),
            TARGET,
            StatusCode::UNAUTHORIZED,
            error("sign_in_required"),
        ),
        (
            "no origin",
            changed(&id, &["origin"], &[]),
            TARGET,
            StatusCode::FORBIDDEN,
            error("csrf"),
        ),
        (
            "a foreign origin",
            changed(&id, &["origin"], &[("origin", "https://evil.test")]),
            TARGET,
            StatusCode::FORBIDDEN,
            error("csrf"),
        ),
        (
            "an origin over http",
            changed(&id, &["origin"], &[("origin", "http://hk.test")]),
            TARGET,
            StatusCode::FORBIDDEN,
            error("csrf"),
        ),
        (
            "a cross-site fetch",
            changed(&id, &[], &[("sec-fetch-site", "cross-site")]),
            TARGET,
            StatusCode::FORBIDDEN,
            error("csrf"),
        ),
        (
            "a session that must change its password",
            changed(
                &id,
                &["cookie"],
                &[(
                    "cookie",
                    &format!("__Host-hk_session={}", must_change.as_str()),
                )],
            ),
            TARGET,
            StatusCode::FORBIDDEN,
            error("password_change_required"),
        ),
        (
            "a user whose name is no operator's",
            changed(
                &id,
                &["cookie"],
                &[(
                    "cookie",
                    &format!("__Host-hk_session={}", stranger.as_str()),
                )],
            ),
            TARGET,
            StatusCode::FORBIDDEN,
            error("forbidden"),
        ),
        (
            "no Upgrade",
            changed(&id, &["upgrade"], &[]),
            TARGET,
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "no Connection",
            changed(&id, &["connection"], &[]),
            TARGET,
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "version 12",
            changed(
                &id,
                &["sec-websocket-version"],
                &[("sec-websocket-version", "12")],
            ),
            TARGET,
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "no key",
            changed(&id, &["sec-websocket-key"], &[]),
            TARGET,
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "no host in the query",
            good(&id),
            "/v1/ssh/terminal",
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "an empty host",
            good(&id),
            "/v1/ssh/terminal?host=",
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "another parameter only",
            good(&id),
            "/v1/ssh/terminal?service=web",
            StatusCode::BAD_REQUEST,
            error("bad_request"),
        ),
        (
            "no listener to hand the coordinator over",
            good(&id),
            "/v1/ssh/terminal?service=x&host=node-07",
            StatusCode::INTERNAL_SERVER_ERROR,
            error("internal"),
        ),
    ];
    for (label, headers, target, status, body) in cases {
        let got = answer(coordinator, request(Method::GET, target, &headers)).await;
        assert_eq!(got, (status, body), "{label}");
    }
    let mut http2 = request(Method::GET, TARGET, &good(&id));
    *http2.version_mut() = Version::HTTP_2;
    assert_eq!(
        answer(coordinator, http2).await,
        (StatusCode::BAD_REQUEST, error("bad_request")),
        "HTTP/2"
    );
    assert!(site.rig.ledger.certs().is_empty());
    assert!(site.rig.ledger.audit().is_empty());
}

/// Proves: a coordinator that holds no SSH CA answers 404 `not_configured` to a request that is
/// otherwise right, and the route answers a method other than `GET` with 405, the heartbeat
/// surface with 404 (it never serves the route), and a coordinator with no sign-in with 404.
///
/// Catches: a terminal offered where there is nothing to reach, and a route on the heartbeat
/// listener.
#[tokio::test]
async fn the_route_is_not_served_without_ssh_a_sign_in_or_the_private_surface() {
    let rig = ScratchRig::new("route-unconfigured");
    let clock = Arc::clone(&rig.clock);
    let login = Login::new(settings(), Arc::new(Seconds(clock)), counting()).expect("a sign-in");
    let coordinator = rig.coordinator.with_login(login);
    let sessions = &coordinator.login().expect("a sign-in").sessions;
    let (id, _) = sessions.create("operator", false, None).expect("a session");
    let asked = request(Method::GET, TARGET, &good(id.as_str()));
    assert_eq!(
        answer(&coordinator, asked).await,
        (StatusCode::NOT_FOUND, error("not_configured"))
    );
    let posted = request(Method::POST, TARGET, &good(id.as_str()));
    assert_eq!(
        answer(&coordinator, posted).await,
        (StatusCode::METHOD_NOT_ALLOWED, error("method_not_allowed"))
    );
    let asked = request(Method::GET, TARGET, &good(id.as_str()));
    let response = handle(&coordinator, Surface::Heartbeat, &None, asked, PATIENCE).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let without = ScratchRig::new("route-no-signin");
    let asked = request(Method::GET, TARGET, &good("anything"));
    assert_eq!(
        answer(&without.coordinator, asked).await,
        (StatusCode::NOT_FOUND, error("not_found"))
    );
}

/// The head of the answer to an upgrade request written to `stream`, read up to the blank line
/// and no further.
async fn upgrade<S>(stream: &mut S, authority: &str, id: &str) -> String
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let asked = format!(
        "GET {TARGET} HTTP/1.1\r\nHost: {authority}\r\nOrigin: https://{authority}\r\n\
         Cookie: __Host-hk_session={id}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {KEY}\r\n\r\n"
    );
    stream.write_all(asked.as_bytes()).await.expect("sent");
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .expect("the head of the answer");
        head.push(byte[0]);
    }
    String::from_utf8(head).expect("UTF-8")
}

/// Whether `head` has the header `name: value`, in any case of the name.
fn has_header(head: &str, name: &str, value: &str) -> bool {
    head.lines().any(|line| {
        line.split_once(": ")
            .is_some_and(|(found, text)| found.eq_ignore_ascii_case(name) && text == value)
    })
}

/// Proves: over a real HTTP/1.1 connection of hyper a good request is answered `101 Switching
/// Protocols` with `Upgrade: websocket`, `Connection: Upgrade` and the accept key of the RFC's
/// sample, and the connection then speaks WebSocket to a terminal: the first message opens the
/// shell on the host, what is typed is echoed, and the shell's end is the last message.
///
/// Catches: an upgrade whose response hyper does not hand the connection over for, and a terminal
/// task that is not given the coordinator that outlives the request.
#[tokio::test]
async fn a_good_request_is_upgraded_and_the_connection_then_carries_the_terminal() {
    let site = Site::new(
        "route-upgrade",
        Kind::Ssh(Behavior::good()),
        Some(vec![host_fingerprint()]),
    )
    .await;
    let coordinator = Arc::clone(&site.rig.coordinator);
    let (mut client, server) = tokio::io::duplex(1 << 20);
    let served = Arc::clone(&coordinator);
    let serving = tokio::spawn(async move {
        let service = service_fn(move |mut asked: Request<hyper::body::Incoming>| {
            let coordinator = Arc::clone(&served);
            asked
                .extensions_mut()
                .insert(Shared(Arc::clone(&coordinator)));
            async move {
                Ok::<_, Infallible>(
                    handle(&coordinator, Surface::Control, &None, asked, PATIENCE).await,
                )
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server), service)
            .with_upgrades()
            .await;
    });
    let head = upgrade(&mut client, "hk.test", &site.session.0).await;
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{head}"
    );
    assert!(has_header(&head, "upgrade", "websocket"), "{head}");
    assert!(has_header(&head, "connection", "Upgrade"), "{head}");
    assert!(has_header(&head, "sec-websocket-accept", ACCEPT), "{head}");
    let mut browser = Browser::over(client, serving);
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    assert_eq!(browser.status("opened").await.as_deref(), Some("root"));
    browser.binary(b"hello\r").await;
    browser.output_until(b"hello\r").await;
    browser.binary(b"exit\r").await;
    browser.closed("exit").await;
}

/// Proves: the listener's own HTTP/1.1 connection, over TLS, serves the upgrade: a client that
/// negotiated `http/1.1` is answered `101` and then speaks WebSocket (here a first message with a
/// wrong token, which ends the socket `bad_request`).
///
/// Catches: a listener that serves HTTP/1.1 without upgrades, which would answer `101` and then
/// drop the connection.
#[tokio::test]
async fn the_listener_serves_the_upgrade_on_an_http1_connection() {
    let site = Site::new("route-listener", Kind::Ssh(Behavior::good()), None).await;
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(1 << 20);
    let serving = tokio::spawn(connection(
        server,
        source,
        Surface::Control,
        Arc::clone(&site.rig.coordinator),
        CLIENT_PATIENCE,
    ));
    let mut tls = connector(&ca, &[b"http/1.1"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    let head = upgrade(&mut tls, "localhost", &site.session.0).await;
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
        "{head}"
    );
    assert!(has_header(&head, "sec-websocket-accept", ACCEPT), "{head}");
    let mut browser = Browser::over(tls, serving);
    let wrong = site.hello(80, 24).replace(&site.csrf, "not-the-token");
    browser.text(&wrong).await;
    browser.closed("bad_request").await;
}

/// Proves: an upgraded connection keeps its slot of the listener while its terminal lives: with a
/// listener whose slots are two (one for the connection, one the accept loop holds for the next),
/// after the `101` and a ping the terminal answers, the connection still holds its slot, and the
/// slot is free once the terminal has ended.
///
/// Catches: a listener that frees the slot of a connection at the upgrade, so that the sockets of
/// terminals are not counted against its limit.
#[tokio::test]
async fn an_upgraded_connection_holds_its_slot_until_the_terminal_ends() {
    let site = Site::new("route-slot", Kind::Ssh(Behavior::good()), None).await;
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(1 << 20);
    let slots = Arc::new(tokio::sync::Semaphore::new(2));
    let mut connections = Some(server);
    let accept = move || {
        let next = connections.take();
        async move {
            match next {
                Some(stream) => Ok(stream),
                None => std::future::pending().await,
            }
        }
    };
    let coordinator = Arc::clone(&site.rig.coordinator);
    let accepting = tokio::spawn(accept_each(accept, Arc::clone(&slots), move |stream| {
        connection(
            stream,
            source.clone(),
            Surface::Control,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
        )
    }));
    let mut tls = connector(&ca, &[b"http/1.1"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    let head = upgrade(&mut tls, "localhost", &site.session.0).await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let mut browser = Browser::over(tls, tokio::spawn(async {}));
    browser.raw(&client_frame(9, b"alive")).await;
    assert_eq!(browser.next().await, Sent::Pong(b"alive".to_vec()));
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(slots.available_permits(), 0);
    let wrong = site.hello(80, 24).replace(&site.csrf, "not-the-token");
    browser.text(&wrong).await;
    browser.closed("bad_request").await;
    // The slot comes back once the connection is over, and not before.
    let _freed = slots.acquire().await.expect("a slot");
    accepting.abort();
}
