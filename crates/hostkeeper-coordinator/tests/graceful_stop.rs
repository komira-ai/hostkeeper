//! The stop of `hostkeeper-coordinator serve` (docs/design/coordinator-multi-instance.md,
//! section 11, item 2): from the signal `/readyz` says `draining` and every listener goes on
//! accepting for `--drain-delay-secs`, counted from the signal; then the heartbeat, private and
//! LAN listeners accept no more connections, each open connection is told to finish, and
//! `config::start` returns only when none is open, while the health listener answers until it
//! does. Every test drives `start` over the in-memory network on tokio's paused clock, which is
//! the injected clock here: it moves on only when every task is waiting, so a `start` that is
//! still running after a virtual second is waiting on a connection, the same way on every run;
//! no test measures how long anything took.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_coordinator::config::{StartError, start};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::ServerName;
use support::serve::{Files, flags_of, roots};
use support::{BEAT, Ca, Net, OPERATOR, accepting, client_config, kbf_prefixes};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// The heartbeat listener's address in these tests.
const HEARTBEAT: &str = "192.0.2.1:1";

/// The private (control) listener's address.
const CONTROL: &str = "192.0.2.1:2";

/// The LAN listener's address.
const LAN: &str = "192.0.2.7:3";

/// The health listener's address.
const HEALTH: &str = "127.0.0.1:9";

/// A virtual second: far inside the patience of 10 seconds a connection has for anything, so
/// nothing but a stop can end a connection within it.
const A_SECOND: Duration = Duration::from_secs(1);

fn addr(text: &str) -> SocketAddr {
    text.parse().expect("an address")
}

/// A coordinator started by `config::start` on a network of its own, with the drain delay the
/// test names and a stop the test sends.
struct Coordinator {
    net: Net,
    ca: Ca,
    stop: Option<oneshot::Sender<()>>,
    ended: JoinHandle<Result<(), StartError>>,
    _files: Files,
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.ended.abort();
    }
}

impl Coordinator {
    /// Starts it with the flags `extra` names (given the files they may name) after the
    /// listeners' own and `--drain-delay-secs 0`, and waits until `listeners` addresses are bound.
    async fn start(name: &str, extra: impl FnOnce(&Files) -> Vec<String>, listeners: u32) -> Self {
        Self::start_draining(name, extra, listeners, 0).await
    }

    /// [`Coordinator::start`] with `--drain-delay-secs <delay>`.
    async fn start_draining(
        name: &str,
        extra: impl FnOnce(&Files) -> Vec<String>,
        listeners: u32,
        delay: u64,
    ) -> Self {
        let files = Files::new(name);
        // The coordinator's certificate and the clients' CA are of a CA the test holds, so a
        // client can trust the one and present a certificate of the other.
        let ca = Ca::new("stop CA");
        let leaf = ca.coordinator();
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &ca.pem, 0o644);
        let mut args = files.args();
        args.extend(extra(&files).into_iter().map(OsString::from));
        args.push("--drain-delay-secs".into());
        args.push(delay.to_string().into());
        let net = Net::default();
        let registry = net.clone();
        let bound = Arc::new(Semaphore::new(0));
        let told = Arc::clone(&bound);
        let bind = move |at: SocketAddr| {
            let receive = registry.listen(at);
            told.add_permits(1);
            async move { Ok::<_, io::Error>((at, accepting(receive))) }
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let ended = tokio::spawn(start(flags_of(args), roots(), bind, async move {
            let _ = stopped.await;
            Ok(())
        }));
        bound.acquire_many(listeners).await.expect("open").forget();
        Self {
            net,
            ca,
            stop: Some(stop),
            ended,
            _files: files,
        }
    }

    /// Sends the stop signal.
    fn stop(&mut self) {
        self.stop
            .take()
            .expect("stopped once")
            .send(())
            .expect("the coordinator is listening for it");
    }

    /// Whether `start` has not returned after a virtual second in which every other task had
    /// nothing left to do.
    async fn is_waiting(&mut self) -> bool {
        tokio::time::timeout(A_SECOND, &mut self.ended)
            .await
            .is_err()
    }

    /// What `start` returned, which it must have by a virtual second after nothing is left to do.
    async fn returned(&mut self) -> Result<(), StartError> {
        tokio::time::timeout(A_SECOND, &mut self.ended)
            .await
            .expect("start returned")
            .expect("start did not panic")
    }

    /// A TLS client end of a new connection to `at`, trusting the coordinator's certificate and
    /// presenting `identity` when given, with the handshake done.
    async fn dial(
        &self,
        at: &str,
        h2: bool,
        identity: Option<&support::Leaf>,
    ) -> TlsStream<DuplexStream> {
        let stream = self.net.connect(addr(at)).expect("connected");
        let mut config = (*client_config(&self.ca, identity)).clone();
        if h2 {
            config.alpn_protocols = vec![b"h2".to_vec()];
        }
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").expect("a name"), stream)
            .await
            .expect("a handshake")
    }

    /// The certificate of an updater of this coordinator's farm.
    fn updater(&self) -> support::Leaf {
        self.ca.updater("d1", "daemon", &kbf_prefixes())
    }

    /// The error of connecting to `at`: nobody accepts there.
    fn refused(&self, at: &str) -> io::ErrorKind {
        self.net
            .connect(addr(at))
            .map(drop)
            .expect_err("nobody accepts there")
            .kind()
    }
}

/// The head of a heartbeat's request, and its body in two parts.
fn heartbeat_in_two() -> (String, &'static str, &'static str) {
    let (first, second) = BEAT.split_at(BEAT.len() / 2);
    let head = format!(
        "POST /v1/heartbeat HTTP/1.1\r\nhost: localhost\r\ncontent-length: {}\r\n\r\n",
        BEAT.len()
    );
    (head, first, second)
}

/// Proves: a heartbeat that is half sent when the stop comes is answered in full once the rest
/// arrives, the connection is closed after the answer (no keep-alive past the request in
/// flight), and `start` has not returned until then and returns `Ok` after; a connection opened
/// after the stop is not accepted, on the heartbeat listener and the private one.
///
/// Catches: a `start` that returns at the stop and drops the connection tasks with a decision
/// taken and no answer sent, one that answers the request in flight and keeps the connection
/// for the next, and a listener that goes on accepting while it waits.
#[tokio::test(start_paused = true)]
async fn a_request_in_flight_at_the_stop_is_answered_in_full_and_then_start_returns() {
    let mut coordinator = Coordinator::start("stop-in-flight", |_| Vec::new(), 2).await;
    let who = coordinator.updater();
    let mut tls = coordinator.dial(HEARTBEAT, false, Some(&who)).await;
    let (head, first, second) = heartbeat_in_two();
    tls.write_all(head.as_bytes()).await.expect("sent");
    tls.write_all(first.as_bytes()).await.expect("sent");

    coordinator.stop();
    assert!(
        coordinator.is_waiting().await,
        "start returned with a request in flight"
    );
    assert_eq!(
        coordinator.refused(HEARTBEAT),
        io::ErrorKind::ConnectionReset
    );
    assert_eq!(coordinator.refused(CONTROL), io::ErrorKind::ConnectionReset);

    tls.write_all(second.as_bytes()).await.expect("sent");
    let mut answer = Vec::new();
    tokio::time::timeout(A_SECOND, tls.read_to_end(&mut answer))
        .await
        .expect("the connection was closed after the answer")
        .expect("read to the close");
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("\r\n\r\n{"), "a body: {answer}");
    coordinator.returned().await.expect("a clean stop");
}

/// Proves: an HTTP/1.1 connection that is idle between two requests when the stop comes (its
/// first request was answered and it is kept alive) is closed by the coordinator, and `start`
/// returns `Ok` without anyone dropping the client's end.
///
/// Catches: an idle keep-alive connection that holds `start` until its patience is out.
#[tokio::test(start_paused = true)]
async fn an_idle_http1_connection_is_closed_at_the_stop() {
    let mut coordinator = Coordinator::start("stop-idle-h1", |_| Vec::new(), 2).await;
    let mut tls = coordinator.dial(CONTROL, false, None).await;
    let request = format!(
        "GET /v1/status HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer {OPERATOR}\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).await.expect("sent");
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("\r\n\r\n{") {
        let mut chunk = [0_u8; 4096];
        let n = tls.read(&mut chunk).await.expect("read");
        assert_ne!(n, 0, "closed before the answer");
        seen.extend_from_slice(&chunk[..n]);
    }
    assert!(
        coordinator.is_waiting().await,
        "start returned before the stop"
    );

    coordinator.stop();
    let mut rest = Vec::new();
    tokio::time::timeout(A_SECOND, tls.read_to_end(&mut rest))
        .await
        .expect("closed by the coordinator at the stop")
        .expect("read to the close");
    coordinator.returned().await.expect("a clean stop");
}

/// Proves: an idle HTTP/2 connection is told to go away when the stop comes (its client's
/// driver ends, and a new request on it is refused), the connection then closes, and `start`
/// returns `Ok`, all within a virtual second, which is a tenth of the patience after which an
/// idle one is told anyway.
///
/// Catches: an HTTP/2 connection that is left to its patience at the stop, so a stopping
/// coordinator waits ten seconds for each idle client.
#[tokio::test(start_paused = true)]
async fn an_idle_http2_connection_is_told_to_go_away_at_the_stop() {
    let mut coordinator = Coordinator::start("stop-idle-h2", |_| Vec::new(), 2).await;
    let tls = coordinator.dial(CONTROL, true, None).await;
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut send, driver) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .expect("an HTTP/2 handshake");
    let driver = tokio::spawn(driver);
    let status = || {
        Request::builder()
            .uri("https://localhost/v1/status")
            .header("authorization", format!("Bearer {OPERATOR}"))
            .body(Full::new(Bytes::new()))
            .expect("a request")
    };
    let answer = send.send_request(status()).await.expect("an answer");
    assert_eq!(answer.status(), 200);
    answer.into_body().collect().await.expect("the body");
    assert!(
        coordinator.is_waiting().await,
        "start returned before the stop"
    );

    coordinator.stop();
    let ended = tokio::time::timeout(A_SECOND, driver)
        .await
        .expect("told to go away and closed")
        .expect("the driver did not panic");
    assert!(ended.is_ok(), "{ended:?}");
    assert!(
        send.send_request(status()).await.is_err(),
        "a new call after the go-away"
    );
    coordinator.returned().await.expect("a clean stop");
}

/// Proves: with every listener the flags name (heartbeat, private, LAN, health), a connection
/// that is open and has said nothing keeps `start` waiting after the stop, and meanwhile no new
/// connection is accepted on the heartbeat, private or LAN listener, while the health listener
/// answers `/healthz` `200 ok` and `/readyz` `503 draining`; once that connection ends,
/// `start` returns `Ok`.
///
/// Catches: a stop that leaves the LAN listener, or either of the others, accepting; one that
/// closes the health listener with the rest, so the probe that tells an edge to send no more
/// gets no answer while the coordinator waits; and a wait that ends with the stop signal.
#[tokio::test(start_paused = true)]
async fn every_listener_but_the_health_one_stops_accepting_while_start_waits() {
    let mut coordinator = Coordinator::start(
        "stop-all-listeners",
        |files| {
            let path = |name: &str| files.path(name).display().to_string();
            [
                "--enrollment-dir",
                &path("enrollment"),
                "--ca-key-file",
                &path("ca.key"),
                "--ca-init",
                "--server-cert-file",
                &path("server.pem"),
                "--server-name",
                "localhost",
                "--lan-listen",
                LAN,
                "--health-listen",
                HEALTH,
            ]
            .map(str::to_owned)
            .to_vec()
        },
        4,
    )
    .await;
    // Connected and silent: its handshake is not begun, so only the client ends it.
    let silent = coordinator.net.connect(addr(LAN)).expect("connected");
    // Once nothing else is left to do the listener has accepted it.
    assert!(
        coordinator.is_waiting().await,
        "start returned before the stop"
    );

    coordinator.stop();
    assert!(
        coordinator.is_waiting().await,
        "start returned with a connection open"
    );
    for at in [HEARTBEAT, CONTROL, LAN] {
        assert_eq!(
            coordinator.refused(at),
            io::ErrorKind::ConnectionReset,
            "{at}"
        );
    }
    let (live, ready) = (
        plain(&coordinator.net, "/healthz").await,
        plain(&coordinator.net, "/readyz").await,
    );
    assert_eq!(status_and_body(&live), ("HTTP/1.1 200 OK", "ok"), "{live}");
    assert_eq!(
        status_and_body(&ready),
        ("HTTP/1.1 503 Service Unavailable", "draining"),
        "{ready}"
    );
    assert!(
        coordinator.is_waiting().await,
        "start returned after a probe"
    );

    drop(silent);
    coordinator.returned().await.expect("a clean stop");
}

/// Proves: when no connection is open at the stop, `start` returns `Ok` at once (within a
/// virtual second in which nothing else is left to do, and with no wait for a patience).
///
/// Catches: a wait for connections that never ends when there are none to wait for.
#[tokio::test(start_paused = true)]
async fn start_returns_at_once_when_no_connection_is_open() {
    let mut coordinator = Coordinator::start("stop-no-connection", |_| Vec::new(), 2).await;
    assert!(
        coordinator.is_waiting().await,
        "start returned before the stop"
    );
    coordinator.stop();
    coordinator.returned().await.expect("a clean stop");
}

/// Proves: with `--drain-delay-secs 15` and a signal that comes 100 virtual seconds after the
/// start, from the signal `/readyz` is `503 draining` and `/healthz` stays `200 ok`; 14 seconds
/// into the delay the heartbeat and private listeners still accept, a heartbeat sent then is
/// answered `200`, and `start` has not returned; past the 15 seconds they accept no more, and
/// `start` returns `Ok`.
///
/// Catches: a delay counted from the start rather than the signal (so a coordinator that ran
/// longer than the delay stops accepting at once, before the edge has seen the new instance), a
/// drain that stops accepting at the signal, and one that never stops.
#[tokio::test(start_paused = true)]
async fn the_drain_delay_counts_from_the_signal_and_every_listener_accepts_through_it() {
    let health = |_: &Files| vec!["--health-listen".to_owned(), HEALTH.to_owned()];
    let mut coordinator = Coordinator::start_draining("stop-delay", health, 3, 15).await;
    tokio::time::sleep(Duration::from_secs(100)).await;
    let ready = plain(&coordinator.net, "/readyz").await;
    assert_eq!(
        status_and_body(&ready),
        ("HTTP/1.1 200 OK", "ok"),
        "{ready}"
    );

    coordinator.stop();
    tokio::time::sleep(Duration::from_secs(14)).await;
    let (live, ready) = (
        plain(&coordinator.net, "/healthz").await,
        plain(&coordinator.net, "/readyz").await,
    );
    assert_eq!(status_and_body(&live), ("HTTP/1.1 200 OK", "ok"), "{live}");
    assert_eq!(
        status_and_body(&ready),
        ("HTTP/1.1 503 Service Unavailable", "draining"),
        "{ready}"
    );
    let control = coordinator.net.connect(addr(CONTROL));
    assert!(control.is_ok(), "the private listener accepts in the delay");
    drop(control);
    let who = coordinator.updater();
    let mut tls = coordinator.dial(HEARTBEAT, false, Some(&who)).await;
    let request = format!(
        "POST /v1/heartbeat HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\
         content-length: {}\r\n\r\n{BEAT}",
        BEAT.len()
    );
    tls.write_all(request.as_bytes()).await.expect("sent");
    let mut answer = Vec::new();
    tls.read_to_end(&mut answer)
        .await
        .expect("read to the close");
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(
        !coordinator.ended.is_finished(),
        "start returned in the delay"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        coordinator.refused(HEARTBEAT),
        io::ErrorKind::ConnectionReset
    );
    assert_eq!(coordinator.refused(CONTROL), io::ErrorKind::ConnectionReset);
    coordinator.returned().await.expect("a clean stop");
}

/// One plain HTTP/1.1 `GET` of `path` on the health listener: everything it wrote before it
/// closed the connection.
async fn plain(net: &Net, path: &str) -> String {
    let mut stream = net.connect(addr(HEALTH)).expect("connected");
    let head = format!("GET {path} HTTP/1.1\r\nhost: localhost\r\n\r\n");
    stream.write_all(head.as_bytes()).await.expect("written");
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .await
        .expect("read to the close");
    answer
}

/// The status line and the body of `answer`.
fn status_and_body(answer: &str) -> (&str, &str) {
    let (head, body) = answer.split_once("\r\n\r\n").expect("a head");
    (head.lines().next().expect("a status line"), body)
}
