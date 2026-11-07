//! Downloads, over HTTPS only, for `fetch`: at most [`MAX_REDIRECTS`] redirects, and a body
//! of at most the caller's limit (a body of exactly that size is accepted).

use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::ring::default_provider;
use ureq::Agent;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::{DefaultResolver, Resolver};
use ureq::unversioned::transport::{Connector, DefaultConnector};

/// How long one download may take, whole.
const TIMEOUT: Duration = Duration::from_secs(600);

/// How many redirects one download follows. Release assets redirect once or twice (the
/// release page to its storage host), so five leaves room without letting a loop run long.
/// Past it the download is an error ([`ureq::Error::TooManyRedirects`]), never the last
/// redirect's body.
pub(crate) const MAX_REDIRECTS: u32 = 5;

/// Why a download failed.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The server answered with a status outside 2xx.
    #[error("answered {0}")]
    Status(u16),
    /// The request did not complete: a URL the client refuses (plain `http://` when only
    /// HTTPS is allowed), the connection or TLS failed, or the body passed the limit.
    #[error(transparent)]
    Transport(#[from] ureq::Error),
}

/// Fetches a URL's body.
pub trait Download {
    /// The body of `url`, which must answer 2xx and hold at most `limit` bytes.
    ///
    /// # Errors
    /// [`DownloadError`]: the status, or why the request did not complete.
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, DownloadError>;
}

/// [`Download`] over ureq: redirects are followed (release assets are served from a
/// redirect target), every hop must be `https://` when `https_only`, no proxy setting is
/// read, and the host's roots are trusted. What arrives is checked against its sha256 pin
/// afterwards, so the roots guard the transfer, not what is installed.
pub struct Net {
    agent: Agent,
}

impl Net {
    /// An agent; `https_only` is true outside tests.
    pub fn new(https_only: bool) -> Net {
        let roots: Vec<Certificate<'static>> = rustls_native_certs::load_native_certs()
            .certs
            .iter()
            .map(|der| Certificate::from_der(der.as_ref()).to_owned())
            .collect();
        Net::with_parts(
            https_only,
            &roots,
            DefaultConnector::default(),
            DefaultResolver::default(),
        )
    }

    /// An agent over this connector and resolver (the network's own in [`Net::new`]; an
    /// in-memory pair in this module's tests).
    fn with_parts(
        https_only: bool,
        roots: &[Certificate<'static>],
        connector: impl Connector,
        resolver: impl Resolver,
    ) -> Net {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::new_with_certs(roots))
            .unversioned_rustls_crypto_provider(Arc::new(default_provider()))
            .build();
        let config = Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .https_only(https_only)
            .max_redirects(MAX_REDIRECTS)
            .max_redirects_will_error(true)
            .timeout_global(Some(TIMEOUT))
            .tls_config(tls)
            .build();
        Net {
            agent: Agent::with_parts(config, connector, resolver),
        }
    }
}

impl Download for Net {
    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, DownloadError> {
        let mut answer = self.agent.get(url).call()?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(DownloadError::Status(status));
        }
        // ureq's limit fails the read that follows the last permitted byte, even at the end
        // of the body, so a body of exactly `limit` bytes would be refused. Allow one more
        // byte and report any overrun against the caller's limit.
        let body = answer
            .body_mut()
            .with_config()
            .limit(limit.saturating_add(1));
        match body.read_to_vec() {
            Ok(bytes) if bytes.len() as u64 > limit => Err(over(limit)),
            Ok(bytes) => Ok(bytes),
            Err(ureq::Error::BodyExceedsLimit(_)) => Err(over(limit)),
            Err(other) => Err(other.into()),
        }
    }
}

/// The error for a body longer than `limit`.
fn over(limit: u64) -> DownloadError {
    DownloadError::Transport(ureq::Error::BodyExceedsLimit(limit))
}

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
