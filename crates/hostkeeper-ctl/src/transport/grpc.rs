//! gRPC: one unary call of raw protobuf bytes through tonic's generic client, over a TLS channel
//! ([`Tonic`]) or straight into tonic services in-process ([`InMemory`]).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::{Buf as _, BufMut as _, Bytes};
use tonic::Status;
use tonic::body::Body;
use tonic::client::{Grpc, GrpcService};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::codegen::http::{self, uri::PathAndQuery};
use tonic::codegen::{Body as HttpBody, Service, StdError};
use tonic::metadata::MetadataMap;
use tonic::server::NamedService;
use tonic::transport::{Certificate, ClientTlsConfig, Identity};

use super::tls::Material;
use super::{ConnectError, CtlTransport, PeerIdentity, Target};

/// The codec of a call whose messages are already encoded: bytes in, bytes out.
#[derive(Debug, Clone, Copy, Default)]
struct RawCodec;

#[derive(Debug)]
struct RawEncoder;

#[derive(Debug)]
struct RawDecoder;

impl Encoder for RawEncoder {
    type Item = Bytes;
    type Error = Status;

    fn encode(&mut self, item: Bytes, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for RawDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

impl Codec for RawCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = RawEncoder;
    type Decoder = RawDecoder;

    fn encoder(&mut self) -> RawEncoder {
        RawEncoder
    }

    fn decoder(&mut self) -> RawDecoder {
        RawDecoder
    }
}

/// One unary call through `service`: a channel, or a service in-process. `peer`, when given, is
/// put in the request's extensions.
pub(crate) async fn unary<S>(
    service: S,
    path: &str,
    request: Bytes,
    metadata: &MetadataMap,
    peer: Option<PeerIdentity>,
) -> Result<Bytes, Status>
where
    S: GrpcService<Body>,
    S::Error: Into<StdError>,
    S::ResponseBody: HttpBody<Data = Bytes> + Send + 'static,
    <S::ResponseBody as HttpBody>::Error: Into<StdError> + Send,
{
    // Everything that can branch is in the functions below, which are not generic, so that each
    // is compiled once for every service this is called with.
    let (path, call) = request_of(path, request, metadata, peer)?;
    let mut grpc = Grpc::new(service);
    grpc.ready()
        .await
        .map_err(Into::<StdError>::into)
        .map_err(not_ready)?;
    Ok(grpc.unary(call, path, RawCodec).await?.into_inner())
}

/// The path and request of a call: the bytes, the metadata, and the peer identity of an
/// in-memory call.
fn request_of(
    path: &str,
    request: Bytes,
    metadata: &MetadataMap,
    peer: Option<PeerIdentity>,
) -> Result<(PathAndQuery, tonic::Request<Bytes>), Status> {
    let path = PathAndQuery::try_from(path)
        .map_err(|_| Status::internal(format!("{path:?} is not a gRPC path")))?;
    let mut call = tonic::Request::new(request);
    *call.metadata_mut() = metadata.clone();
    if let Some(peer) = peer {
        call.extensions_mut().insert(peer);
    }
    Ok((path, call))
}

/// A runtime on the current thread, for one call.
fn runtime() -> Result<tokio::runtime::Runtime, Status> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Status::internal(format!("cannot start a runtime: {e}")))
}

/// The status of a channel that is not ready.
fn not_ready(e: StdError) -> Status {
    Status::unavailable(format!("the channel is not ready: {e}"))
}

/// gRPC over TLS to `--endpoint`. The runtime and the connection are made in each call.
#[derive(Debug, Clone)]
pub struct Tonic {
    endpoint: tonic::transport::Endpoint,
}

impl Tonic {
    /// The transport of `target`, trusting only its CA and presenting its client identity.
    pub(crate) fn new(target: &Target, material: &Material) -> Result<Tonic, ConnectError> {
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&material.ca_pem))
            .domain_name(target.endpoint.host());
        if let Some(identity) = &material.identity {
            tls = tls.identity(Identity::from_pem(&identity.cert_pem, &identity.key_pem));
        }
        let endpoint = tonic::transport::Endpoint::from(target.endpoint.uri().clone())
            .connect_timeout(target.timeout)
            .timeout(target.timeout)
            .tls_config(tls)
            .map_err(|e| ConnectError::Unusable {
                what: "the TLS material",
                path: target.ca_file.clone(),
                why: e.to_string(),
            })?;
        Ok(Tonic { endpoint })
    }
}

impl CtlTransport for Tonic {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        let runtime = runtime()?;
        runtime.block_on(async {
            let channel = self.endpoint.connect().await.map_err(|e| {
                Status::unavailable(format!("cannot connect to {}: {e}", self.endpoint.uri()))
            })?;
            unary(channel, path, request, metadata, None).await
        })
    }
}

type Reply = Pin<Box<dyn Future<Output = http::Response<Body>> + Send>>;
type Handler = Arc<dyn Fn(http::Request<Body>) -> Reply + Send + Sync>;

/// tonic services, called in-process by their gRPC paths, each request carrying the
/// [`PeerIdentity`] this transport was made with. For tests: no socket, no TLS; a path no
/// service serves answers `UNIMPLEMENTED`, as a tonic server's router does.
#[derive(Clone)]
pub struct InMemory {
    services: BTreeMap<&'static str, Handler>,
    peer: PeerIdentity,
    calls: Arc<AtomicUsize>,
}

impl std::fmt::Debug for InMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemory")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .field("peer", &self.peer)
            .field("calls", &self.calls())
            .finish()
    }
}

impl InMemory {
    /// No service yet; every call comes from `peer`.
    pub fn new(peer: PeerIdentity) -> InMemory {
        InMemory {
            services: BTreeMap::new(),
            peer,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Serves `service` (a generated tonic server, `HostsServer::new(...)`) under its name.
    pub fn serve<S>(mut self, service: S) -> InMemory
    where
        S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Future: Send + 'static,
    {
        let handler: Handler = Arc::new(move |request: http::Request<Body>| -> Reply {
            let mut service = service.clone();
            Box::pin(async move {
                let Ok(()) = std::future::poll_fn(|cx| service.poll_ready(cx)).await;
                let Ok(response) = service.call(request).await;
                response
            })
        });
        self.services.insert(S::NAME, handler);
        self
    }

    /// How many calls were made through this transport and its clones.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The in-memory router: the service named by a request's path, or `UNIMPLEMENTED`.
#[derive(Clone)]
struct Router(BTreeMap<&'static str, Handler>);

impl Service<http::Request<Body>> for Router {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<http::Response<Body>, Infallible>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Infallible>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let name = request
            .uri()
            .path()
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("");
        match self.0.get(name) {
            Some(handler) => {
                let reply = handler(request);
                Box::pin(async move { Ok(reply.await) })
            }
            None => Box::pin(async { Ok(Status::unimplemented("").into_http()) }),
        }
    }
}

impl CtlTransport for InMemory {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let runtime = runtime()?;
        let router = Router(self.services.clone());
        runtime.block_on(unary(
            router,
            path,
            request,
            metadata,
            Some(self.peer.clone()),
        ))
    }
}

impl super::Connect for InMemory {
    fn connect(&self, _target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError> {
        Ok(Box::new(self.clone()))
    }
}

#[cfg(test)]
mod tests;
