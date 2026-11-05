//! The real token source: a GitHub Actions OIDC token, from the endpoint the platform names.
//!
//! The platform gives a job that was granted `id-token: write` two variables: the URL to ask
//! and the bearer to ask with. They are secret material the platform sets, the one environment
//! read this crate makes. The request trusts exactly the root certificates it is given, follows
//! no redirect and uses no proxy. An error names the status or the transport's reason, never
//! the body, the bearer or a token.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

/// Why a token was not got.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenError {
    #[error("no OIDC token request variables: the calling job must grant id-token: write")]
    NoVariables,
    #[error("the OIDC token request failed: {0}")]
    Request(String),
    #[error("the OIDC token request was answered with status {0}")]
    Status(u16),
    #[error("the OIDC token reply holds no token")]
    NoToken,
}

/// Why `--web-roots` is not usable.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RootsError {
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is not PEM certificates: {why}", path.display())]
    Pem { path: PathBuf, why: String },
    #[error("{} holds no certificate", .0.display())]
    Empty(PathBuf),
}

/// The certificates of the PEM file at `path`; at least one.
pub(crate) fn read_roots(path: &Path) -> Result<Vec<CertificateDer<'static>>, RootsError> {
    let bytes = std::fs::read(path).map_err(|source| RootsError::Read {
        path: path.to_owned(),
        source,
    })?;
    let roots = CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| RootsError::Pem {
            path: path.to_owned(),
            why: e.to_string(),
        })?;
    if roots.is_empty() {
        return Err(RootsError::Empty(path.to_owned()));
    }
    Ok(roots)
}

/// The URL and the bearer of the token request, from the two variables the platform sets; none
/// when either is absent.
#[expect(
    clippy::disallowed_methods,
    reason = "the platform sets these two secret variables for a job granted id-token: write; there is no flag for them (AGENTS.md S2)"
)]
fn platform_variables() -> Option<(String, String)> {
    std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL")
        .ok()
        .zip(std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok())
}

/// Requests OIDC tokens.
pub(crate) struct OidcTokens {
    agent: ureq::Agent,
    /// The URL and the bearer of the request; none when the platform gave none.
    request: Option<(String, String)>,
}

impl OidcTokens {
    /// A source that asks `request` (URL and bearer), trusting exactly `roots`, over HTTPS only
    /// when `https_only` (always, but in a test that asks a loopback server).
    pub(crate) fn new(
        request: Option<(String, String)>,
        roots: &[CertificateDer<'static>],
        https_only: bool,
    ) -> OidcTokens {
        let roots: Vec<Certificate<'static>> = roots
            .iter()
            .map(|der| {
                let bytes: &[u8] = der.as_ref();
                Certificate::from_der(bytes).to_owned()
            })
            .collect();
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::new_with_certs(&roots))
            .build();
        let config = ureq::Agent::config_builder()
            .https_only(https_only)
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(60)))
            .tls_config(tls)
            .build();
        OidcTokens {
            agent: ureq::Agent::new_with_config(config),
            request,
        }
    }

    /// A source for the variables of the job's environment.
    pub(crate) fn from_environment(roots: &[CertificateDer<'static>]) -> OidcTokens {
        OidcTokens::new(platform_variables(), roots, true)
    }

    /// A new token for `audience`.
    pub(crate) fn token(&self, audience: &str) -> Result<String, TokenError> {
        let (url, bearer) = self.request.as_ref().ok_or(TokenError::NoVariables)?;
        let response = self
            .agent
            .get(url)
            .query("audience", audience)
            .header("Authorization", format!("bearer {bearer}"))
            .call()
            .map_err(|e| TokenError::Request(e.to_string()))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(TokenError::Status(status));
        }
        response
            .into_body()
            .read_to_string()
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|json| json["value"].as_str().map(str::to_owned))
            .filter(|token| !token.is_empty())
            .ok_or(TokenError::NoToken)
    }
}
