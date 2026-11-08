//! What the gRPC tests share: a coordinator behind an `Arc` over a scratch directory (with
//! enrollment, an unreachable issuer, or an operator token of a chosen length when a test needs
//! one), updater certificates, a gRPC request and the call of it through
//! [`crate::grpc::route`] with its status and message read back, and a request body that never
//! finishes.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_oidc::Verifier;
use hostkeeper_oidc::testkit::static_verifier;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue, TE};
use hyper::{HeaderMap, Method, Request, StatusCode, Uri, Version};
use prost::Message;
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};
use tonic::{Code, Status};

use crate::desired::DesiredStore;
use crate::enrollment::{Enrollment, Setup, StoreAt, os_random, setup};
use crate::grpc::route;
use crate::http::{BoxError, Peer, Surface};
use crate::service::Coordinator;
use crate::testutil::{
    LEGACY, OPERATOR, Rig, clock_of, kbf_prefixes, kbf_services, ops_of, unreachable_verifier,
};

/// How long a body has in the tests that are not about patience.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// The path of `Coordinator.Heartbeat`, which takes a host's certificate.
pub const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
/// The path of `Releases.SetDesired`, which takes CI's token or the operator's.
pub const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
/// The path of `Releases.Converge`, which takes CI's token or the operator's.
pub const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";
/// The path of `Releases.WaitConverged`, which takes CI's token or the operator's.
pub const WAIT_CONVERGED: &str = "/hostkeeper.operator.v1.Releases/WaitConverged";

/// A coordinator the tests call through the router, and the rig that keeps its directory and
/// its clock alive.
pub struct Served {
    /// The coordinator, shared as the listeners share it.
    pub coordinator: Arc<Coordinator>,
    _rig: Rig,
}

/// A coordinator over the directory and clock of a new rig, made by `build`.
pub fn served_built(
    name: &str,
    build: impl FnOnce(&Scratch, &Arc<AtomicU64>) -> Coordinator,
) -> Served {
    let rig = Rig::new(name);
    let coordinator = Arc::new(build(&rig.dir, &rig.clock));
    Served {
        coordinator,
        _rig: rig,
    }
}

/// The recorded kbf service, the operator token [`OPERATOR`] and a CI issuer that answers.
pub fn served(name: &str) -> Served {
    served_built(name, Rig::start)
}

/// [`served`] with the coordinator's own enrollment, so `Enroll`, `Poll` and `Renew` are served.
pub fn served_with_enrollment(name: &str) -> Served {
    served_built(name, |dir, clock| {
        Rig::start(dir, clock).with_enrollment(enrollment_in(dir))
    })
}

/// [`served`] with a CI issuer whose keys cannot be fetched.
pub fn served_unreachable(name: &str) -> Served {
    served_built(name, |dir, clock| {
        over(dir, clock, unreachable_verifier(), OPERATOR)
    })
}

/// [`served`] with `token` as the one operator token.
pub fn served_with_operator_token(name: &str, token: &str) -> Served {
    served_built(name, |dir, clock| {
        over(dir, clock, static_verifier().0, token)
    })
}

/// The recorded kbf coordinator over `dir`, with `verifier` for CI and `operator` as the one
/// operator token.
fn over(
    dir: &Scratch,
    clock: &Arc<AtomicU64>,
    verifier: Arc<Verifier>,
    operator: &str,
) -> Coordinator {
    let store =
        DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
    Coordinator::new(store, verifier, operator.to_owned(), clock_of(clock))
        .with_services(kbf_services())
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
        .with_ops(ops_of(dir))
}

/// A wall clock that always reads the same second.
struct FixedClock;

impl UnixClock for FixedClock {
    fn now_unix(&self) -> u64 {
        1_800_000_000
    }
}

/// Enrollment over files in `dir`, its CA made now.
fn enrollment_in(dir: &Scratch) -> Arc<Enrollment> {
    let store = dir.path().join("enrollment");
    let ca_key = dir.path().join("ca.key");
    let server_cert = dir.path().join("server.pem");
    let names = ["coordinator.example".to_owned()];
    let files = Setup {
        store: StoreAt::Dir(&store),
        ca_key: &ca_key,
        ca_init: true,
        server_cert: &server_cert,
        server_names: &names,
        server_ips: &[],
    };
    Arc::new(
        setup(Arc::new(RealFs), &files, Arc::new(FixedClock), os_random())
            .expect("enrollment starts"),
    )
}

fn san(sans: &[&str]) -> Vec<SanType> {
    sans.iter()
        .map(|s| match s.strip_prefix("dns:") {
            Some(name) => SanType::DnsName(name.try_into().expect("a name")),
            None => SanType::URI((*s).try_into().expect("a uri")),
        })
        .collect()
}

/// A client certificate with the subjectAltNames `sans` (`dns:` names, else URIs).
pub fn peer(sans: &[&str]) -> Peer {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = san(sans);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    Some(Arc::from(
        params.self_signed(&key).expect("sign").der().to_vec(),
    ))
}

/// The certificate of the farm's updater `name` of host role `host_role`.
pub fn updater(name: &str, host_role: &str) -> Peer {
    peer(&[
        "urn:kbf:role:updater",
        &format!("urn:kbf:host-role:{host_role}"),
        &format!("dns:{name}"),
    ])
}

/// `message` as one uncompressed gRPC message: a zero flag, its length as four big-endian
/// bytes, and the message.
pub fn framed(message: &[u8]) -> Bytes {
    let length = u32::try_from(message.len()).expect("a short message");
    let mut bytes = vec![0];
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(message);
    Bytes::from(bytes)
}

/// A gRPC call of `path` over HTTP/2 with `body` as its body and `bearer` as its token.
pub fn grpc_with<B>(path: &str, body: B, bearer: Option<&str>) -> Request<B> {
    let mut request = Request::new(body);
    *request.method_mut() = Method::POST;
    *request.uri_mut() = path.parse::<Uri>().expect("a path");
    *request.version_mut() = Version::HTTP_2;
    let headers = request.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    headers.insert(TE, HeaderValue::from_static("trailers"));
    if let Some(token) = bearer {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("a header value"),
        );
    }
    request
}

/// A gRPC call of `path` whose body is `body` exactly (the caller frames it).
pub fn grpc_request_raw(path: &str, body: Bytes, bearer: Option<&str>) -> Request<Full<Bytes>> {
    grpc_with(path, Full::new(body), bearer)
}

/// A gRPC call of `path` carrying `message`.
pub fn grpc_request(
    path: &str,
    message: impl Message,
    bearer: Option<&str>,
) -> Request<Full<Bytes>> {
    grpc_request_raw(path, framed(&message.encode_to_vec()), bearer)
}

/// The message inside the one gRPC message `bytes` is.
fn unframed(bytes: &Bytes) -> Bytes {
    assert!(bytes.len() >= 5, "a gRPC message has a five-byte header");
    assert_eq!(bytes[0], 0, "the message is not compressed");
    let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    assert_eq!(
        usize::try_from(length).expect("a length"),
        bytes.len() - 5,
        "the header's length is the message's"
    );
    bytes.slice(5..)
}

/// Routes the gRPC `request` and reads the answer: the status code, its message, and, when the
/// call is `OK`, the bytes of the one message it answered with.
pub async fn call_in<B>(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> (Code, String, Option<Bytes>)
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + Send + 'static,
{
    let response = route(coordinator, surface, peer, request, patience).await;
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("a response body");
    let status = collected
        .trailers()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&parts.headers))
        .expect("a gRPC status in the trailers or the headers");
    let message = (status.code() == Code::Ok).then(|| unframed(&collected.to_bytes()));
    (status.code(), status.message().to_owned(), message)
}

/// [`call_in`] with the patience of the tests that are not about it.
pub async fn call(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    request: Request<Full<Bytes>>,
) -> (Code, String, Option<Bytes>) {
    call_in(coordinator, surface, peer, request, PATIENCE).await
}

/// The message `M` in `bytes`.
pub fn decode<M: Message + Default>(bytes: &[u8]) -> M {
    M::decode(bytes).expect("a message")
}

/// Routes `request` and reads the answer of a route that is not gRPC: its status, headers and
/// body.
pub async fn raw<B>(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    request: Request<B>,
) -> (StatusCode, HeaderMap, Bytes)
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + Send + 'static,
{
    let response = route(coordinator, surface, peer, request, PATIENCE).await;
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.expect("a response body").to_bytes();
    (parts.status, parts.headers, bytes)
}

/// A request body that gives `first` and then never gives another frame and never ends.
pub struct Stalled(Option<Bytes>);

impl Stalled {
    /// A body that stalls after `first`.
    pub fn after(first: Bytes) -> Self {
        Self(Some(first))
    }
}

impl Body for Stalled {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.0.take() {
            Some(first) => Poll::Ready(Some(Ok(Frame::data(first)))),
            None => Poll::Pending,
        }
    }
}
