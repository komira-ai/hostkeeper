//! The package proxy's read of a pkg's origin (`docs/design/package-proxy.md`, sections 4 and
//! 10): one `GET` of the `url` of an `installs` entry over HTTPS, following the redirects a
//! release host answers with.
//!
//! The origin is the one the operator wrote in the setting `installs` (packages.komira.ai, or a
//! GitHub release asset as a fallback), never anything in a request. Each hop is one connection
//! (`Connection: close`) with the deadlines of [`crate::repository_origin`] ([`CONNECT`] over the
//! connection, the handshake and the headers, [`IDLE`] on each frame of the body), and no
//! credential is sent. A redirect (301, 302, 303, 307, 308) is followed at most `redirects` times
//! (`proxy_redirects`), only to an `https://` URL or to a path of the same host, so the chain can
//! not leave HTTPS or go on for ever; a `Location` that is anything else, and any status but 200,
//! is a failure. The text of a failure names the kind and never a URL (a release host's redirect
//! carries a signed query). What makes a long chain safe is the digest the caller checks over the
//! body, not the hosts.

use std::fmt;

use bytes::Bytes;
use futures::StreamExt as _;
use hostkeeper_oidc::{Dial, Roots};
use http_body_util::Empty;
use hyper::header::{CONNECTION, CONTENT_LENGTH, ETAG, HOST, HeaderValue, LOCATION, USER_AGENT};
use hyper::http::uri::PathAndQuery;
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use crate::origin_https::{OriginTlsError, connector, tcp_dial};
use crate::proxy_cache::{FetchError, Fetched};
use crate::repository_origin::{CONNECT, Driver, Forwarded};
use crate::service::Coordinator;

/// Why a read of a pkg's origin failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PkgOriginError {
    /// The URL is not `https://` with a host a certificate can be checked for, or has a user.
    #[error("the url is not an https:// URL without a user")]
    Url,
    /// A redirect to a `Location` that is not an `https://` URL or a path of the same host.
    #[error("a redirect named a location that is not an https:// URL or a path")]
    Location,
    /// More redirects than `proxy_redirects`.
    #[error("the origin redirected more than {0} times")]
    Redirects(u64),
    /// The connection or the TLS handshake failed.
    #[error("the connection failed")]
    Connect,
    /// The connection, the handshake and the headers took longer than [`CONNECT`].
    #[error("the connection timed out")]
    TimedOut,
    /// The HTTP exchange failed.
    #[error("the HTTP exchange failed")]
    Http,
    /// The origin answered a status other than 200 and the redirects.
    #[error("the origin answered {0}")]
    Status(u16),
}

impl From<hyper::Error> for PkgOriginError {
    fn from(_: hyper::Error) -> Self {
        Self::Http
    }
}

/// An `https://` URL split as a connection needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
    authority: String,
    /// The path and query a request line names.
    path: PathAndQuery,
    server: ServerName<'static>,
}

impl Target {
    fn parse(text: &str) -> Result<Self, PkgOriginError> {
        let uri: Uri = text.parse().map_err(|_| PkgOriginError::Url)?;
        let authority = uri.authority().ok_or(PkgOriginError::Url)?;
        if uri.scheme_str() != Some("https") || authority.as_str().contains('@') {
            return Err(PkgOriginError::Url);
        }
        let host = uri.host().unwrap_or_default();
        Ok(Self {
            host: host.to_owned(),
            port: uri.port_u16().unwrap_or(443),
            authority: authority.as_str().to_owned(),
            path: uri
                .path_and_query()
                .cloned()
                .unwrap_or(PathAndQuery::from_static("/")),
            server: ServerName::try_from(host.to_owned()).map_err(|_| PkgOriginError::Url)?,
        })
    }

    /// Where a redirect to `location` goes from here.
    fn follow(&self, location: &str) -> Result<Self, PkgOriginError> {
        if location.starts_with('/') && !location.starts_with("//") {
            // The same host and name; only the path is the redirect's, and it must be one a
            // request line can carry.
            return location
                .parse()
                .map(|path| Self {
                    path,
                    ..self.clone()
                })
                .map_err(|_| PkgOriginError::Location);
        }
        Self::parse(location).map_err(|_| PkgOriginError::Location)
    }
}

/// Reads the origins of pkgs over HTTPS.
pub struct PkgOrigin {
    connector: TlsConnector,
    dial: Dial,
}

impl fmt::Debug for PkgOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkgOrigin").finish_non_exhaustive()
    }
}

impl PkgOrigin {
    /// A client trusting `roots`, dialling TCP.
    ///
    /// # Errors
    /// [`OriginTlsError::NoRoots`]: `roots` holds no usable certificate;
    /// [`OriginTlsError::Tls`]: the TLS client configuration is refused.
    pub fn new(roots: Roots) -> Result<Self, OriginTlsError> {
        Ok(Self {
            connector: connector(roots)?,
            dial: tcp_dial(),
        })
    }

    /// The same client dialling through `dial` instead of TCP (tests).
    #[must_use]
    pub fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    /// The read of `url`, following at most `redirects` redirects: the origin's validator, the
    /// length it declared and the body.
    ///
    /// # Errors
    /// [`PkgOriginError`].
    pub(crate) async fn get(&self, url: &str, redirects: u64) -> Result<Fetched, PkgOriginError> {
        let mut target = Target::parse(url)?;
        let mut followed = 0;
        loop {
            let (response, driver) = tokio::time::timeout(CONNECT, self.answer(&target))
                .await
                .map_err(|_| PkgOriginError::TimedOut)??;
            let status = response.status();
            if status == StatusCode::OK {
                return Ok(fetched(response, driver));
            }
            if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Err(PkgOriginError::Status(status.as_u16()));
            }
            if followed == redirects {
                return Err(PkgOriginError::Redirects(redirects));
            }
            followed += 1;
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(PkgOriginError::Location)?;
            target = target.follow(location)?;
        }
    }

    /// The connection, the handshake, the request and the answer's headers.
    async fn answer(
        &self,
        target: &Target,
    ) -> Result<(Response<hyper::body::Incoming>, Driver), PkgOriginError> {
        let tcp = (self.dial)(target.host.clone(), target.port)
            .await
            .map_err(|_| PkgOriginError::Connect)?;
        let tls = self
            .connector
            .connect(target.server.clone(), tcp)
            .await
            .map_err(|_| PkgOriginError::Connect)?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(tls)).await?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let mut request = Request::new(Empty::<Bytes>::new());
        *request.uri_mut() = Uri::from(target.path.clone());
        let headers = request.headers_mut();
        // The authority parsed as a URL's, so it is a header value; the empty value is never sent.
        headers.insert(
            HOST,
            HeaderValue::from_str(&target.authority).unwrap_or(HeaderValue::from_static("")),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("hostkeeper-coordinator"),
        );
        headers.insert(CONNECTION, HeaderValue::from_static("close"));
        let response = sender.send_request(request).await?;
        Ok((response, driver))
    }
}

/// The cache's view of a 200: the `ETag`, the declared length and the body as frames.
fn fetched(response: Response<hyper::body::Incoming>, driver: Driver) -> Fetched {
    let headers = response.headers();
    let validator = headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let length = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let body = http_body_util::BodyStream::new(Forwarded::new(response.into_body(), driver))
        .filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(why) => Some(Err(FetchError::Failed(why.to_string()))),
            }
        });
    Fetched {
        validator,
        length,
        body: Box::pin(body),
    }
}

impl Coordinator {
    /// The same coordinator reading the origins of pkgs with `origin`; without one, a pkg that is
    /// not in `--mdm-pkg-dir` is not fetched ([`crate::mdm_server`]).
    #[must_use]
    pub fn with_pkg_origin(mut self, origin: Option<PkgOrigin>) -> Self {
        self.pkg_origin = origin.map(std::sync::Arc::new);
        self
    }

    /// The reader of pkgs' origins: none without `--proxy-cache-dir`.
    #[must_use]
    pub(crate) fn pkg_origin(&self) -> Option<&std::sync::Arc<PkgOrigin>> {
        self.pkg_origin.as_ref()
    }
}

#[cfg(test)]
#[path = "pkg_origin_tests.rs"]
mod tests;
