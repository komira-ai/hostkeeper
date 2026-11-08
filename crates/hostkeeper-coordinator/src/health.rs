//! The health listener of `serve --health-listen` (`docs/design/two-programs.md`, section 2.7):
//! plain HTTP/1.1 on a loopback address, with no TLS and no token, for a probe on the
//! coordinator's own machine (an orchestrator's HTTP probe, the machine's monitoring). The image
//! has no shell and no client, so the probe runs outside the container, and there is no verb.
//!
//! It answers `GET /healthz` and `HEAD /healthz` only, and `404 not_found` to any other path or
//! method. The answer is `200` with the body `ok` when every listener the flags name is bound
//! and serving and the store takes writes, and `503` with one reason token otherwise:
//! `starting` until every listener is bound, `not_writer` once another coordinator led the
//! database this one's link to it was lost from (this one holds the single-writer lock no more,
//! and stops), `store_unreachable` while the link to the database is lost. A `HEAD` is answered
//! the status and the body's length, with no body. The body is the token and nothing else: no version, no host, no
//! count. An answer reads memory only and writes no audit line.
//!
//! The listener has the limits of the others ([`crate::listen`]): at most
//! [`crate::listen::MAX_CONNECTIONS`] connections at once, the patience for a request head and
//! for each write of an answer the peer does not read. A connection answers one request and is
//! closed.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

use crate::listen::{WriteDeadline, accept_each};
use crate::service::Coordinator;
use crate::store::Health;

/// The one path the health listener answers.
pub(crate) const HEALTH_PATH: &str = "/healthz";

/// What the health listener says of the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every listener is bound and serving, and the store takes writes: `200 ok`.
    Ok,
    /// Not every listener the flags name is bound yet: `503 starting`.
    Starting,
    /// Another coordinator led the database while this one's link was lost: this one holds the
    /// single-writer lock no more, and stops. `503 not_writer`.
    NotWriter,
    /// The link to the database is lost. `503 store_unreachable`.
    StoreUnreachable,
}

impl Verdict {
    /// The body of the answer: `ok`, or the reason token.
    #[must_use]
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Starting => "starting",
            Self::NotWriter => "not_writer",
            Self::StoreUnreachable => "store_unreachable",
        }
    }

    /// The status of the answer: `200` for [`Verdict::Ok`], `503` for any other.
    #[must_use]
    pub(crate) fn status(self) -> StatusCode {
        match self {
            Self::Ok => StatusCode::OK,
            Self::Starting | Self::NotWriter | Self::StoreUnreachable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }
}

/// Whether every listener is bound and serving: set once by the start, after the last bind,
/// and read by every answer of the health listener.
#[derive(Clone, Debug, Default)]
pub(crate) struct Serving(Arc<AtomicBool>);

impl Serving {
    /// Says that every listener is bound and serving.
    pub(crate) fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`Serving::set`] was called.
    #[must_use]
    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What the health listener says of `coordinator`, `serving` or not: [`Verdict::Starting`]
/// first, then the store's health, from memory.
#[must_use]
pub(crate) fn verdict(coordinator: &Coordinator, serving: &Serving) -> Verdict {
    if !serving.is_set() {
        return Verdict::Starting;
    }
    match coordinator.ops.health() {
        Health::Up => Verdict::Ok,
        Health::Lost => Verdict::StoreUnreachable,
        Health::Deposed { .. } => Verdict::NotWriter,
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
    let asks = path == HEALTH_PATH && (method == Method::GET || method == Method::HEAD);
    let (status, body) = if asks {
        let verdict = verdict(coordinator, serving);
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
    accept_each(
        accept,
        Arc::new(Semaphore::new(max_connections)),
        move |stream| connection(stream, Arc::clone(&coordinator), serving.clone(), patience),
    )
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
