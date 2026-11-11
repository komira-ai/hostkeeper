//! The request for a push certificate (`docs/design/coordinator-mdm.md`, section 4.4): the RSA
//! key of the certificate with a CSR over it, and the key and self-signed certificate mdmcert.download
//! encrypts its reply to, in the shapes the service's own client (`mdmctl mdmcert.download -new`,
//! micromdm) makes:
//!
//! - the push key is RSA 2048, and the CSR is signed by it (SHA-256 with RSA) with the subject
//!   `C=US`, `CN=mdm-push` and the email as `emailAddress`;
//! - the encryption certificate is self-signed over a second RSA 2048 key, `CN=mdmcert.download`,
//!   valid 365 days, with the key usages `keyEncipherment` and `digitalSignature` and the server
//!   authentication extended usage.
//!
//! Keys are written as PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`), as the migrated key is.

use rand_core::OsRng;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, DnValue, ExtendedKeyUsagePurpose, KeyPair,
    KeyUsagePurpose, PKCS_RSA_SHA256, SanType,
};
use rsa09::RsaPrivateKey;
use rsa09::pkcs1::EncodeRsaPrivateKey as _;
use rsa09::pkcs8::{EncodePrivateKey as _, LineEnding};
use rustls::pki_types::PrivatePkcs8KeyDer;

use crate::KeyBytes;

/// The size of both RSA keys, in bits.
pub const KEY_BITS: usize = 2048;

/// How long the encryption certificate is valid.
const ENCRYPTION_CERT_DAYS: i64 = 365;

/// The `emailAddress` attribute of a subject.
const EMAIL_OID: [u64; 7] = [1, 2, 840, 113_549, 1, 9, 1];

/// Why a request could not be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The email is not one address.
    #[error("the email address is not valid")]
    Email,
    /// A key or a signature could not be made.
    #[error("the request could not be made: {0}")]
    Crypto(String),
}

/// A push certificate request, made.
pub struct PushRequest {
    /// The push key, PKCS#1 PEM. Secret.
    pub key_pem: KeyBytes,
    /// The CSR, PEM: what mdmcert.download signs and what a vendor signs.
    pub csr_pem: String,
    /// The key mdmcert.download's reply is encrypted to, PKCS#1 PEM. Secret.
    pub encryption_key_pem: KeyBytes,
    /// Its self-signed certificate, PEM: public.
    pub encryption_cert_pem: String,
}

impl std::fmt::Debug for PushRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushRequest").finish_non_exhaustive()
    }
}

fn crypto(why: impl std::fmt::Debug) -> RequestError {
    RequestError::Crypto(format!("{why:?}"))
}

/// Whether `email` is one plain address: `local@domain`, no space, control or quote character,
/// no second `@`, and 254 bytes at most.
#[must_use]
pub fn email_is_valid(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    email.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && email
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '"' | '<' | '>' | ','))
}

/// A new RSA 2048 key.
///
/// # Errors
/// [`RequestError::Crypto`]: the key could not be generated.
pub fn new_rsa_key() -> Result<RsaPrivateKey, RequestError> {
    RsaPrivateKey::new(&mut OsRng, KEY_BITS).map_err(crypto)
}

/// The PKCS#1 PEM of `key`.
///
/// # Errors
/// [`RequestError::Crypto`].
pub fn pkcs1_pem(key: &RsaPrivateKey) -> Result<KeyBytes, RequestError> {
    key.to_pkcs1_pem(LineEnding::LF)
        .map(|pem| KeyBytes::new(pem.as_bytes().to_vec()))
        .map_err(crypto)
}

/// `key` as the signing key of rcgen.
fn signing_key(key: &RsaPrivateKey) -> Result<KeyPair, RequestError> {
    let der = key.to_pkcs8_der().map_err(crypto)?;
    KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.as_bytes().to_vec()),
        &PKCS_RSA_SHA256,
    )
    .map_err(crypto)
}

/// The CSR of `key` for `email`, PEM.
///
/// # Errors
/// [`RequestError::Email`], `Crypto`.
pub fn csr_pem(key: &RsaPrivateKey, email: &str) -> Result<String, RequestError> {
    if !email_is_valid(email) {
        return Err(RequestError::Email);
    }
    let mut name = DistinguishedName::new();
    name.push(DnType::CountryName, "US");
    name.push(DnType::CommonName, "mdm-push");
    name.push(
        DnType::CustomDnType(EMAIL_OID.to_vec()),
        DnValue::Utf8String(email.to_owned()),
    );
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    params
        .serialize_request(&signing_key(key)?)
        .and_then(|csr| csr.pem())
        .map_err(crypto)
}

/// The self-signed encryption certificate of `key`, valid from `now_unix` for a year, PEM.
///
/// # Errors
/// [`RequestError::Crypto`].
pub fn encryption_cert_pem(key: &RsaPrivateKey, now_unix: i64) -> Result<String, RequestError> {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "mdmcert.download");
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    params.subject_alt_names = vec![SanType::DnsName(
        "mdmcert.download".try_into().map_err(crypto)?,
    )];
    params.not_before = time::OffsetDateTime::from_unix_timestamp(now_unix).map_err(crypto)?;
    params.not_after = time::OffsetDateTime::from_unix_timestamp(
        now_unix.saturating_add(ENCRYPTION_CERT_DAYS * 86_400),
    )
    .map_err(crypto)?;
    params.key_usages = vec![
        KeyUsagePurpose::KeyEncipherment,
        KeyUsagePurpose::DigitalSignature,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params
        .self_signed(&signing_key(key)?)
        .map(|cert| cert.pem())
        .map_err(crypto)
}

/// A push certificate request for `email`, with its two new keys, at `now_unix`.
///
/// # Errors
/// [`RequestError::Email`]: refused before any key is made. [`RequestError::Crypto`].
pub fn new_request(email: &str, now_unix: i64) -> Result<PushRequest, RequestError> {
    if !email_is_valid(email) {
        return Err(RequestError::Email);
    }
    let push_key = new_rsa_key()?;
    let encryption_key = new_rsa_key()?;
    Ok(PushRequest {
        csr_pem: csr_pem(&push_key, email)?,
        key_pem: pkcs1_pem(&push_key)?,
        encryption_cert_pem: encryption_cert_pem(&encryption_key, now_unix)?,
        encryption_key_pem: pkcs1_pem(&encryption_key)?,
    })
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
