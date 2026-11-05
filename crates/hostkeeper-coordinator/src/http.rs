//! The two HTTP surfaces, mapped onto [`Coordinator`].
//!
//! - **Heartbeat** (mutual TLS, updaters): `POST /v1/heartbeat` with the heartbeat as JSON;
//!   the answer is the reply as JSON. The caller is the certificate's updater identity
//!   ([`identity_of`]); a certificate that names none is refused with 403 before the body
//!   is read.
//! - **Private** (TLS, no client certificate; `Surface::Control`): the listener of
//!   `--private-listen`, which serves CI's OIDC calls and the operator's. `POST /v1/desired` with
//!   CI's OIDC token as the bearer and the desired state as JSON; `GET /v1/status` with an
//!   operator token as the bearer; `GET /v1/convergence[?service=<name>]` with CI's OIDC token as
//!   the bearer (an operator token is refused there; no query means the coordinator's legacy
//!   service, and is a bad request when there is none); `POST /v1/operator` with an operator
//!   token as the bearer and an action as JSON (`crate::ops`; CI's token is refused). An
//!   operator token is one of the named tokens ([`crate::tokens`]); its name is the `by` of the
//!   change. With `--ui-dir` it also answers `GET` and `HEAD` for `/ui/` and every path below
//!   from that directory ([`crate::ui`]), with no token read; without it those paths are not
//!   found. The heartbeat surface never serves them.
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
//! `below_min_version`, `bad_version` and `digest_conflict` (409). A heartbeat that lists no
//! services, at a coordinator with no legacy service, is a bad request.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_wire::wire::{HEARTBEAT_PATH, HeartbeatRead, MAX_HEARTBEAT_BYTES, read_heartbeat};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Body;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use serde::Serialize;

use crate::identity::identity_of;
use crate::ops::OperateError;
use crate::service::{CiRefused, ConvergenceError, Coordinator, SetError};

/// The path CI posts the desired state to.
pub const DESIRED_PATH: &str = "/v1/desired";

/// The path an operator reads the status from.
pub const STATUS_PATH: &str = "/v1/status";

/// The path CI reads whether the farm has converged from.
pub const CONVERGENCE_PATH: &str = "/v1/convergence";

/// The path an operator posts an action to.
pub const OPERATOR_PATH: &str = "/v1/operator";

/// The most bytes of a desired state read.
pub const MAX_DESIRED_BYTES: usize = 4096;

/// The most bytes of an operator action read.
pub const MAX_OPERATOR_BYTES: usize = 4096;

/// Which listener a request came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The mutual-TLS listener of the updaters.
    Heartbeat,
    /// The private listener: the listener of CI and the operator.
    Control,
}

/// The leaf of the client certificate a connection presented, DER.
pub type Peer = Option<Arc<[u8]>>;

type Answer = Response<Full<Bytes>>;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn json<T: Serialize>(status: StatusCode, value: &T) -> Answer {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_default()
}

fn error(status: StatusCode, code: &'static str) -> Answer {
    json(status, &serde_json::json!({ "error": code }))
}

fn bearer<B>(request: &Request<B>) -> Option<&str> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// The body of `request`, if it is at most `limit` bytes and all of it arrives within
/// `patience`.
async fn read<B>(request: Request<B>, limit: usize, patience: Duration) -> Option<Bytes>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let body = Limited::new(request.into_body(), limit);
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
        | (Surface::Control, _, DESIRED_PATH | OPERATOR_PATH | STATUS_PATH | CONVERGENCE_PATH) => {
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
    let identity = match peer
        .as_deref()
        .map(|der| identity_of(der, coordinator.prefixes()))
    {
        Some(Ok(identity)) => identity,
        Some(Err(why)) => {
            tracing::warn!(%why, "refused a heartbeat: the certificate is no updater's");
            return error(StatusCode::FORBIDDEN, "forbidden");
        }
        None => {
            tracing::warn!("refused a heartbeat: no client certificate");
            return error(StatusCode::FORBIDDEN, "forbidden");
        }
    };
    let beat = read(request, MAX_HEARTBEAT_BYTES, patience)
        .await
        .and_then(|bytes| read_heartbeat(&bytes, coordinator.legacy_service()).ok());
    match beat {
        Some(HeartbeatRead {
            beat,
            services_dropped,
            extras_dropped,
            extras_dropped_facts,
        }) => {
            if let Some(why) = services_dropped {
                coordinator.note_services_dropped(&identity, &why);
            }
            if let Some(why) = extras_dropped {
                coordinator.note_extras_dropped(&identity, &why, &extras_dropped_facts);
            }
            match coordinator.heartbeat(identity, beat) {
                Ok(answer) => json(StatusCode::OK, &answer),
                Err(why) => {
                    tracing::warn!("refused a heartbeat: {why}");
                    error(StatusCode::BAD_REQUEST, "bad_request")
                }
            }
        }
        None => error(StatusCode::BAD_REQUEST, "bad_request"),
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
        Err(SetError::Invalid(_)) => error(StatusCode::BAD_REQUEST, "bad_request"),
        Err(why @ SetError::UnparseableVersion) => error(
            StatusCode::BAD_REQUEST,
            why.reason().unwrap_or("bad_request"),
        ),
        Err(
            why @ (SetError::BelowMinVersion | SetError::BadVersion | SetError::DigestConflict),
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
