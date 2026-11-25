//! The PROXY protocol header on the heartbeat listener's real path: [`connection_with`] on an
//! in-memory stream whose peer is a documentation address (the proxy, or another source), a host
//! certificate over mutual TLS, a gRPC heartbeat over HTTP/2, and the tunnel the coordinator then
//! dials to the host's sshd, a listener on a port of loopback ([`Rig`]). Nothing but that
//! listener is a socket.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_tls::{Alpn, TlsSource};
use http_body_util::{BodyExt as _, Full};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{
    AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, DuplexStream, ReadBuf,
};
use tokio_rustls::TlsConnector;

use super::Proxies;
use super::tests::{tcp4, v2};
use crate::graceful::Graceful;
use crate::http::Surface;
use crate::listen::{CLIENT_PATIENCE, Remote, connection_with};
use crate::ssh_book::Book as _;
use crate::ssh_tunnel::tests::Rig;

/// The host that heartbeats: one the rig's book holds and its operator may reach.
const HOST: &str = "node-07";

/// The proxy in front of the heartbeat listener, as the listener sees it.
const PROXY: &str = "192.0.2.10:443";

/// A source that is no proxy of the flag.
const STRANGER: &str = "198.51.100.20:5000";

/// The host's own address, which the proxy's header names: loopback, where the rig's sshd is.
const CLIENT: &str = "127.0.0.1:40000";

fn socket(text: &str) -> SocketAddr {
    text.parse().expect("a socket address")
}

/// The server end of an in-memory connection whose peer is `remote`, as a TCP stream's is.
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

/// A heartbeat listener's TLS that requires a host certificate of a CA of its own, and a TLS
/// client that holds one for [`HOST`] and offers `h2`.
struct Tls {
    source: TlsSource,
    client: TlsConnector,
}

impl Tls {
    fn new() -> Self {
        let mut authority = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        authority.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer =
            CertifiedIssuer::self_signed(authority, KeyPair::generate().expect("a CA key"))
                .expect("a CA certificate");
        let issue = |sans: Vec<SanType>, usage| {
            let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
            params.subject_alt_names = sans;
            params.extended_key_usages = vec![usage];
            let key = KeyPair::generate().expect("a key");
            let cert = params.signed_by(&key, &issuer).expect("signed");
            (cert, key)
        };
        let dns = |name: &str| SanType::DnsName(name.to_owned().try_into().expect("a DNS name"));
        let uri = |text: &str| SanType::URI(text.try_into().expect("a URI"));
        let (server, server_key) =
            issue(vec![dns("localhost")], ExtendedKeyUsagePurpose::ServerAuth);
        let (host, host_key) = issue(
            vec![
                uri("urn:kbf:role:updater"),
                uri("urn:kbf:host-role:web"),
                dns(HOST),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        let source = TlsSource::fixed_alpn(
            "heartbeat",
            server.pem().as_bytes(),
            server_key.serialize_pem().as_bytes(),
            Some(issuer.pem().as_bytes()),
            Alpn::H2AndHttp1,
        )
        .expect("a TLS source");
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(issuer.der().to_vec()))
            .expect("a root");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_root_certificates(roots)
            .with_client_auth_cert(
                vec![host.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(host_key.serialize_der())),
            )
            .expect("a client certificate");
        config.alpn_protocols = vec![b"h2".to_vec()];
        Self {
            source,
            client: TlsConnector::from(Arc::new(config)),
        }
    }
}

/// A heartbeat of [`HOST`] that announces `ssh-v1`, as a daemon with an sshd setup sends it.
fn heartbeat() -> Request<Full<Bytes>> {
    let message = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxArm64 as i32,
        agent: Some(agent::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: vec!["heartbeat-v2".to_owned(), "ssh-v1".to_owned()],
        services: vec![agent::ServiceBeat {
            name: "web".to_owned(),
            version: "1.0".to_owned(),
            health: agent::Health::Healthy as i32,
            ..agent::ServiceBeat::default()
        }],
        ..agent::HeartbeatRequest::default()
    }
    .encode_to_vec();
    let mut framed = vec![0];
    framed.extend_from_slice(&u32::try_from(message.len()).expect("short").to_be_bytes());
    framed.extend_from_slice(&message);
    Request::builder()
        .method("POST")
        .uri("https://localhost/hostkeeper.agent.v1.Coordinator/Heartbeat")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Full::new(Bytes::from(framed)))
        .expect("a request")
}

/// What one connection to the heartbeat listener came to.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The heartbeat was answered with this `grpc-status`.
    Answered(String),
    /// The listener closed the connection before the TLS handshake finished.
    Refused,
}

/// One heartbeat of [`HOST`] on a connection from `peer` to a heartbeat listener of `rig` that
/// reads a header from the sources `proxies` (the flag's values), the connection's first bytes
/// being `header`; and the listener's end of the connection, finished.
async fn beat(rig: &Rig, tls: &Tls, proxies: &[&str], peer: &str, header: &[u8]) -> Ended {
    let sources: Vec<_> = proxies
        .iter()
        .map(|source| source.parse().expect("a source"))
        .collect();
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let served = tokio::spawn(connection_with(
        From {
            inner: server,
            remote: socket(peer),
        },
        tls.source.config(),
        Surface::Heartbeat,
        Arc::clone(&rig.coordinator),
        CLIENT_PATIENCE,
        Graceful::default(),
        Proxies::new(&sources),
    ));
    client.write_all(header).await.expect("the header is sent");
    let name = ServerName::try_from("localhost").expect("a name");
    let ended = match tls.client.connect(name, client).await {
        Err(_) => Ended::Refused,
        Ok(stream) => {
            let (mut send, driver) = hyper::client::conn::http2::handshake::<_, _, Full<Bytes>>(
                TokioExecutor::new(),
                TokioIo::new(stream),
            )
            .await
            .expect("an HTTP/2 handshake");
            let driving = tokio::spawn(driver);
            let response = send.send_request(heartbeat()).await.expect("an answer");
            let head = response.headers().get("grpc-status").cloned();
            let body = response.into_body().collect().await.expect("the body");
            let status = body
                .trailers()
                .and_then(|trailers| trailers.get("grpc-status").cloned())
                .or(head)
                .expect("a grpc-status");
            drop(send);
            driving.abort();
            Ended::Answered(status.to_str().expect("ASCII").to_owned())
        }
    };
    if ended == Ended::Refused {
        served.await.expect("the listener's end finished");
    } else {
        served.abort();
    }
    ended
}

/// A version 2 header of TCP over IPv4 from `client` to the proxy's address.
fn header_from(client: &str) -> Vec<u8> {
    v2(0x21, 0x11, &tcp4(socket(client), socket(PROXY)))
}

/// The rig's book hears [`HOST`] from `address`, as a heartbeat before the test's did.
fn hear_from(rig: &Rig, address: &str) {
    rig.book
        .inner
        .heard(HOST, address, &[])
        .expect("the host is heard");
}

/// The address the rig's book holds for [`HOST`].
fn recorded(rig: &Rig) -> Option<String> {
    rig.book.inner.address(HOST)
}

/// Proves: with `--heartbeat-proxy-protocol-from` naming the proxy, a heartbeat that comes
/// through it with a version 2 header records the header's source as the host's address, not the
/// proxy's, and a tunnel to the host then dials that address (the rig's sshd on loopback
/// accepts the connection and the tunnel opens).
///
/// Catches: the tunnels of every host behind a TCP passthrough dialling the passthrough
/// (`dial_failed`), as they did when the listener recorded the TCP peer.
#[tokio::test]
async fn a_heartbeat_through_a_trusted_proxy_records_the_hosts_address_and_a_tunnel_dials_it() {
    let mut rig = Rig::new("proxy-protocol-heartbeat", |_| {}).await;
    let tls = Tls::new();
    // Where the host was last heard from: the proxy, as every host behind it was.
    hear_from(&rig, "192.0.2.10");

    let ended = beat(&rig, &tls, &["192.0.2.10"], PROXY, &header_from(CLIENT)).await;
    assert_eq!(ended, Ended::Answered("0".to_owned()));
    assert_eq!(recorded(&rig).as_deref(), Some("127.0.0.1"));

    let live = rig.tunnel("operator", HOST).await;
    assert_eq!(
        live.host.peer_addr().expect("the dial's address").ip(),
        socket(CLIENT).ip()
    );
}

/// Proves: on a listener that reads a header from the proxy only, a version 1 header from the
/// proxy (and from a network of the flag) is taken, while the same header from a source the
/// flag does not name fails the TLS handshake: the connection is closed, no heartbeat is heard,
/// and the address the book holds stays what it was.
///
/// Catches: a listener that reads a header from every peer, which would let any client that
/// reaches it name the address its tunnels dial.
#[tokio::test]
async fn a_header_from_a_source_the_flag_does_not_name_is_refused() {
    let rig = Rig::new("proxy-protocol-stranger", |_| {}).await;
    let tls = Tls::new();
    let v1 = b"PROXY TCP4 127.0.0.1 192.0.2.10 40000 443\r\n";
    hear_from(&rig, "192.0.2.99");

    let ended = beat(&rig, &tls, &["192.0.2.10"], STRANGER, v1).await;
    assert_eq!(ended, Ended::Refused);
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.99"));
    let ended = beat(&rig, &tls, &["192.0.2.10"], STRANGER, &header_from(CLIENT)).await;
    assert_eq!(ended, Ended::Refused);
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.99"));

    let ended = beat(&rig, &tls, &["192.0.2.10"], PROXY, v1).await;
    assert_eq!(ended, Ended::Answered("0".to_owned()));
    assert_eq!(recorded(&rig).as_deref(), Some("127.0.0.1"));
    hear_from(&rig, "192.0.2.99");
    let ended = beat(&rig, &tls, &["198.51.100.0/24"], STRANGER, v1).await;
    assert_eq!(ended, Ended::Answered("0".to_owned()));
    assert_eq!(recorded(&rig).as_deref(), Some("127.0.0.1"));
}

/// Proves: a connection from the proxy without a header, or with a malformed one, is closed
/// before its handshake and records nothing; a header that names no address (`LOCAL`, the
/// proxy's own connection) is taken and its heartbeat answered, and records nothing either, so
/// the proxy's address is never the host's.
///
/// Catches: a connection from the proxy that skips its header being heard from the proxy's
/// address, which is the address every tunnel would then dial.
#[tokio::test]
async fn a_connection_from_the_proxy_needs_a_header_and_one_naming_no_address_records_none() {
    let rig = Rig::new("proxy-protocol-required", |_| {}).await;
    let tls = Tls::new();
    hear_from(&rig, "192.0.2.99");

    assert_eq!(
        beat(&rig, &tls, &["192.0.2.10"], PROXY, b"").await,
        Ended::Refused
    );
    let malformed = v2(0x21, 0x12, &tcp4(socket(CLIENT), socket(PROXY)));
    assert_eq!(
        beat(&rig, &tls, &["192.0.2.10"], PROXY, &malformed).await,
        Ended::Refused
    );
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.99"));

    let local = v2(0x20, 0x00, &[]);
    let ended = beat(&rig, &tls, &["192.0.2.10"], PROXY, &local).await;
    assert_eq!(ended, Ended::Answered("0".to_owned()));
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.99"));
}

/// Proves: without the flag the listener reads no header: a heartbeat from the address that
/// would be a proxy records that address, as before, and a header from it fails the handshake.
#[tokio::test]
async fn without_the_flag_no_header_is_read_and_the_peer_is_recorded() {
    let rig = Rig::new("proxy-protocol-off", |_| {}).await;
    let tls = Tls::new();

    assert_eq!(
        beat(&rig, &tls, &[], PROXY, b"").await,
        Ended::Answered("0".to_owned())
    );
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.10"));
    assert_eq!(
        beat(&rig, &tls, &[], PROXY, &header_from(CLIENT)).await,
        Ended::Refused
    );
    assert_eq!(recorded(&rig).as_deref(), Some("192.0.2.10"));
}

/// Proves: a trusted proxy that sends nothing is closed once the handshake's patience has passed
/// (the paused clock runs to it), and records nothing.
#[tokio::test(start_paused = true)]
async fn a_proxy_that_sends_no_header_is_closed_after_the_patience() {
    let rig = Rig::new("proxy-protocol-patience", |_| {}).await;
    let tls = Tls::new();
    let (mut client, server) = tokio::io::duplex(1024);
    let source = "192.0.2.10".parse().expect("a source");
    connection_with(
        From {
            inner: server,
            remote: socket(PROXY),
        },
        tls.source.config(),
        Surface::Heartbeat,
        Arc::clone(&rig.coordinator),
        CLIENT_PATIENCE,
        Graceful::default(),
        Proxies::new(&[source]),
    )
    .await;
    let mut rest = Vec::new();
    client.read_to_end(&mut rest).await.expect("closed");
    assert_eq!(rest, Vec::<u8>::new());
    assert_eq!(recorded(&rig).as_deref(), Some("127.0.0.1"));
}
