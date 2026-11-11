//! The port the ABM client speaks through: one request to ABM, one answer. Production is
//! [`HttpsTransport`], HTTP/1.1 over TLS to `https://mdmenrollment.apple.com`, one connection per
//! request; the tests' fake ([`super::fake`]) answers from a script, so that no test touches a
//! network (AGENTS.md T12).
//!
//! The port carries the bytes and the status alone. What a status means (a session that ran out,
//! a clock that is wrong, a cursor that is too old) is the client's to judge ([`super::client`]).

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use hostkeeper_oidc::{Dial, Roots};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::header::{CONNECTION, CONTENT_TYPE, HOST, RETRY_AFTER, USER_AGENT};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio_rustls::TlsConnector;

use crate::origin_https::{OriginTlsError, OriginUrl, connector, tcp_dial};

/// ABM's address.
pub(crate) const ABM_BASE: &str = "https://mdmenrollment.apple.com";

/// The most bytes of an answer that are read: a page of 1000 devices is well under a megabyte.
pub(crate) const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// One request to ABM.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AbmRequest {
    /// The method.
    pub(crate) method: Method,
    /// The path and query, beginning with `/`.
    pub(crate) path: String,
    /// The headers, by name. A value is a credential: the `Debug` of a request shows none.
    pub(crate) headers: Vec<(&'static str, String)>,
    /// The body, JSON, when the method takes one.
    pub(crate) body: Option<Vec<u8>>,
    /// How long the request may take, from the connection to the last byte of the answer.
    pub(crate) timeout: Duration,
}

impl fmt::Debug for AbmRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbmRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// ABM's answer: the status, the `Retry-After` it named in seconds, and the body.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AbmResponse {
    /// The HTTP status.
    pub(crate) status: u16,
    /// The seconds `Retry-After` names, when it is a number of seconds.
    pub(crate) retry_after: Option<u64>,
    /// The body.
    pub(crate) body: Vec<u8>,
}

impl fmt::Debug for AbmResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbmResponse")
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .finish_non_exhaustive()
    }
}

/// Why no answer came: the text names the step and never a value of the request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct TransportError(pub(crate) String);

/// The error of the step `what`, whatever the step's own error: no text of it is kept.
fn step<E>(what: &'static str) -> impl FnOnce(E) -> TransportError {
    move |_| TransportError(format!("ABM: {what} failed"))
}

/// Sends one request to ABM.
pub(crate) trait AbmTransport: Send + Sync {
    /// The answer to `request`.
    ///
    /// # Errors
    /// [`TransportError`]: the connection, the handshake, the request or the answer failed, or
    /// the time ran out.
    fn send(&self, request: AbmRequest) -> BoxFuture<'_, Result<AbmResponse, TransportError>>;
}

/// The production transport.
pub(crate) struct HttpsTransport {
    url: OriginUrl,
    connector: TlsConnector,
    dial: Dial,
    max_answer: usize,
}

impl HttpsTransport {
    /// A transport to `base` (`https://host[:port]`), trusting `roots`.
    ///
    /// # Errors
    /// [`OriginTlsError`]: the roots hold no usable certificate, or the TLS client is refused.
    pub(crate) fn new(base: &OriginUrl, roots: Roots) -> Result<Self, OriginTlsError> {
        Ok(Self {
            url: base.clone(),
            connector: connector(roots)?,
            dial: tcp_dial(),
            max_answer: MAX_ANSWER_BYTES,
        })
    }

    /// The same transport reading at most `bytes` of an answer (tests).
    #[cfg(test)]
    pub(crate) fn with_max_answer(mut self, bytes: usize) -> Self {
        self.max_answer = bytes;
        self
    }

    /// The same transport dialling through `dial` (tests).
    #[cfg(test)]
    pub(crate) fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    async fn exchange(&self, request: AbmRequest) -> Result<AbmResponse, TransportError> {
        let tcp = (self.dial)(self.url.host().to_owned(), self.url.port())
            .await
            .map_err(step("the connection"))?;
        let tls = self
            .connector
            .connect(self.url.server(), tcp)
            .await
            .map_err(step("the TLS handshake"))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls))
                .await
                .map_err(step("the HTTP handshake"))?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut builder = Request::builder()
            .method(request.method)
            .uri(self.url.target(request.path.trim_start_matches('/')))
            .header(HOST, self.url.authority())
            .header(USER_AGENT, "hostkeeper-coordinator")
            .header(CONNECTION, "close");
        if request.body.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/json;charset=UTF8");
        }
        for (name, value) in request.headers {
            let mut value =
                hyper::header::HeaderValue::from_str(&value).map_err(step("a header value"))?;
            value.set_sensitive(true);
            builder = builder.header(name, value);
        }
        let body = Full::new(Bytes::from(request.body.unwrap_or_default()));
        let request = builder.body(body).map_err(step("the request"))?;
        let outcome = async {
            let response = sender
                .send_request(request)
                .await
                .map_err(step("the request"))?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let body = Limited::new(response.into_body(), self.max_answer)
                .collect()
                .await
                .map_err(step("reading the answer"))?
                .to_bytes();
            Ok(AbmResponse {
                status,
                retry_after,
                body: body.to_vec(),
            })
        }
        .await;
        driver.abort();
        outcome
    }
}

impl AbmTransport for HttpsTransport {
    fn send(&self, request: AbmRequest) -> BoxFuture<'_, Result<AbmResponse, TransportError>> {
        Box::pin(async move {
            let limit = request.timeout;
            tokio::time::timeout(limit, self.exchange(request))
                .await
                .unwrap_or_else(|_| Err(TransportError("ABM: the request timed out".to_owned())))
        })
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
