//! `hostkeeper-coordinator serve --health-listen`: the plain-HTTP loopback listener a probe on
//! the coordinator's machine asks `GET /healthz` of (docs/design/two-programs.md section 2.7).
//! It takes a loopback address only; without the flag there is none; with it, it is bound first
//! and says `starting` until every other listener is bound, then `ok`, and `draining` in every
//! answer made after the stop. The connections are in memory; nothing binds a socket.

mod support;

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use hostkeeper_coordinator::config::{Bound, Parts, StartError, run, start, start_line};
use support::serve::{Files, build_given, flags_of, refusal, roots};
use support::{Net, accepting};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};

/// The health listener's address in these tests.
const HEALTH: &str = "127.0.0.1:9";

/// `files`' arguments with `--health-listen <addr>` added.
fn with_health(files: &Files, addr: &str) -> Vec<std::ffi::OsString> {
    let mut args = files.args();
    args.push("--health-listen".into());
    args.push(addr.into());
    args
}

/// Proves: a `--health-listen` address that is not loopback is refused at start with
/// `HealthListen` naming the address: the IPv4 and IPv6 wildcards, the IPv4 wildcard written as
/// IPv6, an interface's address of either family and the IPv4 broadcast address; the refusal's
/// text names the flag and says what it takes. Every address of 127.0.0.0/8, `::1` and a
/// loopback IPv4 address written as IPv6 are taken and handed to `run` as given.
///
/// Catches: a health listener, which answers with no token and no TLS, bound where another
/// machine reaches it; a check that knows only `127.0.0.1`, or that refuses `::1`.
#[test]
fn a_health_address_that_is_not_loopback_is_refused_at_start_by_name() {
    let files = Files::new("health-flags");
    for refused in [
        "0.0.0.0:9",
        "[::]:9",
        "[::ffff:0.0.0.0]:9",
        "192.0.2.1:9",
        "[2001:db8::1]:9",
        "255.255.255.255:9",
    ] {
        let want: SocketAddr = refused.parse().expect("an address");
        match refusal(with_health(&files, refused)) {
            StartError::HealthListen(got) => assert_eq!(got, want),
            other => panic!("{refused} refused otherwise: {other}"),
        }
    }
    assert_eq!(
        StartError::HealthListen("0.0.0.0:9".parse().expect("an address")).to_string(),
        "--health-listen: 0.0.0.0:9 is not a loopback address; the health listener answers \
         with no token and no TLS, so name an address of 127.0.0.0/8 or ::1"
    );
    for taken in [
        "127.0.0.1:9",
        "127.255.0.1:9",
        "[::1]:9",
        "[::ffff:127.0.0.1]:9",
    ] {
        let parts = build_given(&flags_of(with_health(&files, taken)))
            .unwrap_or_else(|why| panic!("{taken} refused: {why}"));
        assert_eq!(
            parts.health_listen,
            Some(taken.parse().expect("an address"))
        );
    }
}

/// What a `run` on an in-memory network did: the addresses it asked to bind, in order, and the
/// ones it told `ready`; `held` is told each time a bind starts to wait for the gate.
struct Running {
    net: Net,
    asked: Arc<Mutex<Vec<SocketAddr>>>,
    held: Arc<Notify>,
    ready: oneshot::Receiver<Bound>,
    serving: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

/// Runs `parts` on a network of its own. A bind of any address but [`HEALTH`] first waits for a
/// permit of `gate`, so a test holds the start between its binds.
fn run_gated(parts: Parts, gate: Arc<Semaphore>) -> Running {
    let net = Net::default();
    let registry = net.clone();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let held = Arc::new(Notify::new());
    let holding = Arc::clone(&held);
    let health: SocketAddr = HEALTH.parse().expect("an address");
    let bind = move |addr: SocketAddr| {
        log.lock().expect("log").push(addr);
        let receive = registry.listen(addr);
        let gate = Arc::clone(&gate);
        let holding = Arc::clone(&holding);
        async move {
            if addr != health {
                holding.notify_one();
                gate.acquire().await.expect("open").forget();
            }
            Ok::<_, io::Error>((addr, accepting(receive)))
        }
    };
    let (told, ready) = oneshot::channel();
    let serving = tokio::spawn(async move {
        let _ = run(parts, bind, move |bound| {
            let _ = told.send(bound);
        })
        .await;
    });
    Running {
        net,
        asked,
        held,
        ready,
        serving,
    }
}

/// One plain HTTP/1.1 request on a new connection to `addr`: everything the listener wrote
/// before it closed the connection.
async fn plain(net: &Net, addr: SocketAddr, method: &str, path: &str) -> String {
    let mut stream: DuplexStream = net.connect(addr).expect("connected");
    let head = format!("{method} {path} HTTP/1.1\r\nhost: localhost\r\n\r\n");
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

/// Proves: with `--health-listen`, `run` binds the health listener first and serves it at once:
/// while the heartbeat listener's bind is held, `GET /healthz` is answered `503 starting` (a
/// `HEAD` the status and the body's length, with no body); once every listener is bound, `ready` is told the health
/// address beside the others, the start line names it last as `health=`, and `GET /healthz` is
/// `200 ok`; a `POST` of it and another path are `404 not_found`. Each connection answers one
/// request and is closed.
///
/// Catches: a health listener bound only after the others (so a slow start reads as down rather
/// than starting), one that says `ok` before every listener is bound, a start line that leaves
/// out where it is, and a probe connection left open.
#[tokio::test]
async fn the_health_listener_says_starting_until_every_listener_is_bound_then_ok() {
    let files = Files::new("health-run");
    let parts = build_given(&flags_of(with_health(&files, HEALTH))).expect("a start");
    let gate = Arc::new(Semaphore::new(0));
    let mut running = run_gated(parts, Arc::clone(&gate));
    let health: SocketAddr = HEALTH.parse().expect("an address");

    // The health listener answers while the heartbeat listener's bind waits.
    running.held.notified().await;
    let answer = plain(&running.net, health, "GET", "/healthz").await;
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 503 Service Unavailable", "starting"),
        "{answer}"
    );
    let answer = plain(&running.net, health, "HEAD", "/healthz").await;
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 503 Service Unavailable", ""),
        "{answer}"
    );
    assert!(
        answer.contains("\r\ncontent-length: 8\r\n"),
        "the length of `starting`: {answer}"
    );
    assert!(
        running.ready.try_recv().is_err(),
        "ready before every listener is bound"
    );

    gate.add_permits(2);
    let bound = (&mut running.ready).await.expect("ready");
    let heartbeat: SocketAddr = "192.0.2.1:1".parse().expect("an address");
    let control: SocketAddr = "192.0.2.1:2".parse().expect("an address");
    assert_eq!(
        *running.asked.lock().expect("log"),
        vec![health, heartbeat, control],
        "the health listener first"
    );
    assert_eq!(
        bound,
        Bound {
            heartbeat,
            control,
            lan: None,
            mdm: None,
            health: Some(health),
        }
    );
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        start_line(&bound),
        format!(
            "hostkeeper-coordinator {version} heartbeat=192.0.2.1:1 control=192.0.2.1:2 \
             health=127.0.0.1:9"
        )
    );
    let with_lan = Bound {
        lan: Some("192.0.2.7:3".parse().expect("an address")),
        ..bound
    };
    assert_eq!(
        start_line(&with_lan),
        format!(
            "hostkeeper-coordinator {version} heartbeat=192.0.2.1:1 control=192.0.2.1:2 \
             lan=192.0.2.7:3 health=127.0.0.1:9"
        )
    );

    let answer = plain(&running.net, health, "GET", "/healthz").await;
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 200 OK", "ok"),
        "{answer}"
    );
    let answer = plain(&running.net, health, "POST", "/healthz").await;
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 404 Not Found", "not_found"),
        "{answer}"
    );
    let answer = plain(&running.net, health, "GET", "/v1/status").await;
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 404 Not Found", "not_found"),
        "{answer}"
    );
}

/// Proves: without `--health-listen` the start holds no health address, `run` asks to bind the
/// heartbeat and the private listeners only, `ready` is told no health address, and the start
/// line has no `health=` word.
///
/// Catches: a health listener bound by default, on an address no flag named.
#[tokio::test]
async fn there_is_no_health_listener_without_the_flag() {
    let files = Files::new("health-none");
    let parts = build_given(&flags_of(files.args())).expect("a start");
    assert_eq!(parts.health_listen, None);
    let gate = Arc::new(Semaphore::new(2));
    let mut running = run_gated(parts, gate);
    let bound = (&mut running.ready).await.expect("ready");
    let heartbeat: SocketAddr = "192.0.2.1:1".parse().expect("an address");
    let control: SocketAddr = "192.0.2.1:2".parse().expect("an address");
    assert_eq!(
        *running.asked.lock().expect("log"),
        vec![heartbeat, control]
    );
    assert_eq!(bound.health, None);
    assert!(
        !start_line(&bound).contains("health="),
        "{}",
        start_line(&bound)
    );
    let unbound = running.net.connect(HEALTH.parse().expect("an address"));
    assert_eq!(
        unbound.map(drop).map_err(|why| why.kind()),
        Err(io::ErrorKind::ConnectionRefused)
    );
}

/// Proves: through `start`, a probe of the health listener is answered `200 ok` while it serves;
/// once the stop resolves, `start` returns `Ok`, and a probe that connected before the stop and
/// whose request is answered after it is told `503 draining`, not `ok`. (Its connection's task
/// outlives `start` in the test's runtime; it stands for an answer made while the process exits.)
///
/// Catches: a `start` that does not drain on the stop signal, so an answer made between the
/// signal and the exit reads the coordinator as healthy.
#[tokio::test]
async fn an_answer_made_after_the_stop_through_start_is_draining() {
    let files = Files::new("health-stop");
    let health: SocketAddr = HEALTH.parse().expect("an address");
    let head = "GET /healthz HTTP/1.1\r\nhost: localhost\r\n\r\n";
    // Two probes wait for the health listener's accepts: one sent whole, one written as `start`
    // reads it (a pipe of 1 byte).
    let (mut before, first) = tokio::io::duplex(4096);
    before.write_all(head.as_bytes()).await.expect("written");
    let (mut across, second) = tokio::io::duplex(1);
    let (offer, probes) = mpsc::unbounded_channel();
    offer.send(first).expect("offered");
    offer.send(second).expect("offered");
    let probes = Mutex::new(Some(probes));
    // The other listeners accept nothing: their senders are kept until the test ends.
    let quiet = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&quiet);
    let bind = move |addr: SocketAddr| {
        let receive = if addr == health {
            probes.lock().expect("probes").take().expect("bound once")
        } else {
            let (send, receive) = mpsc::unbounded_channel();
            kept.lock().expect("kept").push(send);
            receive
        };
        async move { Ok::<_, io::Error>((addr, accepting(receive))) }
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let args = with_health(&files, HEALTH);
    let running = tokio::spawn(start(flags_of(args), roots(), bind, async move {
        stopped.await.expect("sent");
        Ok(())
    }));

    let mut answer = String::new();
    before.read_to_string(&mut answer).await.expect("read");
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 200 OK", "ok"),
        "{answer}"
    );
    // Written only once the health listener reads it: the probe is connected before the stop.
    across.write_all(b"GET ").await.expect("written");
    stop.send(()).expect("sent");
    running
        .await
        .expect("the start ended")
        .expect("a stop is a clean end");
    across
        .write_all(&head.as_bytes()["GET ".len()..])
        .await
        .expect("written");
    let mut answer = String::new();
    across.read_to_string(&mut answer).await.expect("read");
    assert_eq!(
        status_and_body(&answer),
        ("HTTP/1.1 503 Service Unavailable", "draining"),
        "{answer}"
    );
    drop(quiet);
}
