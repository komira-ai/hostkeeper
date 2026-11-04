//! The gRPC transports: in-memory through a tonic service with a peer identity, and tonic over
//! TLS to a loopback server (127.0.0.1, port 0) with the test PKI.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use tonic::body::Body;
use tonic::codegen::Service;
use tonic::codegen::http;
use tonic::metadata::MetadataMap;
use tonic::server::{NamedService, ServerStreamingService, UnaryService};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

use super::{InMemory, RawCodec, Tonic, unary, within};
use crate::testutil::{Pki, file, loopback_target, pki, run_fixture, scratch, with_call};
use crate::transport::tls::Material;
use crate::transport::{Connect as _, Network};
use crate::{CtlTransport, PeerIdentity, TransportKind};

/// A tonic service, `hostkeeper.fixture.v1.Gadgets`, that answers a request with its bytes, the
/// request `peer` with the caller's identity, the request `auth` with the `authorization`
/// metadata it carried (`none` when it carried none), and the request `refuse` with
/// PERMISSION_DENIED.
#[derive(Clone)]
struct Echo;

impl NamedService for Echo {
    const NAME: &'static str = "hostkeeper.fixture.v1.Gadgets";
}

struct Answer(Option<PeerIdentity>);

impl UnaryService<Bytes> for Answer {
    type Response = Bytes;
    type Future = std::future::Ready<Result<Response<Bytes>, Status>>;

    fn call(&mut self, request: Request<Bytes>) -> Self::Future {
        let bearer = request
            .metadata()
            .get("authorization")
            .map(|value| Bytes::copy_from_slice(value.as_encoded_bytes()));
        let bearer = bearer.unwrap_or_else(|| Bytes::from_static(b"none"));
        let body = request.into_inner();
        std::future::ready(match &body[..] {
            b"refuse" => Err(Status::permission_denied("forbidden")),
            b"auth" => Ok(Response::new(bearer)),
            b"peer" => Ok(Response::new(Bytes::from(format!("{:?}", self.0)))),
            _ => Ok(Response::new(body)),
        })
    }
}

impl Service<http::Request<Body>> for Echo {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<http::Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let peer = request.extensions().get::<PeerIdentity>().cloned();
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(RawCodec);
            Ok(grpc.unary(Answer(peer), request).await)
        })
    }
}

/// Catches the peer identity not reaching the service, a status of the service lost, a path no
/// service serves answered by anything but UNIMPLEMENTED, and calls not counted.
#[test]
fn in_memory_calls_the_service_with_the_peer_identity() {
    let memory = InMemory::new(PeerIdentity::Ci("repo:workflow".to_owned())).serve(Echo);
    let metadata = MetadataMap::new();
    let path = "/hostkeeper.fixture.v1.Gadgets/List";
    assert_eq!(
        memory
            .call(path, Bytes::from_static(b"\x0a\x01x"), &metadata)
            .expect("echo"),
        Bytes::from_static(b"\x0a\x01x")
    );
    assert_eq!(
        memory
            .call(path, Bytes::from_static(b"peer"), &metadata)
            .expect("peer"),
        Bytes::from("Some(Ci(\"repo:workflow\"))")
    );
    let refused = memory
        .call(path, Bytes::from_static(b"refuse"), &metadata)
        .expect_err("refused");
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::PermissionDenied, "forbidden")
    );
    let missing = memory
        .call(
            "/hostkeeper.fixture.v1.Widgets/Make",
            Bytes::new(),
            &metadata,
        )
        .expect_err("not served");
    assert_eq!(missing.code(), Code::Unimplemented);
    let bad = memory
        .call("not a path", Bytes::new(), &metadata)
        .expect_err("a bad path");
    assert_eq!(bad.code(), Code::Internal);
    assert_eq!(memory.calls(), 5);
    let shown = format!("{memory:?}");
    assert!(
        shown.contains("hostkeeper.fixture.v1.Gadgets") && shown.contains("calls: 5"),
        "{shown}"
    );
    // Through `ctl`: the in-memory transport is a `Connect` too.
    let ran = run_fixture(&with_call(&["gadgets", "list"]), &memory, b"");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
    assert_eq!(memory.calls(), 6);
}

/// A service that is never ready.
#[derive(Clone)]
struct NeverReady;

impl Service<http::Request<Body>> for NeverReady {
    type Response = http::Response<Body>;
    type Error = std::io::Error;
    type Future = std::future::Ready<Result<http::Response<Body>, std::io::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Err(std::io::Error::other("closed")))
    }

    fn call(&mut self, _: http::Request<Body>) -> Self::Future {
        std::future::ready(Err(std::io::Error::other("closed")))
    }
}

/// Catches a channel that is not ready reported as anything but UNAVAILABLE.
#[test]
fn a_channel_that_is_not_ready_is_unavailable() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let status = runtime
        .block_on(unary(
            NeverReady,
            "/a.B/C",
            Bytes::new(),
            &MetadataMap::new(),
            None,
        ))
        .expect_err("not ready");
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(status.message(), "the channel is not ready: closed");
}

/// A loopback gRPC server with TLS, serving [`Echo`]; it stops when dropped.
struct Loopback {
    runtime: Option<tokio::runtime::Runtime>,
    port: u16,
}

impl Loopback {
    fn start(pki: &Pki, client_ca: Option<&str>) -> Loopback {
        Loopback::start_with(&pki.server_cert, &pki.server_key, client_ca)
    }

    /// A server presenting `cert` with `key`.
    fn start_with(cert: &str, key: &str, client_ca: Option<&str>) -> Loopback {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let mut tls = ServerTlsConfig::new().identity(Identity::from_pem(cert, key));
        if let Some(ca) = client_ca {
            tls = tls.client_ca_root(Certificate::from_pem(ca));
        }
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                Echo,
                tokio_stream::wrappers::TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Loopback {
            runtime: Some(runtime),
            port,
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Catches the tonic transport not reaching a server its CA signed, sending other bytes than
/// given, or returning other bytes than the server's.
#[test]
fn the_tonic_transport_calls_over_tls() {
    let pki = pki();
    let server = Loopback::start(&pki, None);
    let dir = scratch("tonic-ok");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let target = loopback_target(server.port, TransportKind::Grpc, &ca, None);
    let transport = Network.connect(&target).expect("a transport");
    let reply = transport
        .call(
            "/hostkeeper.fixture.v1.Gadgets/List",
            Bytes::from_static(b"\x0a\x01y"),
            &MetadataMap::new(),
        )
        .expect("a reply");
    assert_eq!(reply, Bytes::from_static(b"\x0a\x01y"));
    let refused = transport
        .call(
            "/hostkeeper.fixture.v1.Gadgets/List",
            Bytes::from_static(b"refuse"),
            &MetadataMap::new(),
        )
        .expect_err("refused");
    assert_eq!(refused.code(), Code::PermissionDenied);
}

/// Catches a server whose certificate does not chain to `--ca-file` being trusted.
#[test]
fn the_tonic_transport_refuses_a_server_of_another_ca() {
    let pki = pki();
    let other = crate::testutil::pki();
    let server = Loopback::start(&pki, None);
    let dir = scratch("tonic-other-ca");
    let ca = file(&dir, "ca.pem", other.ca.as_bytes());
    let target = loopback_target(server.port, TransportKind::Grpc, &ca, None);
    let transport = Network.connect(&target).expect("a transport");
    let status = transport
        .call(
            "/hostkeeper.fixture.v1.Gadgets/List",
            Bytes::new(),
            &MetadataMap::new(),
        )
        .expect_err("another CA");
    assert_eq!(status.code(), Code::Unavailable, "{status:?}");
    assert!(
        status
            .message()
            .starts_with("cannot connect to https://127.0.0.1:"),
        "{status:?}"
    );
}

/// Catches the client certificate not presented to a server that asks for one (the heartbeat
/// listener's mutual TLS), and a call without one going through.
#[test]
fn the_tonic_transport_presents_the_client_certificate() {
    let pki = pki();
    let server = Loopback::start(&pki, Some(&pki.ca));
    let dir = scratch("tonic-mtls");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let cert = file(&dir, "client.pem", pki.client_cert.as_bytes());
    let key = file(&dir, "client.key", pki.client_key.as_bytes());
    let path = "/hostkeeper.fixture.v1.Gadgets/List";
    let with = Network
        .connect(&loopback_target(
            server.port,
            TransportKind::Grpc,
            &ca,
            Some((&cert, &key)),
        ))
        .expect("a transport");
    assert_eq!(
        with.call(path, Bytes::new(), &MetadataMap::new())
            .expect("a reply"),
        Bytes::new()
    );
    let without = Network
        .connect(&loopback_target(
            server.port,
            TransportKind::Grpc,
            &ca,
            None,
        ))
        .expect("a transport");
    assert!(
        without
            .call(path, Bytes::new(), &MetadataMap::new())
            .is_err()
    );
}

/// Catches a client key that is not its certificate's accepted by the tonic transport.
#[test]
fn the_tonic_transport_refuses_a_key_of_another_certificate() {
    let pki = pki();
    let other = crate::testutil::pki();
    let dir = scratch("tonic-key");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let cert = file(&dir, "client.pem", pki.client_cert.as_bytes());
    let key = file(&dir, "other.key", other.client_key.as_bytes());
    let refused = Network
        .connect(&loopback_target(
            0,
            TransportKind::Grpc,
            &ca,
            Some((&cert, &key)),
        ))
        .err()
        .expect("a key of another certificate");
    assert!(
        refused
            .to_string()
            .starts_with(&format!("the TLS material in {ca} is not usable: ")),
        "{refused}"
    );
}

/// A request header map carrying `authorization: <value>`.
fn bearer(value: &str) -> MetadataMap {
    let mut metadata = MetadataMap::new();
    metadata.insert("authorization", value.parse().expect("an ASCII value"));
    metadata
}

/// Catches the caller's authorization metadata altered, dropped or invented on its way to the
/// service, over the in-memory transport and over the real tonic transport with TLS.
#[test]
fn the_bearer_reaches_the_service_exactly() {
    let path = "/hostkeeper.fixture.v1.Gadgets/List";
    let token = "Bearer token-value-1";
    let memory = InMemory::new(PeerIdentity::Anonymous).serve(Echo);
    assert_eq!(
        memory
            .call(path, Bytes::from_static(b"auth"), &bearer(token))
            .expect("a reply"),
        Bytes::from(token)
    );
    assert_eq!(
        memory
            .call(path, Bytes::from_static(b"auth"), &MetadataMap::new())
            .expect("a reply"),
        Bytes::from_static(b"none")
    );

    let pki = pki();
    let server = Loopback::start(&pki, None);
    let dir = scratch("tonic-bearer");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let transport = Network
        .connect(&loopback_target(
            server.port,
            TransportKind::Grpc,
            &ca,
            None,
        ))
        .expect("a transport");
    assert_eq!(
        transport
            .call(path, Bytes::from_static(b"auth"), &bearer(token))
            .expect("a reply"),
        Bytes::from(token)
    );
    assert_eq!(
        transport
            .call(path, Bytes::from_static(b"auth"), &MetadataMap::new())
            .expect("a reply"),
        Bytes::from_static(b"none")
    );
}

/// Catches a server certificate signed by the trusted CA but made for another name, or already
/// expired, being accepted by the tonic transport: the call must end UNAVAILABLE before any
/// request, and so before any bearer, is sent.
#[test]
fn the_tonic_transport_refuses_a_certificate_for_another_name_or_out_of_date() {
    let pki = pki();
    let dir = scratch("tonic-bad-certificates");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    for (what, cert, key) in [
        ("another name", &pki.other_name_cert, &pki.other_name_key),
        ("expired", &pki.expired_cert, &pki.expired_key),
    ] {
        let server = Loopback::start_with(cert, key, None);
        let transport = Network
            .connect(&loopback_target(
                server.port,
                TransportKind::Grpc,
                &ca,
                None,
            ))
            .expect("a transport");
        let status = transport
            .call(
                "/hostkeeper.fixture.v1.Gadgets/List",
                Bytes::from_static(b"auth"),
                &bearer("Bearer token-value-2"),
            )
            .expect_err(what);
        assert_eq!(status.code(), Code::Unavailable, "{what}: {status:?}");
        // The text after the colon is rustls's.
        let ours = format!("cannot connect to https://127.0.0.1:{}/: ", server.port);
        assert!(status.message().starts_with(&ours), "{what}: {status:?}");
    }
}

/// A runtime on the current thread whose clock is paused: it advances only when nothing else
/// can run.
fn paused() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a runtime")
}

/// Catches a peer that takes the connection but never answers the TLS handshake holding the
/// call forever: it must end `DEADLINE_EXCEEDED`, naming `--timeout-secs` and the peer.
#[test]
fn a_peer_that_never_answers_the_handshake_ends_at_the_deadline() {
    let pki = pki();
    let dir = scratch("tonic-silent");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    // Held open, never accepted from: the connection completes and nothing is said.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = listener.local_addr().expect("its address").port();
    let mut target = loopback_target(port, TransportKind::Grpc, &ca, None);
    target.timeout = Duration::from_secs(7);
    let material = Material::read(&target).expect("material");
    let tonic = Tonic::new(&target, &material).expect("a transport");
    let status = paused()
        .block_on(tonic.call_async("/a.B/C", Bytes::new(), &MetadataMap::new()))
        .expect_err("the deadline");
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(
        status.message(),
        format!("the call to https://127.0.0.1:{port}/ did not finish within --timeout-secs 7")
    );
    drop(listener);
}

/// A tonic service that answers every call with its headers and then never sends a message.
#[derive(Clone)]
struct Stalled;

struct NoMessages;

impl ServerStreamingService<Bytes> for NoMessages {
    type Response = Bytes;
    type ResponseStream = tokio_stream::Pending<Result<Bytes, Status>>;
    type Future = std::future::Ready<Result<Response<Self::ResponseStream>, Status>>;

    fn call(&mut self, _: Request<Bytes>) -> Self::Future {
        std::future::ready(Ok(Response::new(tokio_stream::pending())))
    }
}

impl Service<http::Request<Body>> for Stalled {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<http::Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        Box::pin(async move {
            let mut grpc = tonic::server::Grpc::new(RawCodec);
            Ok(grpc.server_streaming(NoMessages, request).await)
        })
    }
}

/// Catches a server that sends the response headers and then leaves the body and trailers
/// pending holding the call forever: the bound covers the response body, not only its headers.
#[test]
fn a_response_that_never_completes_ends_at_the_deadline() {
    let metadata = MetadataMap::new();
    let call = unary(
        Stalled,
        "/hostkeeper.fixture.v1.Gadgets/List",
        Bytes::new(),
        &metadata,
        None,
    );
    let status = paused()
        .block_on(within(
            Duration::from_secs(5),
            "https://stalled.example/",
            Box::pin(call),
        ))
        .expect_err("the deadline");
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(
        status.message(),
        "the call to https://stalled.example/ did not finish within --timeout-secs 5"
    );
}

/// Catches a call that finishes in time being turned into a timeout, or its own status lost.
#[test]
fn a_call_that_finishes_in_time_keeps_its_result() {
    let metadata = MetadataMap::new();
    let call = unary(
        Echo,
        "/hostkeeper.fixture.v1.Gadgets/List",
        Bytes::from_static(b"refuse"),
        &metadata,
        None,
    );
    let status = paused()
        .block_on(within(Duration::from_secs(5), "peer", Box::pin(call)))
        .expect_err("refused");
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden")
    );
}

/// Catches a call made from inside a Tokio runtime (an async test, or `spawn_blocking`)
/// panicking at a nested runtime, or refused, with the in-memory and the tonic transport.
#[test]
fn a_call_from_inside_a_runtime_completes() {
    let path = "/hostkeeper.fixture.v1.Gadgets/List";
    let memory = InMemory::new(PeerIdentity::Anonymous).serve(Echo);
    let current = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let inside = current.block_on(async {
        memory.call(path, Bytes::from_static(b"\x0a\x01x"), &MetadataMap::new())
    });
    assert_eq!(inside.expect("a reply"), Bytes::from_static(b"\x0a\x01x"));

    let pki = pki();
    let server = Loopback::start(&pki, None);
    let dir = scratch("tonic-inside");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let transport = Network
        .connect(&loopback_target(
            server.port,
            TransportKind::Grpc,
            &ca,
            None,
        ))
        .expect("a transport");
    let inside = current.block_on(async {
        transport.call(path, Bytes::from_static(b"\x0a\x01y"), &MetadataMap::new())
    });
    assert_eq!(inside.expect("a reply"), Bytes::from_static(b"\x0a\x01y"));

    let multi = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let blocking = memory.clone();
    let joined = multi.block_on(async move {
        tokio::task::spawn_blocking(move || {
            blocking.call(path, Bytes::from_static(b"\x0a\x01z"), &MetadataMap::new())
        })
        .await
    });
    assert_eq!(
        joined.expect("joined").expect("a reply"),
        Bytes::from_static(b"\x0a\x01z")
    );
}
