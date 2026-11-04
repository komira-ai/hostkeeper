//! The gRPC transports: in-memory through a tonic service with a peer identity, and tonic over
//! TLS to a loopback server (127.0.0.1, port 0) with the test PKI.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use tonic::body::Body;
use tonic::codegen::Service;
use tonic::codegen::http;
use tonic::metadata::MetadataMap;
use tonic::server::{NamedService, UnaryService};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};

use super::{InMemory, RawCodec, unary};
use crate::testutil::{Pki, file, loopback_target, pki, run_fixture, scratch, with_call};
use crate::transport::{Connect as _, Network};
use crate::{CtlTransport, PeerIdentity, TransportKind};

/// A tonic service, `hostkeeper.fixture.v1.Gadgets`, that answers a request with its bytes, the
/// request `peer` with the caller's identity, and the request `refuse` with PERMISSION_DENIED.
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
        let body = request.into_inner();
        std::future::ready(match &body[..] {
            b"refuse" => Err(Status::permission_denied("forbidden")),
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
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let mut tls =
            ServerTlsConfig::new().identity(Identity::from_pem(&pki.server_cert, &pki.server_key));
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

/// Catches a refused connection reported as anything but UNAVAILABLE.
#[test]
fn the_tonic_transport_reports_a_closed_port_as_unavailable() {
    let pki = pki();
    let dir = scratch("tonic-closed");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener");
        listener.local_addr().expect("its address").port()
    };
    let transport = Network
        .connect(&loopback_target(port, TransportKind::Grpc, &ca, None))
        .expect("a transport");
    let status = transport
        .call("/a.B/C", Bytes::new(), &MetadataMap::new())
        .expect_err("closed");
    assert_eq!(status.code(), Code::Unavailable);
}
