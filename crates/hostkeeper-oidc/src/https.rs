//! The production [`KeySource`]: the issuer's discovery document, then the `jwks_uri` it
//! names, over HTTPS only. One connection per request, no redirects, every wait and every
//! body bounded. The `jwks_uri` must be on the issuer's own origin.

use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::header::{CONNECTION, HOST, USER_AGENT};
use hyper::http::uri::Authority;
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::keys::KeySource;

/// The most a discovery document or a JWKS may be.
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;

/// What `HttpsKeys` speaks TLS and HTTP over: any duplex byte stream.
pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// A connection to `host:port`: what `HttpsKeys` speaks TLS and HTTP over.
pub type Stream = Box<dyn AsyncReadWrite>;

/// Opens a [`Stream`] to `host:port`. Production dials TCP; the tests' fake issuer answers over
/// an in-process stream.
pub type Dial =
    Arc<dyn Fn(String, u16) -> BoxFuture<'static, std::io::Result<Stream>> + Send + Sync>;

/// The production dial: a TCP connection to `host:port`.
fn tcp_dial() -> Dial {
    Arc::new(|host: String, port: u16| connect(host, port).boxed())
}

/// The one line of production networking: tests never reach it, they dial in process.
async fn connect(host: String, port: u16) -> std::io::Result<Stream> {
    TcpStream::connect((host, port))
        .await
        .map(|tcp| Box::new(tcp) as Stream)
}

/// The roots an issuer's certificate chains to.
#[derive(Clone, Debug)]
pub enum Roots {
    /// The host's system store.
    System,
    /// Exactly these (tests).
    Given(Vec<CertificateDer<'static>>),
}

/// Why an [`HttpsKeys`] could not be made.
#[derive(Debug, thiserror::Error)]
pub enum KeySourceError {
    /// The roots hold no usable certificate.
    #[error("no trusted root certificates for fetching the issuer's keys")]
    NoRoots,
    /// The TLS client configuration is refused.
    #[error("the TLS client configuration: {0}")]
    Tls(#[from] rustls::Error),
}

/// Fetches an issuer's keys over HTTPS.
pub struct HttpsKeys {
    issuer: String,
    connector: TlsConnector,
    timeout: Duration,
    dial: Dial,
}

impl std::fmt::Debug for HttpsKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsKeys")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl HttpsKeys {
    /// A source for `issuer` (an `https://` URL), trusting `roots`, dialling TCP.
    ///
    /// # Errors
    /// [`KeySourceError::NoRoots`]: `roots` holds no usable certificate;
    /// [`KeySourceError::Tls`]: the TLS client configuration is refused.
    pub fn new(issuer: &str, roots: Roots, timeout: Duration) -> Result<Self, KeySourceError> {
        let certs = match roots {
            Roots::System => rustls_native_certs::load_native_certs().certs,
            Roots::Given(certs) => certs,
        };
        let mut store = rustls::RootCertStore::empty();
        let (added, _) = store.add_parsable_certificates(certs);
        if added == 0 {
            return Err(KeySourceError::NoRoots);
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            issuer: issuer.to_owned(),
            connector: TlsConnector::from(Arc::new(config)),
            timeout,
            dial: tcp_dial(),
        })
    }

    /// The same source dialling through `dial` instead of TCP (tests).
    #[must_use]
    pub fn with_dial(mut self, dial: Dial) -> Self {
        self.dial = dial;
        self
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        match tokio::time::timeout(self.timeout, self.fetch(url)).await {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(cause)) => Err(FetchError::Failed {
                url: url.to_owned(),
                cause,
            }),
            Err(_) => Err(FetchError::TimedOut {
                url: url.to_owned(),
            }),
        }
    }

    async fn fetch(&self, url: &str) -> Result<Vec<u8>, FetchCause> {
        let uri: Uri = url.parse()?;
        if uri.scheme_str() != Some("https") {
            return Err(FetchCause::NotHttps);
        }
        let authority = uri.authority().map_or("", Authority::as_str);
        let host = uri.host().unwrap_or_default();
        let port = uri.port_u16().unwrap_or(443);
        let name = ServerName::try_from(host.to_owned())?;
        let tcp = (self.dial)(host.to_owned(), port)
            .await
            .map_err(FetchCause::Connect)?;
        let tls = self
            .connector
            .connect(name, tcp)
            .await
            .map_err(FetchCause::Tls)?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(tls)).await?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = Request::get(uri.path_and_query().map_or("/", |p| p.as_str()))
            .header(HOST, authority)
            .header(USER_AGENT, "hostkeeper-oidc")
            .header(CONNECTION, "close")
            .body(Empty::<Bytes>::new())?;
        let response = sender.send_request(request).await?;
        let status = response.status();
        if !status.is_success() {
            driver.abort();
            return Err(FetchCause::Status(status));
        }
        let body = Limited::new(response.into_body(), MAX_DOCUMENT_BYTES)
            .collect()
            .await
            .map_err(BodyError)?
            .to_bytes();
        driver.abort();
        Ok(body.to_vec())
    }
}

/// Why one HTTPS request for a document failed.
#[derive(Debug, thiserror::Error)]
pub enum FetchCause {
    /// The URL is not a URI.
    #[error("{0}")]
    Uri(#[from] hyper::http::uri::InvalidUri),
    /// The URL is not `https://`.
    #[error("not https")]
    NotHttps,
    /// The URL's host is not a name or address TLS can verify.
    #[error("{0}")]
    ServerName(#[from] rustls::pki_types::InvalidDnsNameError),
    /// The connection could not be made.
    #[error("{0}")]
    Connect(std::io::Error),
    /// The TLS handshake failed.
    #[error("{0}")]
    Tls(std::io::Error),
    /// HTTP failed after the handshake.
    #[error("{0}")]
    Http(#[from] hyper::Error),
    /// The request could not be built.
    #[error("{0}")]
    Request(#[from] hyper::http::Error),
    /// The server answered with a status that is not a success.
    #[error("status {0}")]
    Status(hyper::StatusCode),
    /// The body could not be read, or is longer than [`MAX_DOCUMENT_BYTES`].
    #[error("{0}")]
    Body(#[from] BodyError),
}

/// Why a document's body could not be read: the connection failed while it was read, or it is
/// longer than [`MAX_DOCUMENT_BYTES`]. Its text is the cause's.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BodyError(pub(crate) Box<dyn std::error::Error + Send + Sync>);

/// Why a discovery document is refused.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The document is not JSON.
    #[error("{0}")]
    NotJson(#[from] serde_json::Error),
    /// The document is another issuer's.
    #[error("its issuer is not {0}")]
    OtherIssuer(String),
    /// `jwks_uri` is not on the issuer's origin.
    #[error("jwks_uri is not on the issuer's origin")]
    OffOrigin,
    /// The document has no `jwks_uri`.
    #[error("no jwks_uri")]
    NoJwksUri,
}

/// Why the issuer's keys could not be fetched; every variant names the URL asked.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// A request failed.
    #[error("{url}: {cause}")]
    Failed {
        /// The URL requested.
        url: String,
        /// What went wrong.
        #[source]
        cause: FetchCause,
    },
    /// A request was not answered in time.
    #[error("{url}: timed out")]
    TimedOut {
        /// The URL requested.
        url: String,
    },
    /// The discovery document is not the issuer's, or lacks a usable `jwks_uri`.
    #[error("{url}: {reason}")]
    Discovery {
        /// The discovery document's URL.
        url: String,
        /// What is wrong with it.
        #[source]
        reason: DiscoveryError,
    },
    /// The JWKS is not UTF-8.
    #[error("{url}: {source}")]
    NotUtf8 {
        /// The JWKS URL.
        url: String,
        /// Where the text stops being UTF-8.
        #[source]
        source: std::string::FromUtf8Error,
    },
}

/// The scheme and authority of `url`, if it has both.
fn origin(url: &str) -> Option<(String, String)> {
    let uri: Uri = url.parse().ok()?;
    Some((
        uri.scheme_str()?.to_owned(),
        uri.authority()?.as_str().to_owned(),
    ))
}

impl HttpsKeys {
    /// The `jwks_uri` of the discovery document `doc`, if the document is the issuer's own
    /// and the URI is on the issuer's origin.
    fn jwks_uri(&self, doc: &[u8]) -> Result<String, DiscoveryError> {
        let doc: serde_json::Value = serde_json::from_slice(doc)?;
        let field = |name| doc.get(name).and_then(|v| v.as_str());
        if field("issuer") != Some(self.issuer.as_str()) {
            return Err(DiscoveryError::OtherIssuer(self.issuer.clone()));
        }
        match field("jwks_uri") {
            Some(uri) if origin(uri) == origin(&self.issuer) => Ok(uri.to_owned()),
            Some(_) => Err(DiscoveryError::OffOrigin),
            None => Err(DiscoveryError::NoJwksUri),
        }
    }
}

impl KeySource for HttpsKeys {
    fn jwks(&self) -> BoxFuture<'_, Result<String, FetchError>> {
        Box::pin(async move {
            let discovery = format!("{}/.well-known/openid-configuration", self.issuer);
            let doc = self.get(&discovery).await?;
            let uri = self
                .jwks_uri(&doc)
                .map_err(|reason| FetchError::Discovery {
                    url: discovery,
                    reason,
                })?;
            String::from_utf8(self.get(&uri).await?)
                .map_err(|source| FetchError::NotUtf8 { url: uri, source })
        })
    }
}
