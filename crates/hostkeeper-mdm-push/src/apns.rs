//! The APNs sender (`docs/design/coordinator-mdm.md`, section 4.3; the spike `apns.md`).
//!
//! `POST https://api.push.apple.com/3/device/<64 hex digits>` over HTTP/2 with the push
//! certificate as the TLS client identity, the headers `apns-topic` (the certificate's topic),
//! `apns-push-type: mdm`, `apns-priority` and `apns-expiration`, and the body
//! `{"mdm":"<PushMagic>"}`. Success is `200` with an empty body and an `apns-id` header.
//!
//! One connection is kept and reused for sequential requests. When the cached connection has been
//! closed (APNs sent a GOAWAY, or the peer went away) the next request connects again, and a
//! request that failed on a reused connection before any answer is sent once more on a new one:
//! a push is safe to repeat.
//!
//! What APNs answers maps to [`PushError`] as the spike measured it: `400 BadDeviceToken` is
//! terminal for that token (an unknown token gives `400`, not `410`), `400 TopicDisallowed` is a
//! setup mistake (the topic is not the certificate's), a certificate APNs refuses by environment
//! arrives as an HTTP/2 GOAWAY whose payload names `BadCertificateEnvironment` and no response,
//! and `403 MissingProviderToken` is what a missing client certificate gives, not an expiry
//! signal. Another `403` is taken as a certificate APNs refuses, which is **unverified**: nothing
//! read says what an expired or revoked certificate gets. `410`, `429`, `500` and `503` are
//! handled as Apple documents them and were not provoked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::client::conn::http2::SendRequest;
use hyper::header::HeaderValue;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls::RootCertStore;
use tokio::sync::Mutex;

use crate::BoxFuture;
use crate::identity::PushIdentity;
use crate::tls::{self, Endpoint, TlsError};
use crate::token::DeviceToken;

/// The host of APNs.
pub const PRODUCTION_HOST: &str = "api.push.apple.com";

/// The port of APNs.
pub const PRODUCTION_PORT: u16 = 443;

/// How much of an error answer is read.
const MAX_ANSWER_BYTES: usize = 4096;

/// One push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Push {
    /// The device's token.
    pub token: DeviceToken,
    /// The `PushMagic` the device sent in its `TokenUpdate`.
    pub magic: String,
    /// The topic of the push certificate.
    pub topic: String,
    /// `apns-priority`: 10, or 5.
    pub priority: u8,
    /// `apns-expiration`: the unix time until which APNs keeps the push, or 0 to try once.
    pub expiration_unix: u64,
}

/// What APNs said of a push it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivered {
    /// The `apns-id` header, when it came.
    pub apns_id: Option<String>,
}

/// Why a push was not delivered to APNs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    /// `400 BadDeviceToken` (or the token does not belong to the topic): terminal for the token.
    #[error("APNs refuses the device token: {0}")]
    BadDeviceToken(String),
    /// `410`: the device token is no longer active.
    #[error("APNs says the device token is no longer registered")]
    Unregistered,
    /// `400 TopicDisallowed`: the topic is not the one the certificate may push to. A setup
    /// mistake (the wrong topic or certificate), the same for every device.
    #[error("APNs refuses the topic: the certificate and the topic do not belong together")]
    TopicDisallowed,
    /// `403 MissingProviderToken`: APNs saw no client certificate.
    #[error("APNs saw no client certificate")]
    NoClientCertificate,
    /// Another `403`: APNs refuses the certificate. Raises the push-certificate alert; what an
    /// expired or revoked certificate gets is not verified.
    #[error("APNs refuses the push certificate: {0}")]
    CertificateRejected(String),
    /// The connection ended with a certificate alert: a TLS alert, or a GOAWAY whose payload says
    /// the certificate is for another environment (`BadCertificateEnvironment`).
    #[error("the push certificate is refused at the connection: {0}")]
    CertificateAlert(String),
    /// `429`: too many requests for the device or the topic.
    #[error("APNs says too many pushes")]
    Throttled,
    /// `500` or `503`: APNs is not able to take it now.
    #[error("APNs is unavailable ({0})")]
    Unavailable(u16),
    /// Any other answer.
    #[error("APNs answered {status}: {reason}")]
    Rejected {
        /// The status.
        status: u16,
        /// The `reason` of the body, or empty.
        reason: String,
    },
    /// The connection could not be made, broke, or the answer did not come.
    #[error("the connection to APNs failed: {0}")]
    Connection(String),
}

impl PushError {
    /// Whether the token is dead: stop pushing it until the next `TokenUpdate` replaces it.
    #[must_use]
    pub fn token_is_dead(&self) -> bool {
        matches!(self, Self::BadDeviceToken(_) | Self::Unregistered)
    }

    /// Whether the setup is wrong (the topic or the certificate), for every device.
    #[must_use]
    pub fn is_setup_mistake(&self) -> bool {
        matches!(self, Self::TopicDisallowed | Self::NoClientCertificate)
    }

    /// Whether the push-certificate alert is raised: APNs or the connection refused the
    /// certificate.
    #[must_use]
    pub fn is_certificate_alert(&self) -> bool {
        matches!(
            self,
            Self::CertificateRejected(_) | Self::CertificateAlert(_)
        )
    }

    /// Whether a later try can succeed with nothing changed: the window is the floor.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Throttled | Self::Unavailable(_) | Self::Connection(_)
        )
    }
}

/// The port the push policy's callers send through: the real client, or a fake.
pub trait PushSender: Send + Sync {
    /// Sends `push` to APNs.
    fn send<'a>(&'a self, push: &'a Push) -> BoxFuture<'a, Result<Delivered, PushError>>;
}

/// The HTTP/2 client of APNs.
pub struct Http2Sender {
    endpoint: Endpoint,
    config: Arc<ClientConfig>,
    timeout: Duration,
    connection: Mutex<Option<SendRequest<Full<Bytes>>>>,
}

impl std::fmt::Debug for Http2Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http2Sender")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Http2Sender {
    /// The sender to `endpoint` that trusts `roots` and presents `identity`, giving up on a
    /// connection or an answer after `timeout`.
    ///
    /// # Errors
    /// [`TlsError`]: the identity is not one rustls signs with.
    pub fn new(
        endpoint: Endpoint,
        roots: RootCertStore,
        identity: &PushIdentity,
        timeout: Duration,
    ) -> Result<Self, TlsError> {
        let config = tls::client_config(roots, Some(identity.tls_identity()), &[b"h2"])?;
        Ok(Self {
            endpoint,
            config,
            timeout,
            connection: Mutex::new(None),
        })
    }

    /// The sender of the real APNs, trusting the public web's roots.
    ///
    /// # Errors
    /// [`TlsError`].
    pub fn production(identity: &PushIdentity, timeout: Duration) -> Result<Self, TlsError> {
        Self::new(
            Endpoint::new(PRODUCTION_HOST, PRODUCTION_PORT),
            tls::public_roots(),
            identity,
            timeout,
        )
    }

    /// A connection that can take a request: the cached one if it is open, else a new one.
    /// `true` when it is new.
    async fn ready(&self) -> Result<(SendRequest<Full<Bytes>>, bool), PushError> {
        let mut held = self.connection.lock().await;
        if let Some(open) = held.as_mut() {
            if open.ready().await.is_ok() {
                return Ok((open.clone(), false));
            }
            *held = None;
        }
        let stream = tls::connect(&self.endpoint, self.config.clone(), self.timeout)
            .await
            .map_err(connection_failed)?;
        let (sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await
                .map_err(connection_failed)?;
        tokio::spawn(async move {
            // The connection ends by GOAWAY or by the peer; the next request connects again.
            drop(connection.await);
        });
        *held = Some(sender.clone());
        Ok((sender, true))
    }

    /// Sends the request once on `sender` and reads the answer.
    async fn exchange(
        &self,
        mut sender: SendRequest<Full<Bytes>>,
        request: Request<Full<Bytes>>,
    ) -> Result<Result<Delivered, PushError>, hyper::Error> {
        let answer = sender.send_request(request).await?;
        let (parts, body) = answer.into_parts();
        let apns_id = parts
            .headers
            .get("apns-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if parts.status == StatusCode::OK {
            return Ok(Ok(Delivered { apns_id }));
        }
        let body = Limited::new(body, MAX_ANSWER_BYTES)
            .collect()
            .await
            .map(http_body_util::Collected::to_bytes)
            .unwrap_or_default();
        Ok(Err(map_answer(parts.status.as_u16(), &body)))
    }

    async fn deliver(&self, push: &Push) -> Result<Delivered, PushError> {
        let request = request_of(push, &self.endpoint)?;
        let (sender, fresh) = self.ready().await?;
        match self.exchange(sender, request.clone()).await {
            Ok(answer) => answer,
            Err(why) => {
                // Whatever went wrong, the connection is not kept.
                *self.connection.lock().await = None;
                if fresh {
                    return Err(connection_failed(why));
                }
                // The connection kept from the last push is gone, and no answer came: a push is
                // safe to repeat, so it is sent once more, on a new connection.
                let (sender, _) = self.ready().await?;
                self.exchange(sender, request)
                    .await
                    .unwrap_or_else(|why| Err(connection_failed(why)))
            }
        }
    }
}

impl PushSender for Http2Sender {
    fn send<'a>(&'a self, push: &'a Push) -> BoxFuture<'a, Result<Delivered, PushError>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.deliver(push))
                .await
                .unwrap_or_else(|_| Err(PushError::Connection("the answer timed out".to_owned())))
        })
    }
}

/// The request of `push` to `endpoint`.
fn request_of(push: &Push, endpoint: &Endpoint) -> Result<Request<Full<Bytes>>, PushError> {
    // A string in a fixed shape: it always serializes.
    let body = serde_json::to_vec(&serde_json::json!({ "mdm": push.magic })).unwrap_or_default();
    let header = |text: &str| HeaderValue::from_str(text).map_err(invalid);
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "https://{}/3/device/{}",
            endpoint.server_name,
            push.token.to_hex()
        ))
        .body(Full::new(Bytes::from(body)))
        .map_err(invalid)?;
    let headers = request.headers_mut();
    headers.insert("apns-topic", header(&push.topic)?);
    headers.insert("apns-push-type", HeaderValue::from_static("mdm"));
    headers.insert("apns-priority", header(&push.priority.to_string())?);
    headers.insert(
        "apns-expiration",
        header(&push.expiration_unix.to_string())?,
    );
    Ok(request)
}

/// The error of a request that could not be made: a header value or a URL that is not valid.
fn invalid(why: impl std::fmt::Display) -> PushError {
    PushError::Connection(format!("the request is not valid: {why}"))
}

/// The error of a connection that failed.
fn connection_failed(why: impl std::error::Error + 'static) -> PushError {
    connection_error(&why)
}

/// The error for an answer other than `200`.
fn map_answer(status: u16, body: &[u8]) -> PushError {
    let reason = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("reason")
                .and_then(|r| r.as_str().map(str::to_owned))
        })
        .unwrap_or_default();
    match (status, reason.as_str()) {
        (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => PushError::BadDeviceToken(reason),
        (400, "TopicDisallowed") => PushError::TopicDisallowed,
        (403, "MissingProviderToken") => PushError::NoClientCertificate,
        (403, _) => PushError::CertificateRejected(reason),
        (410, _) => PushError::Unregistered,
        (429, _) => PushError::Throttled,
        (500 | 503, _) => PushError::Unavailable(status),
        _ => PushError::Rejected { status, reason },
    }
}

/// The error for a connection that failed: a certificate alert when the error carries one (a TLS
/// alert of the server, or the payload of a GOAWAY), else the text of the chain.
///
/// Both are read from the text of the chain: `h2` keeps an I/O error only as text, and shows the
/// payload of a GOAWAY escaped, as `({:?})` of its bytes.
fn connection_error(why: &(dyn std::error::Error + 'static)) -> PushError {
    let mut text = String::new();
    let mut link: Option<&(dyn std::error::Error + 'static)> = Some(why);
    while let Some(error) = link {
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&error.to_string());
        link = error.source();
    }
    if let Some(alert) = word_after(&text, "received fatal alert: ")
        && CERTIFICATE_ALERTS.contains(&alert.as_str())
    {
        return PushError::CertificateAlert(alert);
    }
    match word_after(&text, "reason") {
        Some(reason) if reason.contains("Certificate") => PushError::CertificateAlert(reason),
        _ => PushError::Connection(text),
    }
}

/// The TLS alerts that say the certificate is not accepted, as rustls spells them.
const CERTIFICATE_ALERTS: [&str; 6] = [
    "BadCertificate",
    "UnsupportedCertificate",
    "CertificateRevoked",
    "CertificateExpired",
    "CertificateUnknown",
    "UnknownCA",
];

/// The run of letters and digits that follows the first `marker` in `text`, after anything that
/// is not one (quotes, backslashes, a colon).
fn word_after(text: &str, marker: &str) -> Option<String> {
    let after = &text[text.find(marker)? + marker.len()..];
    let word: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!word.is_empty()).then_some(word)
}

#[cfg(test)]
#[path = "apns_tests.rs"]
mod tests;
