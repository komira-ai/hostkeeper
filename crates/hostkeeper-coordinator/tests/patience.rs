//! A client that is slow or silent holds a connection for a bounded time only.
//!
//! Every test serves in-memory connections on paused time: the clock moves on only when every
//! task is waiting, so the server's patience passes at once and the same way on every run. The
//! 5 s waits are on that clock too, and fail a test whose connection the server never closed.
//! The cap test waits on the listener's own behaviour (a second client's handshake does not
//! finish while the first holds the only slot, and does once the first is gone), never on a
//! clock.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_coordinator::http::Surface;
use hostkeeper_coordinator::listen::{Listener, connection, serve_with_slots};
use rustls::pki_types::ServerName;
use support::{BEAT, Farm, accepting, client_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::Semaphore;

const PATIENCE: Duration = Duration::from_millis(200);

/// The client end of an in-memory connection whose server end is served on `surface`.
fn connected(farm: &Farm, surface: Surface) -> DuplexStream {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let tls = match surface {
        Surface::Heartbeat => farm.tls.0.clone(),
        Surface::Control => farm.tls.1.clone(),
    };
    tokio::spawn(connection(
        server,
        tls,
        surface,
        Arc::clone(&farm.coordinator),
        PATIENCE,
    ));
    client
}

/// Catches: a client that connects and never starts its handshake holding the connection
/// open: the server closes it once the patience is out.
#[tokio::test(start_paused = true)]
async fn a_client_that_never_handshakes_is_dropped() {
    let farm = Farm::start("patience-handshake").await;
    let mut silent = connected(&farm, Surface::Heartbeat);
    let mut sink = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), silent.read_to_end(&mut sink));
    assert!(read.await.is_ok(), "the server closed the connection");
}

/// Catches: a client that finishes its handshake and then sends no request holding the
/// connection open: the server closes it once the patience is out.
#[tokio::test(start_paused = true)]
async fn a_client_that_sends_no_request_is_dropped() {
    let farm = Farm::start("patience-request").await;
    let who = farm.updater("d1", "daemon");
    let config = client_config(&farm.ca, Some(&who));
    let name = ServerName::try_from("localhost").expect("a name");
    let mut tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, connected(&farm, Surface::Heartbeat))
        .await
        .expect("a handshake");
    let mut sink = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut sink));
    assert!(read.await.is_ok(), "the server closed the connection");
}

/// Catches: a client that finishes the handshake, announces a body and sends none holding
/// the connection (and its buffers) open: the server answers 400 once the patience is out,
/// on the listener that asks for no client certificate, with no credentials.
#[tokio::test(start_paused = true)]
async fn a_client_that_stalls_its_request_body_is_answered_and_dropped() {
    let farm = Farm::start("patience-body").await;
    let config = client_config(&farm.ca, None);
    let name = ServerName::try_from("localhost").expect("a name");
    let mut tls = tokio_rustls::TlsConnector::from(config)
        .connect(name, connected(&farm, Surface::Control))
        .await
        .expect("a handshake");
    let head = "POST /v1/desired HTTP/1.1\r\nhost: localhost\r\ncontent-length: 4096\r\n\r\n";
    tls.write_all(head.as_bytes()).await.expect("sent");
    let mut sink = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut sink));
    let _ = read.await.expect("the server closed the connection");
    let text = String::from_utf8_lossy(&sink);
    assert!(text.starts_with("HTTP/1.1 400"), "{text}");
}

/// Catches: no cap on the connections of a listener: with a cap of one, a client that holds
/// the only slot keeps a second client waiting, and the second is served once the first is
/// gone. The listener serves connections arriving on the in-memory network, with a patience
/// long enough that the first client's silence is never what frees the slot.
#[tokio::test(start_paused = true)]
async fn a_listener_serves_no_more_connections_than_its_cap() {
    let farm = Farm::start("patience-cap").await;
    let slots = Arc::new(Semaphore::new(1));
    let addr: SocketAddr = "192.0.2.1:3".parse().expect("an address");
    let listener = Listener {
        accept: accepting(farm.net.listen(addr)),
        tls: farm.tls.0.clone(),
        surface: Surface::Heartbeat,
    };
    tokio::spawn(serve_with_slots(
        listener,
        Arc::clone(&farm.coordinator),
        Duration::from_secs(3_600),
        Arc::clone(&slots),
    ));
    let first = farm.net.connect(addr).expect("connected");
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(slots.available_permits(), 0, "the slot is held");

    let who = farm.updater("d1", "daemon");
    let config = client_config(&farm.ca, Some(&who));
    let name = ServerName::try_from("localhost").expect("a name");
    let queued = farm.net.connect(addr).expect("queued");
    let second = tokio::spawn(tokio_rustls::TlsConnector::from(config).connect(name, queued));
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert!(!second.is_finished(), "served beside the first");
    drop(first);
    let late = tokio::time::timeout(Duration::from_secs(60), second).await;
    let mut served = late
        .expect("served after the slot freed")
        .expect("joined")
        .expect("a handshake");
    served
        .write_all(
            format!(
                "POST /v1/heartbeat HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\
                 content-length: {}\r\n\r\n{BEAT}",
                BEAT.len()
            )
            .as_bytes(),
        )
        .await
        .expect("sent");
    let mut answer = Vec::new();
    let _ = served.read_to_end(&mut answer).await;
    let text = String::from_utf8_lossy(&answer);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
}
