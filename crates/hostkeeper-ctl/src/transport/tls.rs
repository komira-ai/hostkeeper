//! The TLS material of a call, read from the files the flags name before anything connects: the
//! CA the server's certificate must chain to (no other root is trusted), and the client
//! certificate and key of mutual TLS. Both transports trust exactly this; a file that is
//! missing, or holds no certificate or key, is the caller's error.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};

use super::{ConnectError, Target};

/// The client certificate chain and its key.
pub struct Identity {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    cert_path: std::path::PathBuf,
}

/// What a call's TLS trusts and presents.
pub struct Material {
    pub ca_pem: Vec<u8>,
    roots: RootCertStore,
    ca_path: std::path::PathBuf,
    pub identity: Option<Identity>,
}

fn read(what: &'static str, path: &Path) -> Result<Vec<u8>, ConnectError> {
    std::fs::read(path).map_err(|source| ConnectError::Read {
        what,
        path: path.to_owned(),
        source,
    })
}

fn unusable(what: &'static str, path: &Path, why: impl ToString) -> ConnectError {
    ConnectError::Unusable {
        what,
        path: path.to_owned(),
        why: why.to_string(),
    }
}

/// Every PEM certificate in `pem`; at least one.
fn certificates(
    what: &'static str,
    path: &Path,
    pem: &[u8],
) -> Result<Vec<CertificateDer<'static>>, ConnectError> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| unusable(what, path, e))?;
    if certs.is_empty() {
        return Err(unusable(what, path, "no PEM certificate"));
    }
    Ok(certs)
}

impl Material {
    /// Reads `--ca-file`, and `--client-cert-file` with `--client-key-file` when given.
    pub fn read(target: &Target) -> Result<Material, ConnectError> {
        let ca_path = target.ca_file.clone();
        let ca_pem = read("the CA", &ca_path)?;
        let mut roots = RootCertStore::empty();
        for cert in certificates("the CA", &ca_path, &ca_pem)? {
            roots
                .add(cert)
                .map_err(|e| unusable("the CA", &ca_path, e))?;
        }
        let identity = match &target.client_identity {
            None => None,
            Some((cert_path, key_path)) => {
                let cert_pem = read("the client certificate", cert_path)?;
                let chain = certificates("the client certificate", cert_path, &cert_pem)?;
                let key_pem = read("the client key", key_path)?;
                let key = PrivateKeyDer::from_pem_slice(&key_pem)
                    .map_err(|e| unusable("the client key", key_path, e))?;
                Some(Identity {
                    cert_pem,
                    key_pem,
                    chain,
                    key,
                    cert_path: cert_path.clone(),
                })
            }
        };
        Ok(Material {
            ca_pem,
            roots,
            ca_path,
            identity,
        })
    }

    /// A rustls client configuration that trusts only the CA and presents the identity, offering
    /// `alpn` (`http/1.1` for the gateway).
    pub fn client_config(&self, alpn: &[u8]) -> Result<Arc<ClientConfig>, ConnectError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| unusable("the CA", &self.ca_path, e))?
            .with_root_certificates(self.roots.clone());
        let mut config = match &self.identity {
            None => builder.with_no_client_auth(),
            Some(identity) => builder
                .with_client_auth_cert(identity.chain.clone(), identity.key.clone_key())
                .map_err(|e| unusable("the client certificate", &identity.cert_path, e))?,
        };
        config.alpn_protocols = vec![alpn.to_vec()];
        Ok(Arc::new(config))
    }
}
