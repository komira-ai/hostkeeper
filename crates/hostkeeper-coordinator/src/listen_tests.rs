//! The write deadline of a listener's connections, the protocol a connection speaks (HTTP/2 or
//! HTTP/1.1, by what its handshake settles on), and the tick that re-reads the tokens file.

use std::fs;
use std::future::poll_fn;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::task::noop_waker_ref;
use hostkeeper_proto::hostkeeper::operator::v1::{WaitConvergedRequest, WaitConvergedResponse};
use hostkeeper_tls::TlsSource;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, Version};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tonic::{Code, Status};

use super::{CLIENT_PATIENCE, WriteDeadline, connection, reload_all};
use crate::http::{STATUS_PATH, Surface};
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, Rig, ci_token, scratch};
use crate::tokens::OperatorTokens;

const PATIENCE: Duration = Duration::from_millis(200);

/// Catches: a write that waits on a peer that does not read never failing: it keeps waiting
/// while the patience has not passed (however often it is polled), then fails with `TimedOut`.
#[tokio::test(start_paused = true)]
async fn a_write_the_peer_does_not_take_times_out_after_the_patience() {
    let (near, _far) = tokio::io::duplex(4);
    let mut io = WriteDeadline::new(near, PATIENCE);
    let mut cx = Context::from_waker(noop_waker_ref());
    let first = Pin::new(&mut io).poll_write(&mut cx, b"abcdefgh");
    let first = first.map(|written| written.map_err(|why| why.kind()));
    assert_eq!(first, Poll::Ready(Ok(4)));
    let waiting = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    assert!(waiting.is_pending());
    tokio::time::advance(PATIENCE / 2).await;
    let still = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    assert!(still.is_pending());
    tokio::time::advance(PATIENCE).await;
    let late = Pin::new(&mut io).poll_write(&mut cx, b"efgh");
    let late = late.map(|written| written.map_err(|why| why.kind()));
    assert_eq!(late, Poll::Ready(Err(io::ErrorKind::TimedOut)));
}

/// Catches: the deadline running from the first byte of a long response instead of from the
/// last write that made progress: a peer that keeps reading, slowly, is never cut off.
#[tokio::test(start_paused = true)]
async fn a_peer_that_keeps_reading_is_not_cut_off() {
    let (near, mut far) = tokio::io::duplex(4);
    let mut io = WriteDeadline::new(near, PATIENCE);
    let reader = tokio::spawn(async move {
        let mut got = Vec::new();
        let mut chunk = [0u8; 4];
        for _ in 0..5 {
            tokio::time::sleep(PATIENCE * 3 / 4).await;
            let n = far.read(&mut chunk).await.expect("read");
            got.extend_from_slice(&chunk[..n]);
        }
        got
    });
    let sent = io.write_all(&[7u8; 20]).await;
    assert_eq!(sent.map_err(|e| e.kind()), Ok(()));
    let got = reader.await.expect("joined");
    assert_eq!(got, vec![7u8; 20]);
}

/// Catches: flush, shutdown and read not passing through to the connection: they behave as
/// the inner stream's do.
#[tokio::test(start_paused = true)]
async fn flush_shutdown_and_read_reach_the_connection() {
    let (near, mut far) = tokio::io::duplex(64);
    let mut io = WriteDeadline::new(near, PATIENCE);
    far.write_all(b"ping").await.expect("sent");
    let mut got = [0u8; 4];
    let mut buf = ReadBuf::new(&mut got);
    poll_fn(|cx| Pin::new(&mut io).poll_read(cx, &mut buf))
        .await
        .expect("read");
    assert_eq!(buf.filled(), b"ping");
    io.write_all(b"pong").await.expect("written");
    io.flush().await.expect("flushed");
    io.shutdown().await.expect("shut down");
    let mut rest = Vec::new();
    far.read_to_end(&mut rest).await.expect("read to the end");
    assert_eq!(rest, b"pong");
}

/// A tokens file naming `ci-ops` with the hash of `token`, mode 0600.
fn tokens_file(path: &Path, token: &str) {
    let hash: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(
        path,
        format!("[[token]]\nname = \"ci-ops\"\nsha256 = \"{hash}\"\n"),
    )
    .expect("written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("mode");
}

/// Proves that one look of the tick reads the tokens file again: after `reload_all` (which
/// `reload_every` calls each interval) a token replaced in the file is refused and the new one
/// accepted, with no TLS source in play.
///
/// Catches: the tick that looks at the TLS files leaving the tokens file alone.
#[tokio::test]
async fn one_look_of_the_tick_reads_the_tokens_file_again() {
    let dir = scratch("listen-tick-tokens");
    let path = dir.path().join("tokens.toml");
    let first = "first-token-0123456789abcdef0123456789";
    let second = "second-token-0123456789abcdef0123456789";
    tokens_file(&path, first);
    let tokens = OperatorTokens::from_file(&path).expect("tokens");
    assert!(tokens.name_of(first).is_some());
    tokens_file(&path, second);
    reload_all(None, &[], &tokens).await;
    assert_eq!(tokens.name_of(first), None);
    assert!(tokens.name_of(second).is_some());
}

// ---- the protocol a connection speaks -----------------------------------------------------

/// The path of `Audit.List`, a gRPC method the private listener serves.
const AUDIT_LIST: &str = "/hostkeeper.operator.v1.Audit/List";

/// The path of `Coordinator.Heartbeat`, which the heartbeat listener serves.
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";

/// A server's TLS source over a certificate for `localhost` issued by a CA of its own, and the
/// DER of that CA, for a client to trust. The source's own ALPN is `h2` only: the listener
/// offers what it offers whatever that is.
fn tls_material() -> (TlsSource, Vec<u8>) {
    let mut authority = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let issuer = CertifiedIssuer::self_signed(authority, KeyPair::generate().expect("a CA key"))
        .expect("a CA certificate");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = vec![SanType::DnsName(
        "localhost".to_owned().try_into().expect("a DNS name"),
    )];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().expect("a key");
    let cert = params.signed_by(&key, &issuer).expect("signed");
    let source = TlsSource::fixed(
        "control",
        cert.pem().as_bytes(),
        key.serialize_pem().as_bytes(),
        None,
    )
    .expect("a TLS source");
    (source, issuer.der().to_vec())
}

/// A TLS client that trusts the CA `ca` and offers the application protocols `offered`.
fn connector(ca: &[u8], offered: &[&[u8]]) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.to_vec()))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = offered.iter().map(|name| name.to_vec()).collect();
    TlsConnector::from(Arc::new(config))
}

/// A connection to a listener of `surface` that serves `coordinator`, in memory: the server end
/// is served by [`connection`] on a task, and the client end has finished its handshake.
async fn dial(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    offered: &[&[u8]],
) -> TlsStream<DuplexStream> {
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(64 * 1024);
    tokio::spawn(connection(
        server,
        source,
        surface,
        Arc::clone(coordinator),
        CLIENT_PATIENCE,
    ));
    connector(&ca, offered)
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake")
}

/// The protocol the handshake of `tls` settled on.
fn negotiated(tls: &TlsStream<DuplexStream>) -> Option<Vec<u8>> {
    tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec)
}

/// A coordinator in a rig of its own, with the rig's directory, which must outlive it.
fn coordinator(name: &str) -> (Arc<Coordinator>, hostkeeper_testkit::Scratch) {
    let rig = Rig::new(name);
    (Arc::new(rig.coordinator), rig.dir)
}

/// One `GET` of the status as the operator over HTTP/1.1 on `tls`, and the whole answer.
async fn status_over_http1(mut tls: TlsStream<DuplexStream>) -> String {
    let request = format!(
        "GET {STATUS_PATH} HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer {OPERATOR}\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).await.expect("sent");
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await.expect("the whole answer");
    String::from_utf8(raw).expect("UTF-8")
}

/// An HTTP/2 client over `tls`.
async fn http2(
    tls: TlsStream<DuplexStream>,
) -> hyper::client::conn::http2::SendRequest<Full<Bytes>> {
    let (send, driver) = hyper::client::conn::http2::handshake::<_, _, Full<Bytes>>(
        TokioExecutor::new(),
        TokioIo::new(tls),
    )
    .await
    .expect("an HTTP/2 handshake");
    tokio::spawn(driver);
    send
}

/// The request of the JSON route `GET /v1/status` as the operator, over HTTP/2.
fn status_request() -> Request<Full<Bytes>> {
    Request::builder()
        .method("GET")
        .uri(format!("https://localhost{STATUS_PATH}"))
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::new()))
        .expect("a request")
}

/// The gRPC call of `path` with an empty message, with `bearer` if there is one, over HTTP/2.
fn grpc_request(path: &str, bearer: Option<&str>) -> Request<Full<Bytes>> {
    let call = Request::builder()
        .method("POST")
        .uri(format!("https://localhost{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    let call = match bearer {
        Some(token) => call.header("authorization", format!("Bearer {token}")),
        None => call,
    };
    call.body(Full::new(Bytes::from_static(&[0, 0, 0, 0, 0])))
        .expect("a request")
}

/// The status a gRPC answer ended with: from its trailers, or from its headers when it had no
/// body.
async fn grpc_status(response: Response<hyper::body::Incoming>) -> Status {
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("the body");
    collected
        .trailers()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&parts.headers))
        .expect("a grpc-status")
}

/// Proves: a client that offers `h2` and `http/1.1` settles on `h2`, and the one connection then
/// answers a JSON route (the operator's status, 200, a JSON object) and a gRPC call
/// (`Audit.List`, 200 with `application/grpc` and status `OK`), both over HTTP/2.
///
/// Catches: a listener that serves HTTP/1.1 to a client that negotiated `h2` (the handshake
/// would fail), one that does not offer `h2` (the client would settle on `http/1.1`), or one that
/// serves HTTP/2 but cannot answer a gRPC call or the JSON routes on it.
#[tokio::test]
async fn a_client_that_offers_h2_is_served_http2_for_json_routes_and_grpc_calls() {
    let (coordinator, _dir) = coordinator("listen-h2");
    let tls = dial(&coordinator, Surface::Control, &[b"h2", b"http/1.1"]).await;
    assert_eq!(negotiated(&tls), Some(b"h2".to_vec()));
    let mut send = http2(tls).await;

    let json = send
        .send_request(status_request())
        .await
        .expect("an answer");
    assert_eq!(json.version(), Version::HTTP_2);
    assert_eq!(json.status(), 200);
    let body = json
        .into_body()
        .collect()
        .await
        .expect("the body")
        .to_bytes();
    assert!(
        serde_json::from_slice::<Value>(&body)
            .expect("JSON")
            .is_object()
    );

    let call = send
        .send_request(grpc_request(AUDIT_LIST, Some(OPERATOR)))
        .await
        .expect("an answer");
    assert_eq!(call.version(), Version::HTTP_2);
    assert_eq!(call.status(), 200);
    assert_eq!(call.headers()["content-type"], "application/grpc");
    let status = grpc_status(call).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
}

/// Proves: a client that offers only `http/1.1` settles on `http/1.1` (the listener's own source
/// says `h2` only, and the listener still offers both), and is answered the JSON route over
/// HTTP/1.1: a `200` status line and a JSON body.
///
/// Catches: a listener that offers only what its source's ALPN says, which would refuse this
/// client's handshake, or one that serves HTTP/2 to a client that did not negotiate it.
#[tokio::test]
async fn a_client_that_offers_only_http1_is_served_http1() {
    let (coordinator, _dir) = coordinator("listen-http1");
    let tls = dial(&coordinator, Surface::Control, &[b"http/1.1"]).await;
    assert_eq!(negotiated(&tls), Some(b"http/1.1".to_vec()));
    let answer = status_over_http1(tls).await;
    assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
    let (_, body) = answer.split_once("\r\n\r\n").expect("a head and a body");
    assert!(
        serde_json::from_str::<Value>(body)
            .expect("JSON")
            .is_object()
    );
}

/// Proves: a client that offers no application protocol at all settles on none and is served
/// HTTP/1.1, as the clients that predate ALPN are.
///
/// Catches: a listener that assumes `h2` unless the client says `http/1.1`.
#[tokio::test]
async fn a_client_that_offers_no_protocol_is_served_http1() {
    let (coordinator, _dir) = coordinator("listen-no-alpn");
    let tls = dial(&coordinator, Surface::Control, &[]).await;
    assert_eq!(negotiated(&tls), None);
    let answer = status_over_http1(tls).await;
    assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
}

/// Proves: on the heartbeat listener, an HTTP/2 gRPC `Heartbeat` from a client with no
/// certificate is refused with the status `PERMISSION_DENIED` and exactly the message
/// `forbidden`, and the same connection then answers a second call: the refusal ends the call,
/// not the connection.
///
/// Catches: a heartbeat listener that does not route HTTP/2 to the gRPC service, a refusal whose
/// token is lost on the way out, or a refusal that closes the connection.
#[tokio::test]
async fn a_grpc_heartbeat_without_a_certificate_is_refused_on_the_heartbeat_listener() {
    let (coordinator, _dir) = coordinator("listen-heartbeat-h2");
    let tls = dial(&coordinator, Surface::Heartbeat, &[b"h2"]).await;
    assert_eq!(negotiated(&tls), Some(b"h2".to_vec()));
    let mut send = http2(tls).await;
    for _ in 0..2 {
        let response = send
            .send_request(grpc_request(HEARTBEAT, None))
            .await
            .expect("an answer");
        assert_eq!(response.status(), 200);
        let status = grpc_status(response).await;
        assert_eq!(
            (status.code(), status.message()),
            (Code::PermissionDenied, "forbidden")
        );
    }
}

/// A connection to a listener of the private surface that serves `coordinator`, in memory, as
/// [`dial`] makes one, with the task that serves its server end.
async fn dial_served(
    coordinator: &Arc<Coordinator>,
) -> (TlsStream<DuplexStream>, tokio::task::JoinHandle<()>) {
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let serving = tokio::spawn(connection(
        server,
        source,
        Surface::Control,
        Arc::clone(coordinator),
        CLIENT_PATIENCE,
    ));
    let tls = connector(&ca, &[b"h2"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    (tls, serving)
}

/// Proves: an HTTP/2 client that keeps its connection open and answers the listener's pings is
/// told to go away once the patience has passed, and its connection is served no longer than
/// two patiences in all: the task that serves it ends (on the paused clock).
///
/// Catches: an HTTP/2 connection with no bound but its pings, which an idle client keeps
/// forever, so that 1024 of them hold every slot of a listener.
#[tokio::test(start_paused = true)]
async fn an_idle_http2_connection_is_closed_after_the_patience() {
    let (coordinator, _dir) = coordinator("listen-h2-idle");
    let (tls, serving) = dial_served(&coordinator).await;
    let mut send = http2(tls).await;
    let answer = send
        .send_request(status_request())
        .await
        .expect("an answer");
    assert_eq!(answer.status(), 200);
    drop(answer);
    let ended = tokio::time::timeout(CLIENT_PATIENCE * 4, serving).await;
    assert!(
        matches!(ended, Ok(Ok(()))),
        "the connection is still served"
    );
}

/// Proves: an HTTP/2 connection whose client goes away is no longer served, before the
/// patience has passed (on the paused clock).
///
/// Catches: a bound that holds every HTTP/2 connection for the whole patience, whatever its
/// client does.
#[tokio::test(start_paused = true)]
async fn an_http2_connection_its_client_closes_ends_at_once() {
    let (coordinator, _dir) = coordinator("listen-h2-closed");
    let (tls, serving) = dial_served(&coordinator).await;
    drop(tls);
    let ended = tokio::time::timeout(CLIENT_PATIENCE / 2, serving).await;
    assert!(
        matches!(ended, Ok(Ok(()))),
        "the connection is still served"
    );
}

/// A wait for 2.0 of kbf for 30 seconds as the operator: as a gRPC call, or as the gateway's
/// JSON when `gateway`.
fn wait_request(gateway: bool) -> Request<Full<Bytes>> {
    let path = "/hostkeeper.operator.v1.Releases/WaitConverged";
    let call = Request::builder()
        .method("POST")
        .uri(format!("https://localhost{path}"))
        .header("authorization", format!("Bearer {OPERATOR}"));
    let wait = WaitConvergedRequest {
        service: "kbf".to_owned(),
        version: "2.0".to_owned(),
        wait_secs: 30,
    };
    let (call, body) = if gateway {
        let json = r#"{"service":"kbf","version":"2.0","waitSecs":30}"#;
        (
            call.header("content-type", "application/json"),
            Bytes::from(json),
        )
    } else {
        let message = wait.encode_to_vec();
        let mut framed = vec![0];
        framed.extend_from_slice(&u32::try_from(message.len()).expect("short").to_be_bytes());
        framed.extend_from_slice(&message);
        let call = call
            .header("content-type", "application/grpc")
            .header("te", "trailers");
        (call, Bytes::from(framed))
    };
    call.body(Full::new(body)).expect("a request")
}

/// Proves: an HTTP/2 connection that carries a held `Releases.WaitConverged` call, as gRPC or as
/// the gateway's JSON, is still served 25 seconds in, past the two patiences that close any other
/// connection, and takes no new call then; the call is answered at its 30 seconds (`DEADLINE`: no host runs kbf), and the
/// connection then ends within one patience (on the paused clock).
///
/// Catches: a held call cut with its connection two patiences after the handshake, and a
/// connection kept after its held calls are over.
#[tokio::test(start_paused = true)]
async fn a_held_http2_connection_is_closed_one_patience_after_its_wait_ends() {
    for gateway in [false, true] {
        let rig = Rig::new("listen-h2-held");
        rig.coordinator
            .set_desired(Some(&ci_token()), br#"{"version":"2.0"}"#)
            .await
            .expect("set");
        let (coordinator, _dir) = (Arc::new(rig.coordinator), rig.dir);
        let (tls, mut serving) = dial_served(&coordinator).await;
        let mut send = http2(tls).await;
        let mut answer = tokio::spawn(send.send_request(wait_request(gateway)));
        tokio::select! {
            _ = &mut serving => panic!("closed while a wait is held ({gateway})"),
            _ = &mut answer => panic!("answered before its deadline ({gateway})"),
            () = tokio::time::sleep(Duration::from_secs(25)) => {}
        }
        // Past its first patience the connection takes no new call, held wait or not.
        let late = send.send_request(status_request()).await;
        assert!(late.is_err(), "a new call taken 25 seconds in ({gateway})");
        let answer = answer.await.expect("joined").expect("an answer");
        assert_eq!(answer.status(), 200, "{gateway}");
        let (parts, body) = answer.into_parts();
        let collected = body.collect().await.expect("the body");
        let outcome = if gateway {
            let reply: Value = serde_json::from_slice(&collected.to_bytes()).expect("JSON");
            reply["outcome"].as_str().expect("an outcome").to_owned()
        } else {
            let status = collected
                .trailers()
                .and_then(Status::from_header_map)
                .or_else(|| Status::from_header_map(&parts.headers))
                .expect("a grpc-status");
            assert_eq!(status.code(), Code::Ok);
            let bytes = collected.to_bytes();
            let reply = WaitConvergedResponse::decode(&bytes[5..]).expect("a message");
            reply.outcome().as_str_name().to_owned()
        };
        assert_eq!(outcome, "WAIT_OUTCOME_DEADLINE", "{gateway}");
        let ended = tokio::time::timeout(CLIENT_PATIENCE, serving).await;
        assert!(
            matches!(ended, Ok(Ok(()))),
            "the connection is still served ({gateway})"
        );
        drop(send);
    }
}
