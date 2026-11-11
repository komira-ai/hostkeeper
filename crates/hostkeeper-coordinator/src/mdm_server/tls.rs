//! The TLS of the MDM listener (`docs/design/coordinator-mdm.md`, sections 4.1 and 4.10): the
//! certificate of the MDM name and its key come from the store, the client certificate is optional,
//! and the issuers a client may chain to are the MDM CA (and any issuer the migration trusts).
//!
//! **Hot swap.** [`MdmTls::config`] is asked for each connection. It looks at the versions of the
//! certificate and key the store holds and at the trusted issuers, and makes the configuration
//! again when any of them changed, so a certificate that was uploaded or renewed is served from
//! the next handshake on and a connection that is open keeps what it was accepted under. With no
//! certificate held there is no configuration and the listener answers no handshake.
//!
//! **Optional client certificate.** The verifier lets a client connect with none ([`Peer`] is then
//! `None`) and refuses one that does not chain to a trusted issuer. Each route decides what it
//! needs: `/checkin` and `/mdm` need a certificate or a signature, `/enroll` and `/scep` read
//! neither ([`super::ClientCert`]). The verifier sends the subjects of the trusted issuers as the
//! hint list, since a macOS client may offer no identity that no hint names (not verified on the
//! farm Mac, `docs/design/coordinator-mdm.md` section 12 item 10).
//!
//! [`Peer`]: crate::http::Peer

use std::sync::{Arc, Mutex, PoisonError};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};

use crate::mdm_secrets::SecretKind;
use crate::mdm_setup::MdmSetup;

/// The DER of the issuers a client certificate may chain to.
pub(crate) type Trust = Arc<dyn Fn() -> Vec<Vec<u8>> + Send + Sync>;

/// What a configuration was made from.
#[derive(Clone, PartialEq, Eq)]
struct Made {
    cert: u32,
    key: u32,
    issuers: Vec<Vec<u8>>,
}

/// The TLS configuration of the MDM listener, made again when its inputs change.
pub(crate) struct MdmTls {
    setup: Arc<MdmSetup>,
    trust: Trust,
    cached: Mutex<Option<(Made, Option<Arc<ServerConfig>>)>>,
}

impl MdmTls {
    /// The TLS of `setup`'s certificate for the name, with client certificates from `trust`.
    pub(crate) fn new(setup: Arc<MdmSetup>, trust: Trust) -> Self {
        Self {
            setup,
            trust,
            cached: Mutex::new(None),
        }
    }

    /// The newest version of `kind` held.
    fn newest(&self, kind: SecretKind) -> u32 {
        self.setup
            .list_secrets()
            .unwrap_or_default()
            .iter()
            .filter(|info| info.kind == kind)
            .map(|info| info.version)
            .max()
            .unwrap_or(0)
    }

    /// The configuration for a handshake that starts now; none while no certificate and key are
    /// held, or the ones held do not make one.
    pub(crate) fn config(&self) -> Option<Arc<ServerConfig>> {
        let made = Made {
            cert: self.newest(SecretKind::TlsCert),
            key: self.newest(SecretKind::TlsKey),
            issuers: (self.trust)(),
        };
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((was, config)) = &*cached
            && *was == made
        {
            return config.clone();
        }
        let config = self.build(&made.issuers);
        *cached = Some((made, config.clone()));
        config
    }

    fn build(&self, issuers: &[Vec<u8>]) -> Option<Arc<ServerConfig>> {
        let cert = self.setup.open(SecretKind::TlsCert).ok()??;
        let key = self.setup.open(SecretKind::TlsKey).ok()??;
        let chain = CertificateDer::pem_slice_iter(cert.expose())
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let key = PrivateKeyDer::from_pem_slice(key.expose()).ok()?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .ok()?;
        let mut roots = RootCertStore::empty();
        roots
            .add_parsable_certificates(issuers.iter().map(|der| CertificateDer::from(der.clone())));
        // With no issuer that parses there is no verifier to make, and no certificate is asked.
        let builder = match WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
            .allow_unauthenticated()
            .build()
        {
            Ok(verifier) => builder.with_client_cert_verifier(verifier),
            Err(_) => builder.with_no_client_auth(),
        };
        builder
            .with_single_cert(chain, key)
            .map_err(|why| tracing::error!(%why, "the MDM certificate and key do not make a TLS configuration"))
            .ok()
            .map(Arc::new)
    }
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
