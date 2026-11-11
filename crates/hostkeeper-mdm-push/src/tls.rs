//! The TLS client of the APNs sender and of the mdmcert.download transport: rustls with the
//! `ring` provider, the web's root certificates (`webpki-roots`) or the ones a test gives, and
//! for APNs the push certificate as the client identity.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// Why a client configuration could not be made.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The certificate and key are not an identity rustls can sign with.
    #[error("the TLS client identity is not usable: {0}")]
    Identity(String),
    /// A root certificate is not one rustls takes.
    #[error("a root certificate is not usable: {0}")]
    Root(String),
}

/// Where a client connects, and the name the server's certificate must carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The `host:port` to dial.
    pub dial: String,
    /// The name of the certificate.
    pub server_name: String,
}

impl Endpoint {
    /// The endpoint `host`:`port`, dialed by name and checked against it.
    #[must_use]
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            dial: format!("{host}:{port}"),
            server_name: host.to_owned(),
        }
    }

    /// The same endpoint reached at `dial` (`host:port`), as a test's loopback listener.
    #[must_use]
    pub fn dialing(mut self, dial: &str) -> Self {
        dial.clone_into(&mut self.dial);
        self
    }
}

/// The root certificates of the public web.
#[must_use]
pub fn public_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

/// The roots that are `certificates` (DER), for a test's CA.
///
/// # Errors
/// [`TlsError::Root`]: one is not a certificate.
pub fn roots_of(certificates: &[CertificateDer<'static>]) -> Result<RootCertStore, TlsError> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate.clone())
            .map_err(|why| TlsError::Root(why.to_string()))?;
    }
    Ok(roots)
}

/// A client configuration trusting `roots`, offering `alpn`, and presenting `identity` (the
/// certificate chain and its key) when one is given.
///
/// # Errors
/// [`TlsError::Identity`]: the key is not one rustls signs with, or does not suit the chain.
pub fn client_config(
    roots: RootCertStore,
    identity: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    alpn: &[&[u8]],
) -> Result<Arc<ClientConfig>, TlsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|why| TlsError::Identity(why.to_string()))?
        .with_root_certificates(roots);
    let mut config = match identity {
        Some((chain, key)) => builder
            .with_client_auth_cert(chain, key)
            .map_err(|why| TlsError::Identity(why.to_string()))?,
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    Ok(Arc::new(config))
}

/// Connects to `endpoint` and completes the handshake, within `timeout` for both.
///
/// # Errors
/// An I/O error: the dial, the handshake (a certificate alert of the server arrives as one
/// carrying the rustls error) or the timeout.
pub async fn connect(
    endpoint: &Endpoint,
    config: Arc<ClientConfig>,
    timeout: Duration,
) -> io::Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(endpoint.server_name.clone())
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))?;
    let attempt = async {
        let tcp = TcpStream::connect(&endpoint.dial).await?;
        TlsConnector::from(config).connect(name, tcp).await
    };
    tokio::time::timeout(timeout, attempt)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the connection timed out"))?
}
