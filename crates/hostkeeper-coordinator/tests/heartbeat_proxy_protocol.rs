//! `hostkeeper-coordinator serve --heartbeat-proxy-protocol-from`: the sources whose connections
//! to the heartbeat listener start with a PROXY protocol header. The flag is repeatable, each
//! value an address or a network, and reaches the start's parts as given; a value that is no
//! source is a usage error (exit 2) that names why; without the flag there is none, and its
//! help says what it does. A started coordinator reads a header on its heartbeat listener only,
//! never on its private listener. What the listener does with each header is tested in the crate
//! (`src/proxy_protocol_listen_tests.rs`). Driven in process; nothing binds a socket: each
//! in-memory connection says which documentation address it comes from.

mod support;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use clap::Parser;
use clap::error::ErrorKind;
use futures::future::BoxFuture;
use hostkeeper_coordinator::config::{Bound, Cli, run};
use hostkeeper_coordinator::listen::Remote;
use hostkeeper_coordinator::proxy_protocol::ProxySource;
use rustls::pki_types::ServerName;
use support::serve::{Files, build_given, exit_code, flags_of};
use support::{Ca, client_config, kbf_prefixes};
use tokio::io::{
    AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, DuplexStream, ReadBuf,
};
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

/// The flag this file is about.
const FLAG: &str = "--heartbeat-proxy-protocol-from";

/// `args` with the flag given once per value of `values`.
fn given(args: &[OsString], values: &[&str]) -> Vec<OsString> {
    let mut args = args.to_vec();
    for value in values {
        args.push(FLAG.into());
        args.push((*value).into());
    }
    args
}

fn source(text: &str) -> ProxySource {
    text.parse().expect("a source")
}

/// Proves: each value of the repeated flag reaches the start's parts, in order, and without the
/// flag the heartbeat listener has no source to read a header from.
#[test]
fn each_source_of_the_flag_reaches_the_heartbeat_listener() {
    let files = Files::new("proxy-protocol-flag");
    let parts = build_given(&flags_of(given(
        &files.args(),
        &["192.0.2.10", "2001:db8::/32"],
    )))
    .expect("started");
    assert_eq!(
        parts.heartbeat_proxies,
        vec![source("192.0.2.10"), source("2001:db8::/32")]
    );
    drop(parts);

    let parts = build_given(&flags_of(files.args())).expect("started");
    assert_eq!(parts.heartbeat_proxies, Vec::<ProxySource>::new());
}

/// Proves: a value that is no source (`/0`, bits past the prefix, a name) is refused by the
/// flags' parse with the value and the reason, and the binary exits 2 for it.
///
/// Catches: a `/0` that would let every peer name the address its host's tunnels dial.
#[test]
fn a_value_that_is_no_source_is_a_usage_error_that_names_why() {
    let files = Files::new("proxy-protocol-refused");
    for (value, reason) in [
        (
            "0.0.0.0/0",
            "a /0 network trusts every source; name the proxy's address or its network",
        ),
        (
            "192.0.2.10/24",
            "192.0.2.10/24 has bits set past its prefix; write the network's address",
        ),
        ("edge.example", r#""edge.example" is no IP address"#),
    ] {
        let args = given(&files.args(), &[value]);
        let why = Cli::try_parse_from(args.clone()).expect_err("refused");
        assert_eq!(why.kind(), ErrorKind::ValueValidation, "{why}");
        let text = why.to_string();
        assert!(
            text.contains(&format!(
                "invalid value '{value}' for '{FLAG} <ADDRESS[/BITS]>': {reason}"
            )),
            "{text}"
        );
        assert_eq!(exit_code(args), ExitCode::from(2));
    }
}

/// Proves: `serve --help` names the flag and says a connection from another address is read
/// as TLS.
#[test]
fn the_help_names_the_flag() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(
        help.contains("--heartbeat-proxy-protocol-from <ADDRESS[/BITS]>"),
        "{help}"
    );
    assert!(
        help.contains("A connection from any other address is read as TLS"),
        "{help}"
    );
}

/// The proxy the flag names, as the listeners see its connections.
const PROXY: &str = "192.0.2.10:443";

/// The server end of an in-memory connection that comes from `remote`.
struct From {
    inner: DuplexStream,
    remote: SocketAddr,
}

impl Remote for From {
    fn remote(&self) -> Option<SocketAddr> {
        Some(self.remote)
    }
}

impl AsyncRead for From {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for From {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The listeners of a started coordinator, by the address each was bound at.
type Listening = Arc<Mutex<HashMap<SocketAddr, mpsc::UnboundedSender<From>>>>;

/// The accept function of a listener fed by `receive`.
fn accepting(
    receive: mpsc::UnboundedReceiver<From>,
) -> impl FnMut() -> BoxFuture<'static, io::Result<From>> + Send {
    let receive = Arc::new(tokio::sync::Mutex::new(receive));
    move || {
        let receive = Arc::clone(&receive);
        Box::pin(async move {
            let next = receive.lock().await.recv().await;
            next.ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
        })
    }
}

/// One HTTP/1.1 `GET /` over TLS to the listener at `addr` of `listening`, on a connection from
/// the proxy whose first bytes are `header`: the status line of the answer, or `None` when the
/// TLS handshake failed.
async fn get(
    listening: &Listening,
    addr: SocketAddr,
    tls: &TlsConnector,
    header: &[u8],
) -> Option<String> {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let listener = listening.lock().expect("the listeners")[&addr].clone();
    listener
        .send(From {
            inner: server,
            remote: PROXY.parse().expect("an address"),
        })
        .expect("accepted");
    client.write_all(header).await.expect("the header is sent");
    let name = ServerName::try_from("localhost").expect("a name");
    let mut stream = tls.connect(name, client).await.ok()?;
    let asked = "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(asked.as_bytes()).await.ok()?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.ok()?;
    let answer = String::from_utf8(answer).expect("UTF-8");
    answer.lines().next().map(str::to_owned)
}

/// Proves: a coordinator started with the flag reads a header on its heartbeat listener: a
/// connection from the proxy that starts with one is served there, and one that does not is
/// closed before its handshake; its private listener reads none from the same proxy: the header
/// fails the handshake there, and a connection without one is served.
///
/// Catches: the flag's sources given to no listener (the flag doing nothing) or to the private
/// listener too.
#[tokio::test]
async fn only_the_heartbeat_listener_of_a_started_coordinator_reads_a_header() {
    let files = Files::new("proxy-protocol-run");
    let ca = Ca::new("proxy protocol CA");
    let leaf = ca.coordinator();
    files.put("tls.pem", &leaf.cert_pem, 0o644);
    files.put("tls.key", &leaf.key_pem, 0o600);
    files.put("ca.pem", &ca.pem, 0o644);
    let host = ca.updater("node-07", "web", &kbf_prefixes());
    let mut config = (*client_config(&ca, Some(&host))).clone();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tls = TlsConnector::from(Arc::new(config));
    let parts = build_given(&flags_of(given(&files.args(), &["192.0.2.10"]))).expect("started");

    let listening = Listening::default();
    let registry = Arc::clone(&listening);
    let bind = move |addr: SocketAddr| {
        let (send, receive) = mpsc::unbounded_channel();
        registry.lock().expect("the listeners").insert(addr, send);
        async move { Ok::<_, io::Error>((addr, accepting(receive))) }
    };
    let (bound_tx, bound_rx) = tokio::sync::oneshot::channel::<Bound>();
    let serving = tokio::spawn(async move {
        let _ = run(parts, bind, move |bound| {
            let _ = bound_tx.send(bound);
        })
        .await;
    });
    let bound = bound_rx.await.expect("bound");

    let header = b"PROXY TCP4 203.0.113.5 192.0.2.10 40000 443\r\n";
    let served = get(&listening, bound.heartbeat, &tls, header).await;
    assert!(
        served
            .as_deref()
            .is_some_and(|line| line.starts_with("HTTP/1.1 ")),
        "{served:?}"
    );
    assert_eq!(get(&listening, bound.heartbeat, &tls, b"").await, None);

    assert_eq!(get(&listening, bound.control, &tls, header).await, None);
    let served = get(&listening, bound.control, &tls, b"").await;
    assert!(
        served
            .as_deref()
            .is_some_and(|line| line.starts_with("HTTP/1.1 ")),
        "{served:?}"
    );
    serving.abort();
}
