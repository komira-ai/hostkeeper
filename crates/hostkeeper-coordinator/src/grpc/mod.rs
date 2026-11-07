//! The coordinator's gRPC: `hostkeeper.agent.v1.Coordinator` on the heartbeat listener,
//! `hostkeeper.operator.v1.{Hosts, Releases, Services, Enrollment, Audit, Alerts, Rollouts}` on the
//! private one,
//! and the JSON gateway of the operator API ([`gateway`]), all beside the frozen JSON routes of
//! [`crate::http`].
//!
//! [`route`] is the one entry point of a request. A call over HTTP/2 with a gRPC content type
//! is [`grpc`]; a `POST` of a method of the operator API on the private listener is the
//! gateway's; anything else is a JSON route. A gRPC call is served by the tonic server of its
//! service, called in process, so a message limit and a status are tonic's. The adapters of
//! the services ([`agent`], [`operator`]) call the same [`Coordinator`] functions the JSON
//! routes call and have no rule of their own.
//!
//! The caller is checked before the request is read ([`admit`]): a heartbeat by the host its
//! client certificate names, `SetDesired` and `Converge` by CI's token or the operator's (an
//! operator's token is told first, so it spends no CI token), every other operator method by the
//! operator's token, and `Enroll`, `Poll` and `Renew` by nothing here (the key a
//! host proves, or the certificate `Renew` checks itself). What the check learns goes to the
//! adapter as [`Admitted`] in the request's extensions, so a bearer is resolved once: a CI token
//! is good once.
//!
//! A service is served on one surface only: the agent's on the heartbeat listener (and on the
//! LAN listener of `--lan-listen`, a second listener of the same surface), the operator's on the
//! private one. A path of any other service (the daemon's local one among
//! them) is `UNIMPLEMENTED`.

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_oidc::Verified;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::CoordinatorServer;
use hostkeeper_proto::hostkeeper::operator::v1::alerts_server::AlertsServer;
use hostkeeper_proto::hostkeeper::operator::v1::audit_server::AuditServer;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_server::EnrollmentServer;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::HostsServer;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::ReleasesServer;
use hostkeeper_proto::hostkeeper::operator::v1::rollouts_server::RolloutsServer;
use hostkeeper_proto::hostkeeper::operator::v1::services_server::ServicesServer;
use hostkeeper_wire::wire::MAX_HEARTBEAT_BYTES;
use http_body_util::{Either, Full};
use hyper::body::Body;
use hyper::header::CONTENT_TYPE;
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Version};
use tonic::body::Body as GrpcBody;
use tonic::codegen::Service;
use tonic::{Code, Status};

use self::agent::{AgentService, beat_status};
use self::operator::{
    AlertsService, AuditService, EnrollmentService, HostsService, ReleasesService, RolloutsService,
    ServicesService,
};
use self::refusal::token_status;
use crate::enrollment::MAX_MESSAGE_BYTES;
use crate::http::{
    BoxError, MAX_OPERATOR_BYTES, Peer, Surface, bearer_in, beat_caller, error, handle,
};
use crate::identity::Identity;
use crate::operator::Caller;
use crate::service::{CiRefused, Coordinator, MAX_BEARER_BYTES};

pub(crate) mod agent;
mod beat_map;
pub(crate) mod gateway;
pub(crate) mod operator;
pub(crate) mod refusal;
pub(crate) mod report;

#[cfg(test)]
mod tests;

/// The feature an agent announces to say it may send its heartbeat as a gRPC call. The
/// coordinator advertises it ([`crate::service::FEATURES`]) to an agent that announced it.
pub const GRPC_FEATURE: &str = "grpc-v1";

/// The gRPC path of every method of the operator API that no handler serves yet: each is
/// answered `UNIMPLEMENTED`, with no message, until the coordinator has the handler. None is:
/// a method added to a proto is listed here until its handler lands.
pub const PENDING: &[&str] = &[];

/// The body of an answer: the bytes of a JSON route or the gateway, or a gRPC response.
pub type RouteBody = Either<Full<Bytes>, GrpcBody>;

/// The full name of the agent's service.
const AGENT: &str = "hostkeeper.agent.v1.Coordinator";
/// The full name of the operator API's `Hosts`.
const HOSTS: &str = "hostkeeper.operator.v1.Hosts";
/// The full name of the operator API's `Releases`.
const RELEASES: &str = "hostkeeper.operator.v1.Releases";
/// The full name of the operator API's `Services`.
const SERVICES: &str = "hostkeeper.operator.v1.Services";
/// The full name of the operator API's `Enrollment`.
const ENROLLMENT: &str = "hostkeeper.operator.v1.Enrollment";
/// The full name of the operator API's `Audit`.
const AUDIT: &str = "hostkeeper.operator.v1.Audit";
/// The full name of the operator API's `Alerts`.
const ALERTS: &str = "hostkeeper.operator.v1.Alerts";
/// The full name of the operator API's `Rollouts`.
const ROLLOUTS: &str = "hostkeeper.operator.v1.Rollouts";

/// The path of `Coordinator.Heartbeat`, which needs a client certificate that names a host.
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
/// The path of `Releases.SetDesired`, which takes CI's token or the operator's.
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
/// The path of `Releases.Converge`, which takes CI's token or the operator's: it changes nothing.
const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";

/// What the router learned of a caller before it read the request; the adapter finds it in the
/// request's extensions.
#[derive(Clone)]
pub(crate) enum Admitted {
    /// `Enroll` and `Poll`, which take no credential here, and `Renew`, which takes the
    /// peer's certificate (none when the peer presented none).
    Peer(Peer),
    /// `Heartbeat`: the host the certificate names, and whether it retires.
    Host {
        /// The host and its role.
        identity: Identity,
        /// Whether its heartbeat is heard for its final report only.
        retiring: bool,
    },
    /// `SetDesired` and `Converge` with CI's token: the verified token.
    Ci(Verified),
    /// An operator method with the operator's token (`SetDesired` and `Converge` among them):
    /// the operator, and the bearer it sent.
    Operator {
        /// Who calls.
        caller: Caller,
        /// The token, for the calls that check it themselves.
        bearer: String,
    },
}

impl fmt::Debug for Admitted {
    /// Names the variant and who is admitted; never the bearer token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Peer(peer) => f
                .debug_tuple("Peer")
                .field(&peer.as_ref().map(|der| der.len()))
                .finish(),
            Self::Host { identity, retiring } => f
                .debug_struct("Host")
                .field("identity", identity)
                .field("retiring", retiring)
                .finish(),
            Self::Ci(verified) => f.debug_tuple("Ci").field(verified).finish(),
            Self::Operator { caller, .. } => f
                .debug_struct("Operator")
                .field("caller", caller)
                .finish_non_exhaustive(),
        }
    }
}

/// The full name of the service `path` (`/<package>.<Service>/<Method>`) names.
fn service_of(path: &str) -> Option<&str> {
    Some(path.strip_prefix('/')?.split_once('/')?.0)
}

/// The bearer token of `headers`, if it is no longer than the most the coordinator looks at.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    bearer_in(headers).filter(|token| token.len() <= MAX_BEARER_BYTES)
}

/// Which listener's services a path names.
#[derive(Clone, Copy)]
enum Served {
    /// The agent's service, on the heartbeat listener.
    Agent,
    /// One of the operator API's, on the private listener.
    Operator,
}

/// The credential step of a gRPC call, or a gateway call, of `path` on `surface` from `peer`
/// with `headers`: who the caller is, before any of the request is read.
///
/// # Errors
/// `UNIMPLEMENTED` for a path whose service this surface does not serve. Otherwise the
/// refusal of the service the path names, as a status whose message is its token: a
/// heartbeat's `forbidden` or `denied`; for `SetDesired` and `Converge` with no operator token,
/// CI's `unauthorized` or `keys_unavailable`; the operator's `unauthorized` or `forbidden`.
pub(crate) async fn admit(
    coordinator: &Coordinator,
    surface: Surface,
    path: &str,
    peer: &Peer,
    headers: &HeaderMap,
) -> Result<Admitted, Status> {
    let served = match (surface, service_of(path)) {
        (Surface::Heartbeat, Some(AGENT)) => Served::Agent,
        (
            Surface::Control,
            Some(HOSTS | RELEASES | SERVICES | ENROLLMENT | AUDIT | ALERTS | ROLLOUTS),
        ) => Served::Operator,
        _ => return Err(Status::unimplemented("")),
    };
    match served {
        Served::Agent if path == HEARTBEAT => {
            let caller = beat_caller(coordinator, peer).map_err(beat_status)?;
            Ok(Admitted::Host {
                identity: caller.identity,
                retiring: caller.retiring,
            })
        }
        Served::Agent => Ok(Admitted::Peer(peer.clone())),
        Served::Operator if path == SET_DESIRED || path == CONVERGE => {
            // An operator's token is told before CI's is verified: it is no CI token and
            // spends none.
            if let Ok(name) = coordinator.operator(bearer(headers)) {
                return Ok(Admitted::Operator {
                    caller: Caller::Operator(name),
                    bearer: bearer(headers).unwrap_or_default().to_owned(),
                });
            }
            ci_token(coordinator, headers).await.map(Admitted::Ci)
        }
        Served::Operator => operator_token(coordinator, headers).await,
    }
}

/// CI's token in `headers`, verified.
async fn ci_token(coordinator: &Coordinator, headers: &HeaderMap) -> Result<Verified, Status> {
    coordinator
        .ci(bearer(headers))
        .await
        .map_err(|why| match why {
            CiRefused::Unauthorized(_) => token_status(Code::Unauthenticated, "unauthorized"),
            CiRefused::Unavailable => token_status(Code::Unavailable, "keys_unavailable"),
        })
}

/// The operator `headers` name. CI's token is no operator's: it is `forbidden`.
async fn operator_token(
    coordinator: &Coordinator,
    headers: &HeaderMap,
) -> Result<Admitted, Status> {
    let bearer = bearer(headers);
    match coordinator.caller(bearer).await {
        caller @ Caller::Operator(_) => Ok(Admitted::Operator {
            caller,
            bearer: bearer.unwrap_or_default().to_owned(),
        }),
        Caller::Ci => Err(token_status(Code::PermissionDenied, "forbidden")),
        Caller::Anonymous => Err(token_status(Code::Unauthenticated, "unauthorized")),
    }
}

/// Calls the tonic server `server` with `request`, in process. A generated server is always
/// ready and never fails: a refusal is in the response.
async fn call<S, B>(mut server: S, request: Request<B>) -> Response<GrpcBody>
where
    S: Service<Request<B>, Response = Response<GrpcBody>, Error = Infallible> + Send,
    S::Future: Send,
    B: Send,
{
    let Ok(()) = std::future::poll_fn(|cx| server.poll_ready(cx)).await;
    let Ok(response) = server.call(request).await;
    response
}

/// Serves the gRPC call `request` with the tonic server of its service, in process, with
/// `admitted` in its extensions for the adapter. The service's message limit applies to the
/// request: 16 KiB for a heartbeat, 8 KiB for `Enroll`, `Poll` and `Renew`, 4 KiB for an
/// operator's call. A path of a service this coordinator does not serve is `UNIMPLEMENTED`.
pub(crate) async fn serve_admitted<B>(
    coordinator: &Arc<Coordinator>,
    admitted: Admitted,
    mut request: Request<B>,
) -> Response<GrpcBody>
where
    B: Body + Send + 'static,
    B::Error: Into<BoxError> + Send + 'static,
{
    request.extensions_mut().insert(admitted);
    let coordinator = Arc::clone(coordinator);
    let path = request.uri().path();
    match service_of(path) {
        Some(AGENT) => {
            let limit = if path == HEARTBEAT {
                MAX_HEARTBEAT_BYTES
            } else {
                MAX_MESSAGE_BYTES
            };
            let server = CoordinatorServer::new(AgentService::new(coordinator))
                .max_decoding_message_size(limit);
            call(server, request).await
        }
        Some(HOSTS) => {
            let server = HostsServer::new(HostsService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(RELEASES) => {
            let server = ReleasesServer::new(ReleasesService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(SERVICES) => {
            let server = ServicesServer::new(ServicesService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(ENROLLMENT) => {
            let server = EnrollmentServer::new(EnrollmentService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(AUDIT) => {
            let server = AuditServer::new(AuditService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(ALERTS) => {
            let server = AlertsServer::new(AlertsService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        Some(ROLLOUTS) => {
            let server = RolloutsServer::new(RolloutsService::new(coordinator))
                .max_decoding_message_size(MAX_OPERATOR_BYTES);
            call(server, request).await
        }
        _ => Status::unimplemented("").into_http(),
    }
}

/// Answers the gRPC call `request` that came in on `surface` from `peer`: the caller is
/// checked ([`admit`]) before the request is read, then the service of the path answers. A
/// call that has not been answered after `patience` is `DEADLINE_EXCEEDED`: the bound covers the
/// read of the request and the adapter, whose handlers do not await, so the bound cuts only a
/// request that has not arrived.
pub(crate) async fn grpc<B>(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Response<GrpcBody>
where
    B: Body + Send + 'static,
    B::Error: Into<BoxError> + Send + 'static,
{
    let admitted = match admit(
        coordinator,
        surface,
        request.uri().path(),
        peer,
        request.headers(),
    )
    .await
    {
        Ok(admitted) => admitted,
        Err(status) => return status.into_http(),
    };
    match tokio::time::timeout(patience, serve_admitted(coordinator, admitted, request)).await {
        Ok(response) => response,
        Err(_) => Status::new(Code::DeadlineExceeded, "").into_http(),
    }
}

/// Whether `headers` say the body is gRPC.
fn is_grpc(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/grpc"))
}

/// Answers one request that came in on `surface` from `peer`.
///
/// A call over HTTP/2 with a gRPC content type is a gRPC call ([`grpc`]). Otherwise, on the
/// private listener, a `POST` of a path that names a method of the operator API is the
/// gateway's ([`gateway::answer`]), and any other method of such a path is a 405. Everything
/// else is a JSON route ([`handle`]). A body that has not arrived whole after `patience` is a
/// bad request, or a deadline exceeded for gRPC.
pub async fn route<B>(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Response<RouteBody>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + Send + 'static,
{
    if request.version() == Version::HTTP_2 && is_grpc(request.headers()) {
        return grpc(coordinator, surface, peer, request, patience)
            .await
            .map(Either::Right);
    }
    if surface == Surface::Control && gateway::is_method(request.uri().path()).is_some() {
        let answered = if request.method() == Method::POST {
            gateway::answer(coordinator, peer, request, patience).await
        } else {
            error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        };
        return answered.map(Either::Left);
    }
    handle(coordinator, surface, peer, request, patience)
        .await
        .map(Either::Left)
}
