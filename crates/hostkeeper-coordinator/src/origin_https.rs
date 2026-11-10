//! The HTTPS pieces that serve [`crate::repository_origin`]: an origin's base URL, the TLS client and the TCP dial.
//!
//! The URL is checked once ([`OriginUrl::parse`]): `https://`, a host TLS can verify, and no
//! user, query or fragment. The certificate of an origin must chain to the roots the caller gives
//! (the host's store in the binary), as the OIDC key fetch's does.

use std::sync::Arc;

use futures::FutureExt as _;
use hostkeeper_oidc::{Dial, Roots, Stream};
use hyper::Uri;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Why a repository origin's URL is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OriginUrlError {
    /// Not a URL.
    #[error("{0:?} is not a URL")]
    NotUrl(String),
    /// Not `https://`.
    #[error("{0:?} is not an https:// URL: an origin is fetched over HTTPS only")]
    NotHttps(String),
    /// No host, or one TLS cannot verify a certificate for.
    #[error("{0:?} names no host a certificate can be checked for")]
    Host(String),
    /// A user, a query or a fragment.
    #[error("{0:?} has a user, a query or a fragment: an origin's URL is a base URL")]
    NotBase(String),
}

/// A repository origin's base URL: `https://<host>[:<port>][/<path>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginUrl {
    host: String,
    port: u16,
    authority: String,
    /// The path under the host, without a final `/`: empty for the host's root.
    base: String,
    server: ServerName<'static>,
}

impl OriginUrl {
    /// `text` as an origin's base URL.
    ///
    /// # Errors
    /// [`OriginUrlError`].
    pub fn parse(text: &str) -> Result<Self, OriginUrlError> {
        if text.contains(['@', '?', '#']) {
            return Err(OriginUrlError::NotBase(text.to_owned()));
        }
        let uri: Uri = text
            .parse()
            .map_err(|_| OriginUrlError::NotUrl(text.to_owned()))?;
        if uri.scheme_str() != Some("https") {
            return Err(OriginUrlError::NotHttps(text.to_owned()));
        }
        let host = uri.host().unwrap_or_default();
        let server = ServerName::try_from(host.to_owned())
            .map_err(|_| OriginUrlError::Host(text.to_owned()))?;
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
}

/// Why the TLS client of an origin could not be made.
#[derive(Debug, thiserror::Error)]
pub enum OriginTlsError {
    /// The roots hold no usable certificate.
    #[error("no trusted root certificates for fetching from a repository origin")]
    NoRoots,
    /// The TLS client configuration is refused.
    #[error("the TLS client configuration: {0}")]
    Tls(#[from] rustls::Error),
}

/// The TLS client that trusts `roots` and speaks HTTP/1.1 only, as a fetch over HTTPS makes it.
///
/// # Errors
/// [`OriginTlsError::NoRoots`]: `roots` holds no usable certificate;
/// [`OriginTlsError::Tls`]: the TLS client configuration is refused.
pub(crate) fn connector(roots: Roots) -> Result<TlsConnector, OriginTlsError> {
    let certs = match roots {
        Roots::System => rustls_native_certs::load_native_certs().certs,
        Roots::Given(certs) => certs,
    };
    let mut store = rustls::RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(OriginTlsError::NoRoots);
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
