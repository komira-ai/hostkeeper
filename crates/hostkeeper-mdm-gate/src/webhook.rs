//! The listener NanoHUB's webhook posts to (`--mdm-webhook-listen`, on loopback).
//!
//! HTTP/1.1, one route: `POST /webhook`, the event as its body. NanoHUB is pointed at it with
//! `-webhook-url http://<listen address>/webhook`. The answers:
//!
//! - `200`: the event was taken, or passed over;
//! - `400`: the body is not an event the gate reads;
//! - `404`, `405`: another path, or another method;
//! - `413`: the body is over [`MAX_EVENT_BYTES`];
//! - `500`: the device's record could not be written.
//!
//! NanoHUB logs an answer other than `200` and goes on: it does not send the event again. A
//! connection that does not send its head, or its body, within [`PATIENCE`] is closed.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::event::{self, EventError};
use crate::gate::{Gate, Handled};
use crate::store::StoreError;

/// The largest event body taken. A `DeviceInformation` answer is a few kilobytes; a result of a
/// command another client of NanoHUB sent may be larger, and is passed over anyway.
pub const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// How long a connection may take to send its head and its body.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// The one path served.
pub const PATH: &str = "/webhook";

/// Whether `addr` may be the webhook's listen address: a loopback address.
#[must_use]
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().to_canonical().is_loopback()
}

/// Serves the webhook of `gate` on the connections `accept` gives, forever. A failed accept is
/// logged and the next is waited for after a tenth of a second.
pub async fn serve<A, F, S>(mut accept: A, gate: Arc<Gate>) -> Infallible
where
    A: FnMut() -> F,
    F: Future<Output = io::Result<S>>,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let stream = match accept().await {
            Ok(stream) => stream,
            Err(why) => {
                tracing::warn!(%why, "the MDM webhook listener could not accept a connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let gate = Arc::clone(&gate);
                async move { Ok::<_, Infallible>(answer(&gate, request).await) }
            });
            let served = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(PATIENCE)
                .serve_connection(TokioIo::new(stream), service)
                .await;
            if let Err(why) = served {
                tracing::debug!(%why, "an MDM webhook connection ended");
            }
        });
    }
}

/// Why a webhook body was not taken.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The body is not an event the gate reads.
    #[error(transparent)]
    Event(#[from] EventError),
    /// The device's record could not be written: the event is not taken, and NanoHUB is told.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reads the NanoHUB webhook `body` into an event and hands it to [`Gate::handle`]; a body of a
/// topic or channel the gate does not read is passed over.
///
/// # Errors
/// [`WebhookError`]: the body is no event, or the record cannot be written.
pub async fn take(gate: &Gate, body: &[u8]) -> Result<Handled, WebhookError> {
    match event::parse(body)? {
        Some(event) => Ok(gate.handle(event).await?),
        None => Ok(Handled::PassedOver),
    }
}

fn reply(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

/// The answer to one request.
async fn answer(gate: &Gate, request: Request<Incoming>) -> Response<Full<Bytes>> {
    if request.uri().path() != PATH {
        return reply(StatusCode::NOT_FOUND);
    }
    if request.method() != Method::POST {
        return reply(StatusCode::METHOD_NOT_ALLOWED);
    }
    let body = Limited::new(request.into_body(), MAX_EVENT_BYTES).collect();
    let body = match tokio::time::timeout(PATIENCE, body).await {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(why)) if why.is::<http_body_util::LengthLimitError>() => {
            tracing::warn!("an MDM webhook event over the size limit was refused");
            return reply(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Ok(Err(_)) | Err(_) => return reply(StatusCode::BAD_REQUEST),
    };
    match take(gate, &body).await {
        Ok(_) => reply(StatusCode::OK),
        Err(WebhookError::Event(why)) => {
            tracing::warn!(%why, "an MDM webhook event was refused");
            reply(StatusCode::BAD_REQUEST)
        }
        Err(WebhookError::Store(why)) => {
            tracing::error!(%why, "an MDM webhook event was not recorded");
            reply(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
