//! The coordinator's read of a private repository's origin (docs/design/hostgroups.md, section
//! 6.6): one GET over HTTPS, with the `Authorization` header the credential file holds, whose
//! answer the caller streams to the host.
//!
//! The origin's certificate must chain to the roots the coordinator is built with. Each read is
//! one connection (`Connection: close`) to the host of the repository's `uri`, to the path under
//! it, with a deadline ([`CONNECT`]) over the connection, the handshake and the answer's headers,
//! and, once they are read, a deadline of [`IDLE`] on each frame of the body, but none over the
//! whole: a package may take as long as the host takes to read it. No redirect is followed: a 3xx
//! is a failure, so nothing is ever read from another host or sent the credential. 200 with a
//! `Content-Length` is [`Origin::Found`], 404 is [`Origin::NotFound`]; any other status, and a
//! 200 without a length (the daemon needs one), is an [`OriginError`]. Its text names the kind of
//! failure and never the request, so the credential is in no log line.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_oidc::{Dial, Roots};
use http_body_util::Empty;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_LENGTH, HOST, HeaderValue, USER_AGENT};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsConnector;

use crate::origin_https::{OriginTlsError, OriginUrl, connector, tcp_dial};
use crate::service::Coordinator;

/// The deadline over the connection, the TLS handshake and the origin's headers.
pub const CONNECT: Duration = Duration::from_secs(30);

/// The longest the origin may take to give the next frame of a body.
pub const IDLE: Duration = Duration::from_secs(30);

/// Why a read of the origin failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OriginError {
    /// The repository's `uri` is not one the coordinator can read from.
    #[error("the uri cannot be read from")]
    Uri,
    /// The connection or the TLS handshake failed.
    #[error("the connection failed")]
    Connect,
    /// The connection, the handshake and the headers took longer than [`CONNECT`].
    #[error("the connection timed out")]
    ConnectTimedOut,
    /// The HTTP exchange failed or the request cannot be made.
    #[error("the HTTP exchange failed")]
    Http,
    /// The origin answered 200 without a `Content-Length`.
    #[error("the origin gave no Content-Length")]
    NoLength,
    /// The origin answered a status other than 200 and 404.
    #[error("the origin answered {0}")]
    Status(u16),
}

impl From<hyper::Error> for OriginError {
    fn from(_: hyper::Error) -> Self {
        Self::Http
    }
}

impl From<hyper::http::Error> for OriginError {
    fn from(_: hyper::http::Error) -> Self {
        Self::Http
    }
}

/// Why the body of an answer broke off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BodyError {
    /// The origin gave no frame for [`IDLE`].
    #[error("the origin gave no data for {} seconds", IDLE.as_secs())]
    Idle,
    /// The connection to the origin broke.
    #[error("the connection to the origin broke")]
    Broken,
}

/// The origin's answer to a read.
pub(crate) enum Origin {
    /// 200: the body, with the length the origin gave.
    Found { length: u64, body: Forwarded },
    /// 404.
    NotFound,
}

/// Aborts the task that drives a connection when it is dropped.
pub(crate) struct Driver(pub(crate) JoinHandle<()>);

impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The body of an origin's answer, each frame within [`IDLE`] of the one before it. It keeps the
/// connection's driver alive and ends it when dropped.
pub(crate) struct Forwarded {
    inner: Pin<Box<Incoming>>,
    idle: Pin<Box<Sleep>>,
    _driver: Driver,
}

impl Forwarded {
    /// The body of `response`, each frame within [`IDLE`] of the one before it, which keeps
    /// `driver` alive.
    pub(crate) fn new(response: Incoming, driver: Driver) -> Self {
        Self {
            inner: Box::pin(response),
            idle: Box::pin(tokio::time::sleep(IDLE)),
            _driver: driver,
        }
    }
}

impl Body for Forwarded {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                this.idle.as_mut().reset(Instant::now() + IDLE);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(_))) => Poll::Ready(Some(Err(BodyError::Broken))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending if this.idle.as_mut().poll(cx).is_ready() => {
                Poll::Ready(Some(Err(BodyError::Idle)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Reads private repositories' origins over HTTPS.
pub struct RepositoryOrigin {
    connector: TlsConnector,
    dial: Dial,
}

impl fmt::Debug for RepositoryOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RepositoryOrigin").finish_non_exhaustive()
    }
}

impl RepositoryOrigin {
    /// An origin client trusting `roots`, dialling TCP.
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

    /// The GET of `path` under the repository at `uri`, with `authorization` as the header of
    /// that name.
    ///
    /// # Errors
    /// [`OriginError`].
    pub(crate) async fn get(
        &self,
        uri: &str,
        path: &str,
        authorization: &str,
    ) -> Result<Origin, OriginError> {
        let url = OriginUrl::parse(uri).map_err(|_| OriginError::Uri)?;
        let (response, driver) =
            tokio::time::timeout(CONNECT, self.answer(&url, path, authorization))
                .await
                .map_err(|_| OriginError::ConnectTimedOut)??;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Ok(Origin::NotFound),
            status => return Err(OriginError::Status(status.as_u16())),
        }
        let length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(OriginError::NoLength)?;
        Ok(Origin::Found {
            length,
            body: Forwarded::new(response.into_body(), driver),
        })
    }

    /// The connection, the handshake, the request and the answer's headers.
    async fn answer(
        &self,
        url: &OriginUrl,
        path: &str,
        authorization: &str,
    ) -> Result<(hyper::Response<Incoming>, Driver), OriginError> {
        let tcp = (self.dial)(url.host().to_owned(), url.port())
            .await
            .map_err(|_| OriginError::Connect)?;
        let tls = self
            .connector
            .connect(url.server(), tcp)
            .await
            .map_err(|_| OriginError::Connect)?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(tls)).await?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let mut credential = HeaderValue::from_str(authorization).map_err(|_| OriginError::Http)?;
        credential.set_sensitive(true);
        let request = Request::get(url.target(path))
            .header(HOST, url.authority())
            .header(USER_AGENT, "hostkeeper-coordinator")
            .header(CONNECTION, "close")
            .header(AUTHORIZATION, credential)
            .body(Empty::<Bytes>::new())?;
        let response = sender.send_request(request).await?;
        Ok((response, driver))
    }
}

impl Coordinator {
    /// The same coordinator reading private repositories' origins with `origin`; without one, a
    /// read of a private repository is answered 502 ([`crate::repository_serve`]).
    #[must_use]
    pub fn with_repository_origin(mut self, origin: RepositoryOrigin) -> Self {
        self.repository_origin = Some(origin);
        self
    }
}
