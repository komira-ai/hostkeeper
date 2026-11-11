//! The HTTPS requests the certificate for the MDM name makes (`docs/design/coordinator-mdm.md`,
//! section 4.10): to the ACME directory and to the DNS provider's API. One port, [`Https`], so a
//! test answers both with a fake and no test reaches the network (T12); [`RealHttps`] is the
//! production one, on the TLS client of `hostkeeper-mdm-push` (rustls with ring, the public web's
//! roots) and hyper's HTTP/1.1.
//!
//! A reply is whatever the server answered, status included: a 4xx is a reply and not an error,
//! because the caller reads it (a provider that says 401 is a rejected credential). Redirects are
//! not followed, and a body is read to [`MAX_REPLY_BYTES`] and no further.

use std::time::Duration;

use bytes::Bytes;
use hostkeeper_mdm_push::BoxFuture;
use hostkeeper_mdm_push::tls::{self, Endpoint};
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::header::{HOST, HeaderName, HeaderValue, USER_AGENT};
use hyper::{Method, Request, Uri};
use hyper_util::rt::TokioIo;
use rustls::RootCertStore;

/// The most bytes of a reply that are read: a directory, an order and a certificate chain are a
/// few kilobytes.
pub const MAX_REPLY_BYTES: usize = 1024 * 1024;

/// The limit given to the TLS client's own dial and handshake. The request as a whole is bounded
/// by the client's timeout in [`Https::send`], which this is far above.
const CONNECT_BOUND: Duration = Duration::from_secs(86_400);

/// A request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method, in capitals.
    pub method: String,
    /// The whole URL, `https://host[:port]/path?query`.
    pub url: String,
    /// The headers, names in lower case.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

/// A reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpReply {
    /// The status.
    pub status: u16,
    /// The headers, names in lower case.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

/// Why a request got no reply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HttpFailure(pub String);

fn failed(why: impl std::fmt::Display) -> HttpFailure {
    HttpFailure(why.to_string())
}

/// Sends a request and answers what the server answered.
pub trait Https: Send + Sync {
    /// Sends `request`.
    ///
    /// # Errors
    /// [`HttpFailure`]: no reply (the URL, the connection, the handshake, the timeout, the body).
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpReply, HttpFailure>>;
}

/// The production [`Https`]: a connection per request, within a timeout.
pub struct RealHttps {
    roots: RootCertStore,
    timeout: Duration,
    dial: Option<String>,
}

impl RealHttps {
    /// The client trusting the public web's roots, each request within `timeout`.
    #[must_use]
    pub fn production(timeout: Duration) -> Self {
        Self {
            roots: tls::public_roots(),
            timeout,
            dial: None,
        }
    }

    /// The client trusting `roots` and dialing `dial` (`host:port`) whatever the URL says, for a
    /// test's loopback server; the name checked is still the URL's.
    #[cfg(test)]
    #[must_use]
    pub fn trusting(roots: RootCertStore, dial: &str, timeout: Duration) -> Self {
        Self {
            roots,
            timeout,
            dial: Some(dial.to_owned()),
        }
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpReply, HttpFailure> {
        let uri: Uri = request.url.parse().map_err(failed)?;
        if uri.scheme_str() != Some("https") {
            return Err(failed("the URL is not an https URL"));
        }
        // A URL that parses and has no host gets the empty name, which no certificate has: the
        // handshake fails and says so.
        let host = uri.host().unwrap_or_default();
        let endpoint = Endpoint::new(host, uri.port_u16().unwrap_or(443));
        let endpoint = self
            .dial
            .as_deref()
            .map_or_else(|| endpoint.clone(), |dial| endpoint.clone().dialing(dial));
        let method = Method::from_bytes(request.method.as_bytes()).map_err(failed)?;
        let target = match uri.query() {
            Some(query) => format!("{}?{query}", uri.path()),
            None => uri.path().to_owned(),
        };
        let mut built = Request::builder()
            .method(method)
            .uri(&target)
            .body(Full::new(Bytes::from(request.body)))
            .map_err(failed)?;
        let headers = built.headers_mut();
        headers.insert(HOST, HeaderValue::from_str(host).map_err(failed)?);
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("hostkeeper-coordinator"),
        );
        for (name, value) in &request.headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(failed)?,
                HeaderValue::from_str(value).map_err(failed)?,
            );
        }
        let config =
            tls::client_config(self.roots.clone(), None, &[b"http/1.1"]).map_err(failed)?;
        let stream = tls::connect(&endpoint, config, CONNECT_BOUND)
            .await
            .map_err(failed)?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(failed)?;
        tokio::spawn(async move {
            drop(connection.await);
        });
        let answer = sender.send_request(built).await.map_err(failed)?;
        let status = answer.status().as_u16();
        let headers = answer
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        let body = Limited::new(answer.into_body(), MAX_REPLY_BYTES)
            .collect()
            .await
            .map_err(failed)?
            .to_bytes()
            .to_vec();
        Ok(HttpReply {
            status,
            headers,
            body,
        })
    }
}

impl Https for RealHttps {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpReply, HttpFailure>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.exchange(request))
                .await
                .unwrap_or_else(|_| Err(failed("the request timed out")))
        })
    }
}

#[cfg(test)]
#[path = "https_tests.rs"]
mod tests;
