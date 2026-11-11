//! A push certificate and its key (`docs/design/coordinator-mdm.md`, sections 4.4 and 4.9): read,
//! checked against each other, and made into the TLS client identity APNs authenticates by.
//!
//! The key is read as PEM of any of the shapes rustls-pki-types reads: `BEGIN RSA PRIVATE KEY`
//! (PKCS#1, which the key migrated from NanoHUB is: the spike read it with the same parser),
//! `BEGIN PRIVATE KEY` (PKCS#8) or an EC key. Only an RSA key can be the key of a push
//! certificate made through [`crate::request`], and the match check is for RSA.

use rsa09::pkcs1::DecodeRsaPrivateKey as _;
use rsa09::pkcs8::DecodePrivateKey as _;
use rsa09::pkcs8::DecodePublicKey as _;
use rsa09::{RsaPrivateKey, RsaPublicKey};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use x509_parser::prelude::{FromDer as _, X509Certificate};

/// The start of the topic of every MDM push certificate.
pub const TOPIC_PREFIX: &str = "com.apple.mgmt.External.";

/// The `userId` (`UID`) attribute of a subject, which holds the topic.
const UID_OID: [u64; 7] = [0, 9, 2342, 19_200_300, 100, 1, 1];

/// Why a certificate or a key was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// The text holds no certificate.
    #[error("no certificate in the PEM")]
    NoCertificate,
    /// The certificate is not an X.509 certificate.
    #[error("the certificate is not readable: {0}")]
    Certificate(String),
    /// The text holds no private key, or one that is not readable.
    #[error("the private key is not readable: {0}")]
    Key(String),
    /// The certificate has no `UID` in its subject, so no topic.
    #[error("the certificate has no topic (the UID of its subject)")]
    NoTopic,
    /// The key is not the one the certificate was made for.
    #[error("the key does not match the certificate")]
    KeyMismatch,
    /// The certificate ended before the time given.
    #[error("the certificate expired at unix time {not_after}")]
    Expired {
        /// When it ended.
        not_after: i64,
    },
    /// The topic does not start with [`TOPIC_PREFIX`].
    #[error("the topic is not an MDM push topic")]
    NotMdmTopic,
    /// A renewal names another topic than the certificate it renews.
    #[error("the topic is not the topic of the certificate in use")]
    TopicChanged,
}

/// What is read of a certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertInfo {
    /// The topic: the `UID` of the subject.
    pub topic: String,
    /// When it starts to be valid.
    pub not_before_unix: i64,
    /// When it stops being valid.
    pub not_after_unix: i64,
    /// The DER of its SubjectPublicKeyInfo.
    spki: Vec<u8>,
}

/// The certificates of `pem`, in order, as DER.
///
/// # Errors
/// [`IdentityError::NoCertificate`]: none. [`IdentityError::Certificate`]: a block that is not
/// PEM.
pub fn certificates_of(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, IdentityError> {
    let chain = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|why| IdentityError::Certificate(why.to_string()))?;
    if chain.is_empty() {
        return Err(IdentityError::NoCertificate);
    }
    Ok(chain)
}

/// What the first certificate of `pem` says.
///
/// # Errors
/// [`IdentityError::NoCertificate`], `Certificate`, `NoTopic`.
pub fn read_certificate(pem: &[u8]) -> Result<CertInfo, IdentityError> {
    let chain = certificates_of(pem)?;
    let (_, cert) = X509Certificate::from_der(chain[0].as_ref())
        .map_err(|why| IdentityError::Certificate(why.to_string()))?;
    let topic = topic_of(&cert).ok_or(IdentityError::NoTopic)?;
    Ok(CertInfo {
        topic,
        not_before_unix: cert.validity().not_before.timestamp(),
        not_after_unix: cert.validity().not_after.timestamp(),
        spki: cert.public_key().raw.to_vec(),
    })
}

/// The `UID` of the subject of `cert`.
fn topic_of(cert: &X509Certificate<'_>) -> Option<String> {
    let uid = x509_parser::der_parser::oid::Oid::from(&UID_OID).ok()?;
    cert.subject()
        .iter_attributes()
        .find(|attribute| *attribute.attr_type() == uid)
        .and_then(|attribute| attribute.as_str().ok())
        .map(str::to_owned)
}

/// The private key of `pem`, whichever of the PEM shapes it is.
///
/// # Errors
/// [`IdentityError::Key`]: no key, or one that is not readable.
pub fn read_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, IdentityError> {
    PrivateKeyDer::from_pem_slice(pem).map_err(|why| IdentityError::Key(why.to_string()))
}

/// The RSA key `key` is, if it is one.
pub(crate) fn rsa_key(key: &PrivateKeyDer<'_>) -> Result<RsaPrivateKey, IdentityError> {
    match key {
        PrivateKeyDer::Pkcs1(der) => RsaPrivateKey::from_pkcs1_der(der.secret_pkcs1_der())
            .map_err(|why| IdentityError::Key(why.to_string())),
        PrivateKeyDer::Pkcs8(der) => RsaPrivateKey::from_pkcs8_der(der.secret_pkcs8_der())
            .map_err(|why| IdentityError::Key(why.to_string())),
        _ => Err(IdentityError::Key("not an RSA key".to_owned())),
    }
}

impl CertInfo {
    /// Whether `key` is the private key of this certificate.
    ///
    /// # Errors
    /// [`IdentityError::Key`]: it is not an RSA key, or the certificate's key is not RSA.
    pub fn matches(&self, key: &PrivateKeyDer<'_>) -> Result<bool, IdentityError> {
        let mine = RsaPublicKey::from_public_key_der(&self.spki)
            .map_err(|why| IdentityError::Certificate(why.to_string()))?;
        Ok(rsa_key(key)?.to_public_key() == mine)
    }

    /// Judges the certificate as a push certificate at `now`: not expired, a push topic, and on a
    /// renewal (`current_topic`, the topic of the certificate in use) the same topic.
    ///
    /// # Errors
    /// [`IdentityError::Expired`], `NotMdmTopic`, `TopicChanged`.
    pub fn judge(&self, now_unix: i64, current_topic: Option<&str>) -> Result<(), IdentityError> {
        if self.not_after_unix <= now_unix {
            return Err(IdentityError::Expired {
                not_after: self.not_after_unix,
            });
        }
        if !self.topic.starts_with(TOPIC_PREFIX) {
            return Err(IdentityError::NotMdmTopic);
        }
        match current_topic {
            Some(topic) if topic != self.topic => Err(IdentityError::TopicChanged),
            _ => Ok(()),
        }
    }
}

/// A push certificate, its chain and its key: what the APNs sender authenticates with.
pub struct PushIdentity {
    /// The certificate and what follows it in the PEM, as DER.
    pub chain: Vec<CertificateDer<'static>>,
    /// The key.
    pub key: PrivateKeyDer<'static>,
    /// What the certificate says.
    pub info: CertInfo,
}

impl PushIdentity {
    /// The identity of a certificate PEM and a key PEM that match.
    ///
    /// # Errors
    /// Any of [`IdentityError`] of reading either, and `KeyMismatch`.
    pub fn parse(certificate_pem: &[u8], key_pem: &[u8]) -> Result<Self, IdentityError> {
        let chain = certificates_of(certificate_pem)?;
        let info = read_certificate(certificate_pem)?;
        let key = read_key(key_pem)?;
        if !info.matches(&key)? {
            return Err(IdentityError::KeyMismatch);
        }
        Ok(Self { chain, key, info })
    }

    /// The pair as `rustls` takes a client identity.
    #[must_use]
    pub fn tls_identity(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (self.chain.clone(), self.key.clone_key())
    }
}

impl std::fmt::Debug for PushIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushIdentity")
            .field("topic", &self.info.topic)
            .field("not_after_unix", &self.info.not_after_unix)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
