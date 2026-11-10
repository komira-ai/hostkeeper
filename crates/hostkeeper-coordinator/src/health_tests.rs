//! The health listener's router and connections (`docs/design/two-programs.md`, section 2.7),
//! in process over the in-memory backend, whose health a test sets. Every time is the paused
//! clock's; nothing waits on the wall clock.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Method, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot};

use super::{HEALTH_PATH, Serving, Verdict, answer, connection, drain_on, serve, verdict};
use crate::marks_support::rig_on;
use crate::service::Coordinator;
use crate::store::{Fake, Health};
use crate::testutil::Rig;

/// The patience of the connection tests.
const PATIENCE: Duration = Duration::from_secs(10);

/// A coordinator on `fake`, past its quiet start, and a [`Serving`] already set.
fn serving_on(test: &str, fake: &Fake) -> (Rig, Serving) {
    let rig = rig_on(test, fake, "");
    let serving = Serving::default();
    serving.set();
    (rig, serving)
}

/// The status, the body and the two headers of the answer to `method` of `path`.
async fn asked(
    coordinator: &Coordinator,
    serving: &Serving,
    method: &Method,
    path: &str,
) -> (StatusCode, Bytes, String, String) {
    let response = answer(coordinator, serving, method, path);
    let status = response.status();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let (kind, cache) = (header("content-type"), header("cache-control"));
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a full body")
        .to_bytes();
    (status, body, kind, cache)
}

/// The status and the body of the answer to `GET /healthz`.
async fn get(coordinator: &Coordinator, serving: &Serving) -> (StatusCode, Bytes) {
    let (status, body, _, _) = asked(coordinator, serving, &Method::GET, HEALTH_PATH).await;
    (status, body)
}

/// Proves: a serving coordinator whose store takes writes answers `GET /healthz` with `200` and
/// the body `ok` exactly, as plain text that no cache keeps, and `HEAD /healthz` as a `GET` (the
/// connection leaves the body out); the answer changes nothing the fake holds (no commit, no audit line).
///
/// Catches: a healthy coordinator a probe reads as down, a body that says more than `ok` (a
/// version, a host, a count), an answer a cache in front of the probe could serve stale, and an
/// answer that writes.
#[tokio::test]
async fn a_serving_coordinator_on_a_store_that_takes_writes_is_200_ok() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-ok", &fake);
    let (commits, lines) = (fake.commits(), fake.audit());
    let (status, body, kind, cache) =
        asked(&rig.coordinator, &serving, &Method::GET, HEALTH_PATH).await;
    assert_eq!(
        (status, body, kind.as_str(), cache.as_str()),
        (
            StatusCode::OK,
            Bytes::from_static(b"ok"),
            "text/plain; charset=utf-8",
            "no-store"
        )
    );
    let (status, body, _, _) = asked(&rig.coordinator, &serving, &Method::HEAD, HEALTH_PATH).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, Bytes::from_static(b"ok")),
        "as a GET: the connection leaves the body out"
    );
    assert_eq!(verdict(&rig.coordinator, &serving), Verdict::Ok);
    assert_eq!((fake.commits(), fake.audit()), (commits, lines));
}

/// Proves: `503` with exactly one reason token as the body: `starting` before every listener is
/// bound, `store_unreachable` while the store's link is lost, `not_writer` once the store is
/// deposed; `starting` wins over either store state, a `HEAD` is answered as a `GET`, and
/// a link made again is `200 ok` again.
///
/// Catches: a coordinator that cannot write (link lost) or that another coordinator replaced
/// as the writer read as healthy; a reason token other than the three, or with more beside it;
/// a start read as a store failure; a recovery the probe never sees.
#[tokio::test]
async fn each_state_that_is_not_serving_is_503_with_its_one_reason_token() {
    let fake = Fake::new();
    let rig = rig_on("health-503", &fake, "");
    let serving = Serving::default();
    let unavailable = |token: &'static str| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Bytes::from_static(token.as_bytes()),
        )
    };
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        unavailable("starting")
    );
    fake.set_health(Health::Lost);
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        unavailable("starting"),
        "starting first"
    );
    fake.set_health(Health::Deposed { found: 7 });
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        unavailable("starting"),
        "starting first"
    );

    serving.set();
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        unavailable("not_writer")
    );
    fake.set_health(Health::Lost);
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        unavailable("store_unreachable")
    );
    let (status, body, _, _) = asked(&rig.coordinator, &serving, &Method::HEAD, HEALTH_PATH).await;
    assert_eq!(
        (status, body),
        unavailable("store_unreachable"),
        "as a GET: the connection leaves the body out"
    );

    fake.set_health(Health::Up);
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        (StatusCode::OK, Bytes::from_static(b"ok"))
    );
}

/// Proves: every verdict's status and token: `200 ok`, and `503` for `starting`, `not_writer`,
/// `store_unreachable` and `draining`.
///
/// Catches: a reason that answers `200`, or two verdicts that share a token.
#[test]
fn each_verdict_has_its_status_and_token() {
    let all = [
        (Verdict::Ok, StatusCode::OK, "ok"),
        (
            Verdict::Starting,
            StatusCode::SERVICE_UNAVAILABLE,
            "starting",
        ),
        (
            Verdict::NotWriter,
            StatusCode::SERVICE_UNAVAILABLE,
            "not_writer",
        ),
        (
            Verdict::StoreUnreachable,
            StatusCode::SERVICE_UNAVAILABLE,
            "store_unreachable",
        ),
        (
            Verdict::Draining,
            StatusCode::SERVICE_UNAVAILABLE,
            "draining",
        ),
    ];
    for (verdict, status, token) in all {
        assert_eq!((verdict.status(), verdict.token()), (status, token));
    }
}

/// Proves: once drained, `GET /healthz` is `503` with the body `draining` exactly, whatever
/// else holds: a coordinator still starting, one serving on a store that takes writes, one whose
/// store's link is lost and one deposed; a `HEAD` is answered as a `GET`; neither a link made
/// again nor a [`Serving::set`] after the drain (a start that finishes its binds late) brings
/// back `200 ok`; the answer changes nothing the fake holds. Other paths stay `404 not_found`.
///
/// Catches: a coordinator told to stop that a load balancer still reads as healthy and keeps
/// sending to; a store state or a late end of the start that wins over the drain; a reason
/// token other than `draining`, or with more beside it.
#[tokio::test]
async fn a_drained_coordinator_is_503_draining_whatever_else_holds() {
    let fake = Fake::new();
    let rig = rig_on("health-draining", &fake, "");
    let (commits, lines) = (fake.commits(), fake.audit());
    let draining = (
        StatusCode::SERVICE_UNAVAILABLE,
        Bytes::from_static(b"draining"),
    );

    let starting = Serving::default();
    starting.drain();
    assert_eq!(get(&rig.coordinator, &starting).await, draining, "starting");
    starting.set();
    assert_eq!(
        get(&rig.coordinator, &starting).await,
        draining,
        "a set after the drain"
    );

    let serving = Serving::default();
    serving.set();
    assert_eq!(
        get(&rig.coordinator, &serving).await,
        (StatusCode::OK, Bytes::from_static(b"ok"))
    );
    serving.drain();
    assert_eq!(verdict(&rig.coordinator, &serving), Verdict::Draining);
    assert_eq!(get(&rig.coordinator, &serving).await, draining, "serving");
    for (health, what) in [
        (Health::Lost, "the link lost"),
        (Health::Deposed { found: 7 }, "deposed"),
        (Health::Up, "the link made again"),
    ] {
        fake.set_health(health);
        assert_eq!(get(&rig.coordinator, &serving).await, draining, "{what}");
    }
    let (status, body, kind, cache) =
        asked(&rig.coordinator, &serving, &Method::HEAD, HEALTH_PATH).await;
    assert_eq!(
        (status, body, kind.as_str(), cache.as_str()),
        (
            draining.0,
            draining.1.clone(),
            "text/plain; charset=utf-8",
            "no-store"
        ),
        "as a GET: the connection leaves the body out"
    );
    let (status, body, _, _) = asked(&rig.coordinator, &serving, &Method::GET, "/v1/status").await;
    assert_eq!(
        (status, body),
        (StatusCode::NOT_FOUND, Bytes::from_static(b"not_found"))
    );
    assert_eq!((fake.commits(), fake.audit()), (commits, lines));
}

/// Proves: [`drain_on`] leaves the coordinator as it was while the stop has not come, drains it
/// once the stop resolves, and answers what the stop answered: a stop that succeeded and a stop
/// that failed (the signal handler could not be set up) both drain.
///
/// Catches: a drain that comes before the stop (a serving coordinator read as going away), a
/// stop that never drains, a failed stop that leaves the coordinator read as healthy while it
/// exits, and a stop whose outcome is lost.
#[tokio::test]
async fn the_stop_drains_and_answers_what_it_answered() {
    let serving = Serving::default();
    serving.set();
    let (stop, stopped) = oneshot::channel::<io::Result<()>>();
    let mut draining = std::pin::pin!(drain_on(
        async move { stopped.await.expect("sent") },
        serving.clone()
    ));
    assert!(futures::poll!(draining.as_mut()).is_pending());
    assert!(!serving.is_draining(), "drained before the stop");
    stop.send(Ok(())).expect("sent");
    draining.await.expect("the stop's outcome");
    assert!(serving.is_draining(), "the stop drains");

    let failed = Serving::default();
    let why = drain_on(
        async { Err::<(), _>(io::Error::other("no signal handler")) },
        failed.clone(),
    )
    .await
    .expect_err("the stop's failure");
    assert_eq!(why.to_string(), "no signal handler");
    assert!(failed.is_draining(), "a failed stop drains too");
}

/// Proves: over the listener, a probe answered `200 ok` before the stop, and a probe that
/// connected before the stop and sends its request after it, are told `200 ok` and
/// `503 draining`; a probe that connects after the stop is told `503 draining` too.
///
/// Catches: a verdict taken when a connection is accepted rather than when its request is
/// answered, so a probe that connected just before the stop reads a stopping coordinator as
/// healthy.
#[tokio::test]
async fn a_probe_answered_after_the_stop_is_told_draining() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-drain-listener", &fake);
    let (offer, offered) = mpsc::unbounded_channel::<DuplexStream>();
    let offered = Arc::new(tokio::sync::Mutex::new(offered));
    let accept = move || {
        let offered = Arc::clone(&offered);
        async move {
            offered
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
        }
    };
    let listening = tokio::spawn(serve(
        accept,
        Arc::new(rig.coordinator),
        serving.clone(),
        PATIENCE,
        4,
    ));
    let head = "GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n";

    let (mut before, server) = tokio::io::duplex(4096);
    offer.send(server).expect("offered");
    let answer = exchange(&mut before, head).await;
    assert_eq!(
        answer.split_once("\r\n\r\n").map(|(_, body)| body),
        Some("ok")
    );

    // A pipe of 1 byte: the first bytes are written only once the server reads them.
    let (mut across, server) = tokio::io::duplex(1);
    offer.send(server).expect("offered");
    across.write_all(b"GET ").await.expect("written");
    drain_on(async {}, serving.clone()).await;
    let answer = exchange(&mut across, &head["GET ".len()..]).await;
    let (status, body) = answer.split_once("\r\n\r\n").expect("a head");
    assert!(
        status.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
        "{answer}"
    );
    assert_eq!(body, "draining", "{answer}");

    let (mut after, server) = tokio::io::duplex(4096);
    offer.send(server).expect("offered");
    let answer = exchange(&mut after, head).await;
    assert_eq!(
        answer.split_once("\r\n\r\n").map(|(_, body)| body),
        Some("draining")
    );
    listening.abort();
}

/// Proves: every method of `/healthz` but `GET` and `HEAD` (`POST`, `PUT`, `DELETE`, `PATCH`,
/// `OPTIONS`) and every other path (the root, a trailing slash, another case, a path below it,
/// the private listener's routes and the UI) are `404` with the body `not_found`, a `HEAD` of
/// another path too, whatever the store's state.
///
/// Catches: a health listener that widens the API (a status read, an operator action, a UI file
/// with no token), and a method that changes something answered as a probe.
#[tokio::test]
async fn any_other_path_or_method_is_404() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-404", &fake);
    let not_found = (StatusCode::NOT_FOUND, Bytes::from_static(b"not_found"));
    for method in [
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
    ] {
        let (status, body, _, _) = asked(&rig.coordinator, &serving, &method, HEALTH_PATH).await;
        assert_eq!((status, body), not_found, "{method}");
    }
    fake.set_health(Health::Lost);
    for path in [
        "/",
        "/healthz/",
        "/HEALTHZ",
        "/healthz/more",
        "/health",
        "/v1/status",
        "/v1/operator",
        "/ui/",
    ] {
        let (status, body, _, _) = asked(&rig.coordinator, &serving, &Method::GET, path).await;
        assert_eq!((status, body), not_found, "{path}");
    }
    let (status, body, _, _) = asked(&rig.coordinator, &serving, &Method::HEAD, "/v1/status").await;
    assert_eq!((status, body), not_found);
}

/// Writes `head` on `stream` and reads everything until the server closes it.
async fn exchange(stream: &mut DuplexStream, head: &str) -> String {
    stream.write_all(head.as_bytes()).await.expect("written");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.expect("read");
    answer
}

/// Proves: over a plain HTTP/1.1 connection, `GET /healthz` with a query is answered
/// `200 ok` with `connection: close`, and the connection is closed after that one answer even
/// when the client asked to keep it alive and sent a second request behind the first.
///
/// Catches: a probe that sends a query string read as another path, and a connection a client
/// can hold open with keep-alive.
#[tokio::test(start_paused = true)]
async fn a_connection_answers_one_request_and_is_closed() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-one-request", &fake);
    let coordinator = Arc::new(rig.coordinator);
    let (mut client, server) = tokio::io::duplex(4096);
    let served = tokio::spawn(connection(
        server,
        Arc::clone(&coordinator),
        serving.clone(),
        PATIENCE,
    ));
    let answer = exchange(
        &mut client,
        "GET /healthz?probe=1 HTTP/1.1\r\nhost: localhost\r\nconnection: keep-alive\r\n\r\n\
         GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n",
    )
    .await;
    let (head, body) = answer.split_once("\r\n\r\n").expect("a head");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(head.contains("\r\nconnection: close"), "{answer}");
    assert_eq!(body, "ok", "one answer: {answer}");
    served.await.expect("the connection ended");
}

/// Proves: a client that connects and sends no request head is closed one patience after it
/// connected, and not before; a client that stops reading its answer has its connection closed
/// one patience after the write stalled.
///
/// Catches: a probe port a client can fill with idle connections.
#[tokio::test(start_paused = true)]
async fn an_idle_or_unread_connection_is_closed_after_the_patience() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-patience", &fake);
    let coordinator = Arc::new(rig.coordinator);

    let (_client, server) = tokio::io::duplex(4096);
    let mut idle = tokio::spawn(connection(
        server,
        Arc::clone(&coordinator),
        serving.clone(),
        PATIENCE,
    ));
    tokio::time::advance(PATIENCE / 2).await;
    assert!(
        futures::poll!(&mut idle).is_pending(),
        "closed before the patience"
    );
    tokio::time::advance(PATIENCE).await;
    idle.await.expect("closed after the patience");

    // A pipe of 1 byte: the answer cannot be written while the client does not read.
    let (mut client, server) = tokio::io::duplex(1);
    let mut unread = tokio::spawn(connection(server, coordinator, serving, PATIENCE));
    client
        .write_all(b"GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("written");
    tokio::time::advance(PATIENCE / 2).await;
    assert!(
        futures::poll!(&mut unread).is_pending(),
        "closed before the patience"
    );
    tokio::time::advance(PATIENCE * 2).await;
    unread.await.expect("closed after the patience");
}

/// Proves: the listener serves at most `max_connections` at once: with one slot held by an idle
/// connection, the next one waits unaccepted, and is accepted and answered `200 ok` once the
/// first is closed.
///
/// Catches: a probe port with no limit on its connections, where a flood takes the process's
/// descriptors.
#[tokio::test(start_paused = true)]
async fn the_listener_serves_at_most_its_connections_at_once() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-slots", &fake);
    let coordinator = Arc::new(rig.coordinator);
    let (offer, offered) = mpsc::unbounded_channel::<DuplexStream>();
    let offered = Arc::new(tokio::sync::Mutex::new(offered));
    let accept = move || {
        let offered = Arc::clone(&offered);
        async move {
            offered
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
        }
    };
    let serving = tokio::spawn(serve(accept, coordinator, serving, PATIENCE, 1));

    let (first, server) = tokio::io::duplex(4096);
    offer.send(server).expect("offered");
    let (mut second, server) = tokio::io::duplex(4096);
    offer.send(server).expect("offered");
    second
        .write_all(b"GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("written");
    let mut answer = String::new();
    let mut reading = std::pin::pin!(second.read_to_string(&mut answer));
    // The test runtime has one thread, so each yield lets every task that is ready run once. An
    // accept loop with no limit needs a few turns to take the second connection, read its head
    // and write the answer; 32 is many times that, and with the limit no number of turns answers.
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(
        futures::poll!(reading.as_mut()).is_pending(),
        "answered while the one slot is held"
    );
    drop(first);
    reading.await.expect("read");
    let (head, body) = answer.split_once("\r\n\r\n").expect("a head");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert_eq!(body, "ok");
    serving.abort();
}

/// Proves: an accept that fails is tried again, and the connection the next accept yields is
/// answered `200 ok`.
///
/// Catches: a health listener that stops for good on one failed accept (a descriptor limit hit
/// for a moment), after which every probe reads the coordinator as down.
#[tokio::test(start_paused = true)]
async fn a_failed_accept_is_tried_again() {
    let fake = Fake::new();
    let (rig, serving) = serving_on("health-accept-failed", &fake);
    let (mut client, server) = tokio::io::duplex(4096);
    let mut yields = vec![
        Ok(server),
        Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
    ];
    let accept = move || {
        let next = yields.pop();
        async move {
            match next {
                Some(next) => next,
                None => std::future::pending().await,
            }
        }
    };
    let serving = tokio::spawn(serve(
        accept,
        Arc::new(rig.coordinator),
        serving,
        PATIENCE,
        1,
    ));
    let answer = exchange(
        &mut client,
        "GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n",
    )
    .await;
    let (head, body) = answer.split_once("\r\n\r\n").expect("a head");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert_eq!(body, "ok");
    serving.abort();
}
