//! The two HTTP surfaces, mapped onto [`Coordinator`].
//!
//! - **Heartbeat** (mutual TLS, updaters): `POST /v1/heartbeat` with the heartbeat as JSON;
//!   the answer is the reply as JSON. The caller is the certificate's updater identity
//!   ([`Coordinator::identify`]: the farm CA's subjectAltNames, or, with enrollment, the host a
//!   certificate of the coordinator's CA names); no certificate, or one that names none, is
//!   refused with 403 `forbidden` before the body is read. With enrollment the listener lets a
//!   connection without a client certificate through the handshake (for `Enroll` and `Poll`),
//!   so this refusal is the route's own. A host an operator denied is refused with 403
//!   `denied`: before the body is read once its deny row is final, and after it while it
//!   retires, when its heartbeat is heard for its final report only
//!   ([`Coordinator::final_report`]).
//! - **Private** (TLS, no client certificate; `Surface::Control`): the listener of
//!   `--private-listen`, which serves CI's OIDC calls and the operator's. `POST /v1/desired` with
//!   CI's OIDC token as the bearer and the desired state as JSON; `GET /v1/status` with an
//!   operator token as the bearer; `GET /v1/convergence[?service=<name>]` with CI's OIDC token as
//!   the bearer (an operator token is refused there; no query means the coordinator's legacy
//!   service, and is a bad request when there is none); `POST /v1/operator` with an operator
//!   token as the bearer and an action as JSON (`crate::ops`; CI's token is refused). An
//!   operator token is one of the named tokens ([`crate::tokens`]); its name is the `by` of the
//!   change. With a UI directory (`--ui-dir`, or the binary's default,
//!   [`crate::config::DEFAULT_UI_DIR`]) it also answers `GET` and `HEAD` for `/ui/` and every
//!   path below from that directory ([`crate::ui`]), with no token read; without one those paths
//!   are not found. The heartbeat surface never serves them. `GET /v1/version[?at_least=<X.Y.Z>]`
//!   answers which release this coordinator is, with no token read ([`crate::version_route`]):
//!   CI waits on it before it sets a release a coordinator older than the release would refuse.
//!
//! No answer, on either surface, carries an `Access-Control-*` header: the operator web app is
//! served from the private listener itself, so its calls are same-origin, and a cross-origin
//! `OPTIONS` is answered as any other method a path does not take.
//!
//! An answer of the API is JSON (a file under `/ui/` is the file's bytes), and an error is
//! `{"error":"<code>"}` with a code from a closed set: never a claim value, a path or the text
//! of an internal error. A desired state may name the digest of its version's package
//! (`release_digest`, 64 lowercase hex characters); a version the coordinator refuses is
//! answered with its reason as the code: `unparseable_version` (400), and
//! `below_min_version`, `bad_version` and `digest_conflict` (409), and, with
//! `--release-channel`, `not_in_channel` and `channel_digest_mismatch` (409,
//! [`crate::channel`]). A desired state may instead name the release index of its version
//! (`release_index`, the text of its `index.json`, with `release_index_digest`, its SHA-256),
//! from which each host is ordered the package of its own platform; one the coordinator refuses
//! is answered `index_invalid` or `index_digest_mismatch` (400). A heartbeat that lists no
//! services, at a coordinator with no legacy service, is a bad request. An agent that announces
//! `order-v1` is answered with the order terms of [`crate::order_terms`] on an update, and its
//! heartbeat's top level is never read as the legacy service.
//!
//! These are the frozen JSON routes. [`crate::grpc`] routes a request first: a gRPC call, and
//! the JSON gateway of the operator API, are answered there, and every other request comes here.
//! The heartbeat's caller check ([`beat_caller`]) and the reading and answering of its bytes
//! ([`beat`]) are shared with the gRPC `Heartbeat`, so the two transports have one rule.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_wire::wire::{
    Answer as WireAnswer, HEARTBEAT_PATH, HeartbeatRead, MAX_HEARTBEAT_BYTES, read_heartbeat,
};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use serde::Serialize;

use crate::enrollment::Admission;
use crate::identity::Identity;
use crate::ops::OperateError;
use crate::orders::Order;
use crate::service::{CiRefused, ConvergenceError, Coordinator, SetError};
use crate::trusted_root::TrustedRootSaid;

/// The path CI posts the desired state to.
pub const DESIRED_PATH: &str = "/v1/desired";

/// The path an operator reads the status from.
pub const STATUS_PATH: &str = "/v1/status";

/// The path CI reads whether the farm has converged from.
pub const CONVERGENCE_PATH: &str = "/v1/convergence";

/// The path an operator posts an action to.
pub const OPERATOR_PATH: &str = "/v1/operator";

/// The path anyone that reaches the private listener reads the coordinator's own version from.
pub const VERSION_PATH: &str = "/v1/version";

/// The most bytes of a desired state read.
pub const MAX_DESIRED_BYTES: usize = 4096;

/// The most bytes of an operator action read.
pub const MAX_OPERATOR_BYTES: usize = 4096;

/// The most bytes of a `Releases.SetDesired` request read, on the gRPC route and the JSON
/// gateway: a set with targets carries up to eight entries of 24 KiB on each platform
/// (docs/design/packages.md, section 3.5). Every other operator method reads
/// [`MAX_OPERATOR_BYTES`].
pub const MAX_SET_DESIRED_BYTES: usize = 256 * 1024;

/// The error code of a heartbeat from a host an operator denied (`COORDINATOR_REFUSAL_DENIED`).
pub const DENIED: &str = "denied";

/// Which listener a request came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The mutual-TLS listener of the updaters, and the LAN listener of `--lan-listen`, which
    /// serves the same to network-installed hosts.
    Heartbeat,
    /// The private listener: the listener of CI and the operator.
    Control,
}

/// The leaf of the client certificate a connection presented, DER.
pub type Peer = Option<Arc<[u8]>>;

type Answer = Response<Full<Bytes>>;

/// The error of a request body.
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn json<T: Serialize>(status: StatusCode, value: &T) -> Answer {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
}

/// An answer of `status` whose body is `{"error":"<code>"}`.
pub(crate) fn error(status: StatusCode, code: &'static str) -> Answer {
    json(status, &serde_json::json!({ "error": code }))
}

/// The bearer token in the `Authorization` header of `headers`, if there is one.
pub(crate) fn bearer_in(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn bearer<B>(request: &Request<B>) -> Option<&str> {
    bearer_in(request.headers())
}

/// The body of `request`, if it is at most `limit` bytes and all of it arrives within
/// `patience`.
async fn read<B>(request: Request<B>, limit: usize, patience: Duration) -> Option<Bytes>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    read_body(request.into_body(), limit, patience).await
}

/// `body`, if it is at most `limit` bytes and all of it arrives within `patience`.
pub(crate) async fn read_body<B>(body: B, limit: usize, patience: Duration) -> Option<Bytes>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let body = Limited::new(body, limit);
    tokio::time::timeout(patience, body.collect())
        .await
        .ok()?
        .ok()
        .map(http_body_util::Collected::to_bytes)
}

/// Answers one request that came in on `surface` from `peer`. A body that has not arrived
/// whole after `patience` is a bad request.
pub async fn handle<B>(
    coordinator: &Coordinator,
    surface: Surface,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if surface == Surface::Control
        && let Some(ui) = coordinator.ui()
        && let Some(rest) = path.strip_prefix(crate::ui::PREFIX)
    {
        return ui.answer(&method, rest).await;
    }
    match (surface, method, path.as_str()) {
        (Surface::Heartbeat, Method::POST, HEARTBEAT_PATH) => {
            heartbeat(coordinator, peer, request, patience).await
        }
        (Surface::Control, Method::POST, DESIRED_PATH) => {
            desired(coordinator, request, patience).await
        }
        (Surface::Control, Method::POST, OPERATOR_PATH) => {
            operator(coordinator, request, patience).await
        }
        (Surface::Control, Method::GET, STATUS_PATH) => {
            match coordinator.status(bearer(&request)) {
                Ok(status) => json(StatusCode::OK, &status),
                Err(_) => error(StatusCode::UNAUTHORIZED, "unauthorized"),
            }
        }
        (Surface::Control, Method::GET, VERSION_PATH) => {
            crate::version_route::answer(request.uri().query())
        }
        (Surface::Control, Method::GET, CONVERGENCE_PATH) => {
            let Some(service) = service_of(request.uri().query(), coordinator.legacy_service())
            else {
                return error(StatusCode::BAD_REQUEST, "bad_request");
            };
            match coordinator.convergence(bearer(&request), service).await {
                Ok(convergence) => json(StatusCode::OK, &convergence),
                Err(ConvergenceError::Refused(CiRefused::Unauthorized(_))) => {
                    error(StatusCode::UNAUTHORIZED, "unauthorized")
                }
                Err(ConvergenceError::Refused(CiRefused::Unavailable)) => {
                    error(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
                }
                Err(ConvergenceError::UnknownService) => error(StatusCode::NOT_FOUND, "not_found"),
            }
        }
        (Surface::Heartbeat, _, HEARTBEAT_PATH)
        | (
            Surface::Control,
            _,
            DESIRED_PATH | OPERATOR_PATH | STATUS_PATH | CONVERGENCE_PATH | VERSION_PATH,
        ) => {
            error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        }
        _ => error(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// The service a convergence read asks for: the `legacy` service with no query (none if there
/// is none), else the query must be exactly `service=<name>` (service names need no escaping).
fn service_of<'a>(query: Option<&'a str>, legacy: Option<&'a str>) -> Option<&'a str> {
    match query {
        None => legacy,
        Some(query) => query.strip_prefix("service=").filter(|n| !n.contains('&')),
    }
}

/// The host a heartbeat comes from, as its client certificate and its deny row say.
#[derive(Clone, Debug)]
pub(crate) struct BeatCaller {
    /// The host the certificate names, with its role.
    pub identity: Identity,
    /// Whether the host retires: its heartbeat is heard for its final report only.
    pub retiring: bool,
}

/// Why a heartbeat was not answered. Both transports say it in their own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BeatRefused {
    /// No client certificate, or one that names no updater.
    Forbidden,
    /// The host is denied, or retiring and heard for its final report only.
    Denied,
    /// The heartbeat is not one the coordinator reads, or lists no service it can attribute.
    BadRequest,
}

/// The caller of a heartbeat from `peer`: the host its certificate names, and whether it
/// retires.
///
/// # Errors
/// [`BeatRefused::Forbidden`] for no certificate or one that is no updater's;
/// [`BeatRefused::Denied`] for a host whose deny row is final.
pub(crate) fn beat_caller(
    coordinator: &Coordinator,
    peer: &Peer,
) -> Result<BeatCaller, BeatRefused> {
    let identity = match peer.as_deref().map(|der| coordinator.identify(der)) {
        Some(Ok(identity)) => identity,
        Some(Err(why)) => {
            tracing::warn!(%why, "refused a heartbeat: the certificate is no updater's");
            return Err(BeatRefused::Forbidden);
        }
        None => {
            tracing::warn!("refused a heartbeat: no client certificate");
            return Err(BeatRefused::Forbidden);
        }
    };
    let retiring = match coordinator.admission(&identity) {
        Admission::Admitted => false,
        Admission::Retiring => true,
        Admission::Denied => {
            tracing::warn!(node_id = %identity.host_id, "refused a heartbeat: the host is denied");
            return Err(BeatRefused::Denied);
        }
    };
    Ok(BeatCaller { identity, retiring })
}

/// Reads the heartbeat in `bytes` (the JSON of `hostkeeper_wire::wire`) from `caller` and
/// answers it: the one path both the JSON route and the gRPC `Heartbeat` take once the body is
/// read. The optional facts and the `services` list that were bad are counted and logged and
/// the rest is answered. A retiring host is heard for its final report and then refused. With
/// `typed`, what the gRPC heartbeat said of the host's trusted root, the operator's order for the
/// host, or the update as a typed order, comes beside the answer
/// ([`Coordinator::heartbeat_with_orders`]); the JSON route passes none.
///
/// # Errors
/// [`BeatRefused::BadRequest`] for bytes that are no heartbeat, or one that lists no service
/// and has no legacy service to stand for; [`BeatRefused::Denied`] for a retiring host.
pub(crate) fn beat(
    coordinator: &Coordinator,
    caller: BeatCaller,
    bytes: &[u8],
    typed: Option<TrustedRootSaid>,
) -> Result<(WireAnswer, Option<Order>), BeatRefused> {
    let BeatCaller { identity, retiring } = caller;
    let HeartbeatRead {
        beat,
        services_dropped,
        extras_dropped,
        extras_dropped_facts,
    } = read_heartbeat(bytes, coordinator.legacy_service()).map_err(|_| BeatRefused::BadRequest)?;
    if let Some(why) = services_dropped {
        coordinator.note_services_dropped(&identity, &why);
    }
    if let Some(why) = extras_dropped {
        coordinator.note_extras_dropped(&identity, &why, &extras_dropped_facts);
    }
    if retiring {
        // Heard for its final report only: a slot it held is freed once the report is
        // final, and the host is refused all the same.
        coordinator.final_report(identity, beat);
        return Err(BeatRefused::Denied);
    }
    let answered = if let Some(root) = typed {
        coordinator.heartbeat_with_orders(identity, beat, root)
    } else {
        coordinator
            .heartbeat(identity, beat)
            .map(|answer| (answer, None))
    };
    answered.map_err(|why| {
        tracing::warn!("refused a heartbeat: {why}");
        BeatRefused::BadRequest
    })
}

/// The JSON route's answer to a refused heartbeat.
fn beat_refusal(why: BeatRefused) -> Answer {
    match why {
        BeatRefused::Forbidden => error(StatusCode::FORBIDDEN, "forbidden"),
        BeatRefused::Denied => error(StatusCode::FORBIDDEN, DENIED),
        BeatRefused::BadRequest => error(StatusCode::BAD_REQUEST, "bad_request"),
    }
}

async fn heartbeat<B>(
    coordinator: &Coordinator,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let caller = match beat_caller(coordinator, peer) {
        Ok(caller) => caller,
        Err(why) => return beat_refusal(why),
    };
    let Some(bytes) = read(request, MAX_HEARTBEAT_BYTES, patience).await else {
        return beat_refusal(BeatRefused::BadRequest);
    };
    match beat(coordinator, caller, &bytes, None) {
        Ok((answer, _)) => json(StatusCode::OK, &answer),
        Err(why) => beat_refusal(why),
    }
}

async fn desired<B>(coordinator: &Coordinator, request: Request<B>, patience: Duration) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let token = bearer(&request).map(str::to_owned);
    let Some(body) = read(request, MAX_DESIRED_BYTES, patience).await else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    match coordinator
        .set_desired_noting(token.as_deref(), &body)
        .await
    {
        Ok(set) => json(StatusCode::OK, &set),
        Err(SetError::Unauthorized(_)) => error(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(SetError::Unavailable) => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        Err(SetError::Forbidden) => error(StatusCode::FORBIDDEN, "forbidden"),
        Err(
            why
            @ (SetError::Invalid(_) | SetError::UnparseableVersion | SetError::TargetInvalid(_)),
        ) => error(
            StatusCode::BAD_REQUEST,
            why.reason().unwrap_or("bad_request"),
        ),
        Err(
            why @ (SetError::BelowMinVersion
            | SetError::BadVersion
            | SetError::DigestConflict
            | SetError::ReleaseExists
            | SetError::Channel(_)),
        ) => {
            tracing::warn!(%why, "refused a desired state");
            error(StatusCode::CONFLICT, why.reason().unwrap_or("conflict"))
        }
        Err(why @ (SetError::Store(_) | SetError::Ops(_))) => {
            tracing::error!(%why, "the desired state could not be stored");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

async fn operator<B>(coordinator: &Coordinator, request: Request<B>, patience: Duration) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    // The token first: a caller without it costs no read of the body and learns nothing from
    // its size or its stalling.
    let token = bearer(&request).map(str::to_owned);
    if coordinator.operator(token.as_deref()).is_err() {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Some(body) = read(request, MAX_OPERATOR_BYTES, patience).await else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    match coordinator.operate(token.as_deref(), &body) {
        Ok(entry) => json(StatusCode::OK, &entry),
        Err(OperateError::Unauthorized(_)) => error(StatusCode::UNAUTHORIZED, "unauthorized"),
        Err(why @ (OperateError::Invalid(_) | OperateError::Capped { .. })) => {
            tracing::warn!(%why, "refused an operator action");
            error(StatusCode::BAD_REQUEST, "bad_request")
        }
        Err(OperateError::Store(why)) => {
            tracing::error!(%why, "the ops file could not be written");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}
