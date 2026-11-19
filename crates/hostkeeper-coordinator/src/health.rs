//! The health listener of `serve --health-listen` (`docs/design/two-programs.md`, section 2.7,
//! and `docs/design/coordinator-multi-instance.md`, section 11): plain HTTP/1.1 with no TLS and no
//! token, for a probe on the coordinator's own machine (an orchestrator's HTTP probe, the
//! machine's monitoring) on a loopback address, or for the edge's check on the address of one
//! interface (a wildcard with `--health-listen-wildcard`). The image has no shell and no client,
//! so the probe runs outside the container, and there is no verb.
//!
//! It answers `GET` and `HEAD` of `/healthz` and of `/readyz`, and `404 not_found` to any other
//! path or method; a `HEAD` is answered the status and the body's length, with no body. The body
//! is the token and nothing else: no version, no host, no count. An answer reads memory only and
//! writes no audit line.
//!
//! `/readyz` is whether this coordinator takes traffic, which the edge checks: `200 ok` when every
//! listener the flags name is bound and serving and the store takes writes, and `503` with one
//! reason token otherwise: `draining` from the stop signal until the process exits, whatever else
//! holds, `starting` until every listener is bound, `store_unreachable` while the link to the
//! database is lost.
//!
//! `/healthz` is whether the process is alive, which the orchestrator checks: `200 ok` from the
//! moment every listener is bound, whatever the database does and while the coordinator drains, and
//! `503 starting` before. A database that does not answer, or a stop in progress, must not have
//! the orchestrator kill a coordinator that is doing what it should.
//!
//! The health listener is the one listener the stop does not close: after `--drain-delay-secs` the
//! heartbeat, private and LAN listeners stop accepting and the coordinator waits for the
//! connections they hold ([`crate::graceful`]), and this listener answers `/healthz` `200` and
//! `/readyz` `503 draining` meanwhile, until the process exits. Its own connections are not
//! waited for.
//!
//! The listener has the limits of the others ([`crate::listen`]): at most
//! [`crate::listen::MAX_CONNECTIONS`] connections at once, the patience for a request head and
//! for each write of an answer the peer does not read. A connection answers one request and is
//! closed.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Semaphore;

use crate::graceful::Graceful;
use crate::listen::{WriteDeadline, accept_each};
use crate::service::Coordinator;
use crate::store::Health;

/// The path of the liveness check: `200` once every listener is bound, whatever else holds.
pub(crate) const HEALTH_PATH: &str = "/healthz";

/// The path of the readiness check: `200` only while this coordinator takes traffic, `503` while
/// it starts, cannot reach its store or drains, so the edge that checks it sends a connection only
/// to a coordinator that answers `200`.
pub(crate) const READY_PATH: &str = "/readyz";

/// What the health listener says of the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every listener is bound and serving, and the store takes writes: `200 ok`.
    Ok,
    /// Not every listener the flags name is bound yet: `503 starting`.
    Starting,
    /// The link to the database is lost. `503 store_unreachable`.
    StoreUnreachable,
    /// The coordinator was told to stop and is on its way out: a load balancer sends it no new
    /// connection. `503 draining`, from the stop signal until the process exits.
    Draining,
}

impl Verdict {
    /// The body of the answer: `ok`, or the reason token.
    #[must_use]
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Starting => "starting",
            Self::StoreUnreachable => "store_unreachable",
            Self::Draining => "draining",
        }
    }

    /// The status of the answer: `200` for [`Verdict::Ok`], `503` for any other.
    #[must_use]
    pub(crate) fn status(self) -> StatusCode {
        match self {
            Self::Ok => StatusCode::OK,
            Self::Starting | Self::StoreUnreachable | Self::Draining => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }
}

/// Starting: not every listener is bound yet.
const STARTING: u8 = 0;
/// Every listener is bound and serving.
const SERVING: u8 = 1;
/// Told to stop.
const DRAINING: u8 = 2;

/// Where the coordinator is in its life, read by every answer of the health listener: starting,
/// then serving once the start calls [`Serving::set`] after the last bind, then draining once
/// the stop signal calls [`Serving::drain`]. It only moves forward: a set after a drain leaves
/// it draining.
#[derive(Clone, Debug, Default)]
pub(crate) struct Serving(Arc<AtomicU8>);

impl Serving {
    /// Says that every listener is bound and serving, unless the coordinator is draining.
    pub(crate) fn set(&self) {
        self.0.fetch_max(SERVING, Ordering::SeqCst);
    }

    /// Says that the coordinator was told to stop: from now on it is draining.
    pub(crate) fn drain(&self) {
        self.0.store(DRAINING, Ordering::SeqCst);
    }

    /// Whether [`Serving::set`] or [`Serving::drain`] was called.
    #[must_use]
    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst) != STARTING
    }

    /// Whether [`Serving::drain`] was called.
    #[must_use]
    pub(crate) fn is_draining(&self) -> bool {
        self.0.load(Ordering::SeqCst) == DRAINING
    }
}

/// Waits for `stop` (the stop signal), then says on `serving` that the coordinator is
/// draining, whatever `stop` answered, waits for `delay` and answers what `stop` answered. The
/// health listener says `draining` from the stop until the process exits, and the coordinator's
/// listeners keep accepting while `delay` runs, so an edge that has not yet seen the `503` (or the
/// new instance that replaces this one) still reaches a coordinator that answers.
pub(crate) async fn drain_on<T>(
    stop: impl Future<Output = T>,
    serving: Serving,
    delay: impl Future<Output = ()>,
) -> T {
    let stopped = stop.await;
    serving.drain();
    delay.await;
    stopped
}

/// What the liveness check says of the process: [`Verdict::Starting`] until every listener is
/// bound, and [`Verdict::Ok`] after, whatever the store and a drain say.
#[must_use]
pub(crate) fn liveness(serving: &Serving) -> Verdict {
    if serving.is_set() {
        Verdict::Ok
    } else {
        Verdict::Starting
    }
}

/// What the readiness check says of `coordinator`, `serving` or not: [`Verdict::Draining`]
/// first, then [`Verdict::Starting`], then the store's health, from memory.
#[must_use]
pub(crate) fn verdict(coordinator: &Coordinator, serving: &Serving) -> Verdict {
    if serving.is_draining() {
        return Verdict::Draining;
    }
    if !serving.is_set() {
        return Verdict::Starting;
    }
    match coordinator.ops.health() {
        Health::Up => Verdict::Ok,
        Health::Lost => Verdict::StoreUnreachable,
    }
}

/// The answer to a request of `method` for `path` (the path alone, with no query): the
/// verdict for `GET` and `HEAD` of [`HEALTH_PATH`], and `404 not_found` for anything else. A
/// `HEAD` is answered as a `GET`: the connection sends its head, with the `content-length` of the
/// body, and leaves the body out.
#[must_use]
pub(crate) fn answer(
    coordinator: &Coordinator,
    serving: &Serving,
    method: &Method,
    path: &str,
) -> Response<Full<Bytes>> {
    let asks = (path == HEALTH_PATH || path == READY_PATH)
        && (method == Method::GET || method == Method::HEAD);
    let (status, body) = if asks {
        let verdict = if path == HEALTH_PATH {
            liveness(serving)
        } else {
            verdict(coordinator, serving)
        };
        (verdict.status(), verdict.token())
    } else {
        (StatusCode::NOT_FOUND, "not_found")
    };
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        CACHE_CONTROL,
        hyper::header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Serves one plain HTTP/1.1 connection of the health listener: one request, answered by
/// [`answer`], with `patience` for its head and for each write of the answer; then it is
/// closed.
pub(crate) async fn connection<S>(
    stream: S,
    coordinator: Arc<Coordinator>,
    serving: Serving,
    patience: Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
        let response = answer(
            &coordinator,
            &serving,
            request.method(),
            request.uri().path(),
        );
        async move { Ok::<_, Infallible>(response) }
    });
    let served = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(patience)
        .keep_alive(false)
        .serve_connection(TokioIo::new(WriteDeadline::new(stream, patience)), service)
        .await;
    if let Err(why) = served {
        tracing::debug!(%why, "a health connection ended badly");
    }
}

/// Serves the health listener whose connections `accept` yields, for `coordinator`, for as
/// long as the future is polled, at most `max_connections` at once. It never returns.
pub(crate) fn serve<A, F, S>(
    accept: A,
    coordinator: Arc<Coordinator>,
    serving: Serving,
    patience: Duration,
    max_connections: usize,
) -> impl Future<Output = Infallible>
where
    A: FnMut() -> F + Send,
    F: Future<Output = io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Its connections are counted on a `Graceful` of its own that nothing stops: the health
    // listener answers until the process exits, and the stop does not wait for its connections.
    accept_each(
        accept,
        Arc::new(Semaphore::new(max_connections)),
        Graceful::default(),
        move |stream, _| connection(stream, Arc::clone(&coordinator), serving.clone(), patience),
    )
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
