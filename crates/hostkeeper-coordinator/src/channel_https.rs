//! The production [`ChannelFetcher`]: the release channel's base URL (`--release-channel`), and
//! a fetch from it over HTTPS only.
//!
//! The base URL is checked once, at start ([`ChannelUrl::parse`]): `https://`, a host TLS can
//! verify, and no user, query or fragment. Each fetch is one connection to that host, to the
//! path under the base, with a connect deadline (the TCP connection and the TLS handshake), a
//! total deadline over the whole fetch, at most [`MAX_INDEX_BYTES`] of body, and no redirect
//! followed: a 3xx is a failed fetch, so nothing is ever read from another host. 200 is
//! [`Fetched::Found`], 404 is [`Fetched::NotFound`], any other status a [`FetchError::Status`].
//! The channel's certificate must chain to the roots the caller gives (the host's store in the
//! binary), as the OIDC key fetch's does.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use hostkeeper_oidc::{Dial, Roots, Stream};
use http_body_util::{BodyExt as _, Empty, LengthLimitError, Limited};
use hyper::header::{CONNECTION, HOST, USER_AGENT};
use hyper::{Request, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::channel::{ChannelFetcher, FetchError, Fetched, MAX_INDEX_BYTES};

/// Why a `--release-channel` is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChannelUrlError {
    /// Not a URL.
    #[error("{0:?} is not a URL")]
    NotUrl(String),
    /// Not `https://`.
    #[error("{0:?} is not an https:// URL: the release channel is fetched over HTTPS only")]
    NotHttps(String),
    /// No host, or one TLS cannot verify a certificate for.
    #[error("{0:?} names no host a certificate can be checked for")]
    Host(String),
    /// A user, a query or a fragment.
    #[error("{0:?} has a user, a query or a fragment: the release channel is a base URL")]
    NotBase(String),
}

/// The release channel's base URL: `https://<host>[:<port>][/<path>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelUrl {
    host: String,
    port: u16,
    authority: String,
    /// The path under the host, without a final `/`: empty for the host's root.
    base: String,
    server: ServerName<'static>,
}

impl ChannelUrl {
    /// `text` as the channel's base URL.
    ///
    /// # Errors
    /// [`ChannelUrlError`].
    pub fn parse(text: &str) -> Result<Self, ChannelUrlError> {
        if text.contains(['@', '?', '#']) {
            return Err(ChannelUrlError::NotBase(text.to_owned()));
        }
        let uri: Uri = text
            .parse()
            .map_err(|_| ChannelUrlError::NotUrl(text.to_owned()))?;
        if uri.scheme_str() != Some("https") {
            return Err(ChannelUrlError::NotHttps(text.to_owned()));
        }
        let host = uri.host().unwrap_or_default();
        let server = ServerName::try_from(host.to_owned())
            .map_err(|_| ChannelUrlError::Host(text.to_owned()))?;
        Ok(Self {
            host: host.to_owned(),
            port: uri.port_u16().unwrap_or(443),
            authority: uri
                .authority()
                .map_or_else(|| host.to_owned(), ToString::to_string),
            base: uri.path().trim_end_matches('/').to_owned(),
            server,
        })
    }

    /// The host, to dial.
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// The port, to dial.
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// The authority (`host[:port]`), as the `Host` header names it.
    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    /// The path under the host that `path` is requested at: the base, a `/`, then `path`.
    pub(crate) fn target(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    /// The name TLS checks the certificate for.
    pub(crate) fn server(&self) -> ServerName<'static> {
        self.server.clone()
    }

    /// The URL of `path` under the base.
    #[must_use]
    pub fn url_of(&self, path: &str) -> String {
        format!("https://{}{}/{path}", self.authority, self.base)
    }
}

impl fmt::Display for ChannelUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "https://{}{}", self.authority, self.base)
    }
}

/// Why an [`HttpsChannel`] could not be made.
#[derive(Debug, thiserror::Error)]
pub enum ChannelTlsError {
    /// The roots hold no usable certificate.
    #[error("no trusted root certificates for fetching from the release channel")]
    NoRoots,
    /// The TLS client configuration is refused.
    #[error("the TLS client configuration: {0}")]
    Tls(#[from] rustls::Error),
}

/// The TLS client that trusts `roots` and speaks HTTP/1.1 only, as a fetch over HTTPS makes it.
///
/// # Errors
/// [`ChannelTlsError::NoRoots`]: `roots` holds no usable certificate;
/// [`ChannelTlsError::Tls`]: the TLS client configuration is refused.
pub(crate) fn connector(roots: Roots) -> Result<TlsConnector, ChannelTlsError> {
    let certs = match roots {
        Roots::System => rustls_native_certs::load_native_certs().certs,
        Roots::Given(certs) => certs,
    };
    let mut store = rustls::RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(ChannelTlsError::NoRoots);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}

/// The production dial: a TCP connection to `host:port`.
pub(crate) fn tcp_dial() -> Dial {
    Arc::new(|host: String, port: u16| connect(host, port).boxed())
}

/// The one line of production networking: tests never reach it, they dial in process.
async fn connect(host: String, port: u16) -> std::io::Result<Stream> {
    TcpStream::connect((host, port))
        .await
        .map(|tcp| Box::new(tcp) as Stream)
}

/// Fetches from the release channel over HTTPS.
pub struct HttpsChannel {
    url: ChannelUrl,
    connector: TlsConnector,
    connect: Duration,
    total: Duration,
    dial: Dial,
}

impl fmt::Debug for HttpsChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsChannel")
            .field("url", &self.url.to_string())
            .finish_non_exhaustive()
    }
}

impl HttpsChannel {
    /// The channel at `url`, trusting `roots`, dialling TCP, with `connect` as the deadline of
    /// the connection and its handshake and `total` as the deadline of a whole fetch.
    ///
    /// # Errors
    /// [`ChannelTlsError::NoRoots`]: `roots` holds no usable certificate;
    /// [`ChannelTlsError::Tls`]: the TLS client configuration is refused.
    pub fn new(
        url: ChannelUrl,
        roots: Roots,
        connect: Duration,
        total: Duration,
    ) -> Result<Self, ChannelTlsError> {
        Ok(Self {
            url,
            connector: connector(roots)?,
            connect,
            total,
            dial: tcp_dial(),
        })
    }

    /// The same channel dialling through `dial` instead of TCP (tests).
    #[must_use]
    pub fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    /// The fetch of `path`, within the total deadline.
    async fn get(&self, path: &str) -> Result<Fetched, FetchError> {
        let url = self.url.url_of(path);
        match tokio::time::timeout(self.total, self.request(&url, path)).await {
            Ok(answer) => answer,
            Err(_) => Err(FetchError::TimedOut {
                url,
                secs: self.total.as_secs(),
            }),
        }
    }

    /// One connection, one GET of `path`, its answer.
    async fn request(&self, url: &str, path: &str) -> Result<Fetched, FetchError> {
        let handshake = async {
            let tcp = (self.dial)(self.url.host.clone(), self.url.port).await?;
            self.connector.connect(self.url.server.clone(), tcp).await
        };
        let tls = match tokio::time::timeout(self.connect, handshake).await {
            Ok(Ok(tls)) => tls,
            Ok(Err(why)) => {
                return Err(FetchError::Connect {
                    url: url.to_owned(),
                    why: why.to_string(),
                });
            }
            Err(_) => {
                return Err(FetchError::ConnectTimedOut {
                    url: url.to_owned(),
                    secs: self.connect.as_secs(),
                });
            }
        };
        let http = |why: &dyn fmt::Display| FetchError::Http {
            url: url.to_owned(),
            why: why.to_string(),
        };
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(tls))
                .await
                .map_err(|why| http(&why))?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let answer = async {
            let request = Request::get(format!("{}/{path}", self.url.base))
                .header(HOST, &self.url.authority)
                .header(USER_AGENT, "hostkeeper-coordinator")
                .header(CONNECTION, "close")
                .body(Empty::<Bytes>::new())
                .map_err(|why| http(&why))?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|why| http(&why))?;
            match response.status() {
                StatusCode::OK => {}
                StatusCode::NOT_FOUND => return Ok(Fetched::NotFound),
                status => {
                    return Err(FetchError::Status {
                        url: url.to_owned(),
                        status: status.as_u16(),
                    });
                }
            }
            let body = Limited::new(response.into_body(), MAX_INDEX_BYTES)
                .collect()
                .await
                .map_err(|why| {
                    if why.downcast_ref::<LengthLimitError>().is_some() {
                        FetchError::TooLarge {
                            url: url.to_owned(),
                        }
                    } else {
                        http(&why)
                    }
                })?;
            Ok(Fetched::Found(body.to_bytes().to_vec()))
        }
        .await;
        driver.abort();
        answer
    }
}

impl ChannelFetcher for HttpsChannel {
    fn fetch<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        self.get(path).boxed()
    }
}
