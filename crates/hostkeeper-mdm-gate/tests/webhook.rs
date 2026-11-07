//! The webhook listener: its routes and their answers, a body over the limit or one that never
//! completes, a client that is not HTTP, and an accept that fails.

mod support;

use std::io;
use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_enrollment::fs::{FaultFs, Fs, RealFs};
use hostkeeper_mdm_gate::webhook::{self, MAX_EVENT_BYTES, PATH};
use hostkeeper_mdm_gate::{FileDeviceStore, Gate};
use http_body_util::{BodyExt as _, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use support::{
    Clock, FakeNanoHub, OTHER_UDID, UDID, authenticate, counting, scratch, token_update,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

type Accepted = io::Result<DuplexStream>;

/// A listener over in-memory streams: `connect` makes a client stream and hands its other half
/// to the listener as an accepted connection.
struct Wire {
    tx: mpsc::UnboundedSender<Accepted>,
    task: tokio::task::JoinHandle<std::convert::Infallible>,
}

impl Wire {
    fn start(gate: Arc<Gate>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Accepted>();
        let rx = Arc::new(Mutex::new(rx));
        let accept = move || {
            let rx = Arc::clone(&rx);
            async move {
                match rx.lock().await.recv().await {
                    Some(accepted) => accepted,
                    None => std::future::pending().await,
                }
            }
        };
        Self {
            tx,
            task: tokio::spawn(webhook::serve(accept, gate)),
        }
    }

    fn connect(&self) -> DuplexStream {
        let (client, server) = tokio::io::duplex(64 * 1024);
        self.tx.send(Ok(server)).expect("the listener is running");
        client
    }

    fn fail_accept(&self) {
        self.tx
            .send(Err(io::Error::other("too many open files")))
            .expect("the listener is running");
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A gate over a scratch directory and a fake NanoHUB.
struct Setup {
    _scratch: hostkeeper_testkit::Scratch,
    _hub: FakeNanoHub,
    gate: Arc<Gate>,
}

async fn setup(name: &str, fs: Arc<dyn Fs>) -> Setup {
    let scratch = scratch(name);
    let hub = FakeNanoHub::start().await;
    let store = Box::new(FileDeviceStore::open(fs, &support::dir_of(&scratch)).expect("opens"));
    let gate =
        Gate::open(store, hub.client(), Arc::new(Clock::at(1_000)), counting()).expect("opens");
    Setup {
        _scratch: scratch,
        _hub: hub,
        gate: Arc::new(gate),
    }
}

/// The status of the answer to `method` `path` with `body` on `stream`.
async fn status_of<S>(stream: S, method: Method, path: &str, body: Vec<u8>) -> u16
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("a client");
    tokio::spawn(connection);
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .body(Full::new(Bytes::from(body)))
        .expect("a request");
    let response = sender.send_request(request).await.expect("an answer");
    let status = response.status().as_u16();
    let _ = response.into_body().collect().await;
    status
}

/// Catches: the answers of the routes: a valid event not recorded or not `200`; a body that is
/// no event not `400`; another path not `404`; another method not `405`.
#[tokio::test]
async fn the_routes_answer_as_documented() {
    let setup = setup("webhook-routes", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    let post = |body: Vec<u8>| status_of(wire.connect(), Method::POST, PATH, body);

    assert_eq!(post(authenticate(UDID, None)).await, 200);
    assert_eq!(setup.gate.records().len(), 1);
    assert_eq!(
        post(br#"{"topic":"mdm.DeclarativeManagement"}"#.to_vec()).await,
        200,
        "passed over"
    );
    assert_eq!(post(b"not an event".to_vec()).await, 400);
    assert_eq!(post(Vec::new()).await, 400);
    assert_eq!(post(br#"{"topic":"mdm.CheckOut"}"#.to_vec()).await, 400);
    assert_eq!(setup.gate.records().len(), 1);

    assert_eq!(
        status_of(wire.connect(), Method::GET, PATH, Vec::new()).await,
        405
    );
    for method in [Method::PUT, Method::DELETE, Method::HEAD] {
        assert_eq!(
            status_of(wire.connect(), method, PATH, Vec::new()).await,
            405
        );
    }
    assert_eq!(
        status_of(
            wire.connect(),
            Method::POST,
            "/other",
            authenticate(OTHER_UDID, None)
        )
        .await,
        404
    );
    assert_eq!(
        status_of(wire.connect(), Method::GET, "/other", Vec::new()).await,
        404
    );
    assert_eq!(
        status_of(wire.connect(), Method::POST, "/webhook/", Vec::new()).await,
        404
    );
    assert_eq!(
        status_of(
            wire.connect(),
            Method::POST,
            "/webhook?x=1",
            authenticate(OTHER_UDID, None)
        )
        .await,
        200
    );
    assert_eq!(
        setup.gate.records().len(),
        2,
        "only the served routes recorded"
    );
}

/// Catches: a body over `MAX_EVENT_BYTES` read whole and taken, or answered with anything but
/// `413`; a body of exactly the limit refused for its size.
#[tokio::test]
async fn a_body_over_the_limit_is_413() {
    let (_log, logs) = support::capture_logs();
    let setup = setup("webhook-big", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    let mut over = authenticate(UDID, None);
    over.resize(MAX_EVENT_BYTES + 1, b' ');
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, over).await,
        413
    );
    assert!(setup.gate.records().is_empty());
    assert_eq!(
        logs.lines(),
        [(
            "WARN".to_owned(),
            "an MDM webhook event over the size limit was refused".to_owned()
        )]
    );

    // A body of exactly the limit is read, and is an event.
    let mut exact = authenticate(UDID, None);
    exact.resize(MAX_EVENT_BYTES, b' ');
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, exact).await,
        200
    );
    assert_eq!(setup.gate.records().len(), 1);
}

/// Catches: a record that cannot be written answered `200` (NanoHUB would think it was taken),
/// or a listener that stops serving after it.
#[tokio::test]
async fn a_record_that_cannot_be_written_is_500() {
    let fs = Arc::new(FaultFs::new());
    let setup = setup("webhook-500", fs.clone()).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        500
    );
    assert!(setup.gate.records().is_empty());
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        200
    );
    assert_eq!(setup.gate.records().len(), 1);
}

/// Catches: a client that is not HTTP taking the listener down, or hanging its connection open;
/// the next client not served.
#[tokio::test]
async fn a_client_that_is_not_http_ends_only_its_own_connection() {
    let setup = setup("webhook-garbage", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    for garbage in [
        &b"\x00\x01\x02 not http\r\n\r\n"[..],
        b"POST /webhook HTTP/9.9\r\n\r\n",
        b"\xff\xfe\r\n\r\n",
    ] {
        let mut client = wire.connect();
        client.write_all(garbage).await.expect("written");
        let mut said = Vec::new();
        client
            .read_to_end(&mut said)
            .await
            .expect("the server closes the connection");
        assert!(
            said.is_empty() || said.starts_with(b"HTTP/1.1 400"),
            "{:?}",
            String::from_utf8_lossy(&said)
        );
    }
    // A client that connects and leaves.
    drop(wire.connect());
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        200
    );
    assert_eq!(setup.gate.records().len(), 1);
}

/// Catches: an accept error ending the listener (no event would be heard again) or spinning
/// without a pause. The 100 ms pause passes on the paused clock.
#[tokio::test(start_paused = true)]
async fn a_failed_accept_is_logged_and_the_listener_goes_on() {
    let (_log, logs) = support::capture_logs();
    let setup = setup("webhook-accept", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    wire.fail_accept();
    wire.fail_accept();
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        200
    );
    assert_eq!(setup.gate.records().len(), 1);
    let failures: Vec<_> = logs
        .events()
        .into_iter()
        .filter(|event| {
            event["message"] == "the MDM webhook listener could not accept a connection"
        })
        .collect();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["level"], "WARN");
    assert_eq!(failures[0]["why"], "too many open files");
}

/// Catches: a client that leaves in the middle of its body answered as a body over the limit
/// (`413`) or taken as an event; the connection simply ends, and the listener goes on.
#[tokio::test]
async fn a_client_that_leaves_in_the_middle_of_its_body_is_not_an_event() {
    let setup = setup("webhook-leaves", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    let mut client = wire.connect();
    client
        .write_all(
            b"POST /webhook HTTP/1.1\r\nhost: localhost\r\ncontent-length: 100\r\n\r\n{\"topic\"",
        )
        .await
        .expect("written");
    client.shutdown().await.expect("closed");
    let mut said = Vec::new();
    client
        .read_to_end(&mut said)
        .await
        .expect("the server ends the connection");
    assert!(
        said.is_empty() || said.starts_with(b"HTTP/1.1 400"),
        "{:?}",
        String::from_utf8_lossy(&said)
    );
    assert!(setup.gate.records().is_empty());
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        200
    );
}

/// Catches: a body that never completes holding the connection and the handler open for ever;
/// the answer is `400`. The clock is paused and advances by itself.
#[tokio::test(start_paused = true)]
async fn a_body_that_never_completes_is_400() {
    let setup = setup("webhook-slow-body", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    let mut client = wire.connect();
    client
        .write_all(
            b"POST /webhook HTTP/1.1\r\nhost: localhost\r\ncontent-length: 100\r\n\r\n{\"topic\"",
        )
        .await
        .expect("written");
    let mut said = vec![0; 12];
    client.read_exact(&mut said).await.expect("an answer");
    assert_eq!(&said, b"HTTP/1.1 400");
    assert!(setup.gate.records().is_empty());
}

/// Catches: a client that never finishes its request head holding the connection open for ever.
#[tokio::test(start_paused = true)]
async fn a_head_that_never_completes_ends_the_connection() {
    let setup = setup("webhook-slow-head", Arc::new(RealFs)).await;
    let wire = Wire::start(Arc::clone(&setup.gate));
    let mut client = wire.connect();
    client
        .write_all(b"POST /webhook HTTP/1.1\r\nhost: loc")
        .await
        .expect("written");
    let mut said = Vec::new();
    client
        .read_to_end(&mut said)
        .await
        .expect("the server closes the connection");
    assert!(
        said.is_empty() || said.starts_with(b"HTTP/1.1 408"),
        "{:?}",
        String::from_utf8_lossy(&said)
    );
    assert_eq!(
        status_of(wire.connect(), Method::POST, PATH, authenticate(UDID, None)).await,
        200
    );
}

/// Catches: the listener working only over in-memory streams: here it serves a real loopback
/// socket, many connections one after another, and two at once.
#[tokio::test]
async fn it_serves_a_real_loopback_socket() {
    let setup = setup("webhook-tcp", Arc::new(RealFs)).await;
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.expect("a port"));
    let addr = listener.local_addr().expect("address");
    assert!(webhook::is_loopback(&addr));
    let accept = move || {
        let listener = Arc::clone(&listener);
        async move { listener.accept().await.map(|(stream, _)| stream) }
    };
    let task = tokio::spawn(webhook::serve(accept, Arc::clone(&setup.gate)));

    let one = status_of(
        TcpStream::connect(addr).await.expect("connect"),
        Method::POST,
        PATH,
        authenticate(UDID, None),
    );
    let two = status_of(
        TcpStream::connect(addr).await.expect("connect"),
        Method::POST,
        PATH,
        authenticate(OTHER_UDID, None),
    );
    assert_eq!(tokio::join!(one, two), (200, 200));
    assert_eq!(setup.gate.records().len(), 2);
    let stream = TcpStream::connect(addr).await.expect("connect");
    assert_eq!(
        status_of(stream, Method::POST, PATH, token_update(UDID, 1)).await,
        200
    );
    assert!(
        setup
            .gate
            .records()
            .iter()
            .any(|record| record.is_enrolled())
    );
    task.abort();
}

/// Catches: a listen address that is not loopback accepted (an IPv4-mapped loopback refused, or
/// an unspecified or documentation address accepted).
#[test]
fn only_loopback_addresses_may_listen() {
    for loopback in [
        "127.0.0.1:9",
        "127.8.8.8:1",
        "[::1]:9",
        "[::ffff:127.0.0.1]:9",
    ] {
        assert!(
            webhook::is_loopback(&loopback.parse().expect("an address")),
            "{loopback}"
        );
    }
    for other in [
        "192.0.2.1:9",
        "0.0.0.0:9",
        "[::]:9",
        "[::ffff:192.0.2.1]:9",
        "[2001:db8::1]:9",
    ] {
        assert!(
            !webhook::is_loopback(&other.parse().expect("an address")),
            "{other}"
        );
    }
}
