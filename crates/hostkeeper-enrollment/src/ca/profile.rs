//! The profile a certificate request is held to (design 4.4 and 5.7).
//!
//! A request is accepted only when all of these hold:
//!
//! - it is one DER PKCS#10 request of at most [`MAX_CSR_BYTES`], with nothing after it;
//! - its key is an ECDSA P-256 key in uncompressed form; any other key type or curve is
//!   `key_not_p256`;
//! - its self-signature verifies as ECDSA P-256 with SHA-256 under that key;
//! - its subject is one attribute, a common name `host:<id>` where `<id>` matches the host-id
//!   grammar ([`HostId`]). The id is assigned at approval, so a daemon sends the placeholder
//!   `host:pending` (`pending` matches the grammar); the CA never takes the id from the
//!   request: the certificate's subject is always the approved host id.
//!
//! Requested extensions (subjectAltNames, key usage, extended key usage, basic constraints)
//! are allowed in the request and ignored: the CA never copies one into a certificate. No clock
//! is read here: a request has no validity.

use std::fmt;

use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::oid_registry::OID_X509_COMMON_NAME;
use x509_parser::prelude::FromDer;
use x509_parser::x509::X509Name;

use crate::names::{HostId, KeyHash};

/// The largest request accepted, in bytes. A P-256 request without extensions is about 250
/// bytes.
pub const MAX_CSR_BYTES: usize = 4096;

/// The first 27 bytes of every DER SubjectPublicKeyInfo of an uncompressed ECDSA P-256 key:
/// SEQUENCE { SEQUENCE { id-ecPublicKey, prime256v1 }, BIT STRING { 0x04, ... } }. The 64
/// bytes of the point's coordinates follow.
const P256_SPKI_PREFIX: [u8; 27] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
];

/// The length of that SubjectPublicKeyInfo.
const P256_SPKI_LEN: usize = 91;

/// Where the point (`0x04`, x, y) starts in it.
const P256_POINT_AT: usize = 26;

/// Why a request does not meet the profile. Closed: every refusal is one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The request is not one the CA reads (`bad_csr`).
    #[error("bad_csr: {0}")]
    BadCsr(CsrDefect),
    /// The key is not an uncompressed ECDSA P-256 key (`key_not_p256`).
    #[error("key_not_p256: the key is not an uncompressed ECDSA P-256 key")]
    KeyNotP256,
}

impl ProfileError {
    /// The refusal token the coordinator answers with.
    pub fn token(&self) -> &'static str {
        match self {
            ProfileError::BadCsr(_) => "bad_csr",
            ProfileError::KeyNotP256 => "key_not_p256",
        }
    }
}

/// What is wrong with a request that is `bad_csr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrDefect {
    /// Larger than [`MAX_CSR_BYTES`].
    TooLarge,
    /// Not one DER PKCS#10 request, or bytes after it.
    NotDer,
    /// The self-signature does not verify under the request's key.
    Signature,
    /// The subject is not exactly one common name `host:<id>` with a valid id.
    Subject,
}

impl fmt::Display for CsrDefect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CsrDefect::TooLarge => "the request is larger than 4096 bytes",
            CsrDefect::NotDer => "the request is not one DER PKCS#10 request",
            CsrDefect::Signature => "the request's self-signature does not verify",
            CsrDefect::Subject => "the subject is not one common name host:<id>",
        })
    }
}

/// An uncompressed ECDSA P-256 public key, as its DER SubjectPublicKeyInfo.
#[derive(Clone, PartialEq, Eq)]
pub struct P256Key {
    spki_der: Vec<u8>,
}

impl P256Key {
    /// The key whose DER SubjectPublicKeyInfo is `spki_der`.
    ///
    /// # Errors
    /// [`ProfileError::KeyNotP256`] when it is not an uncompressed P-256 key.
    pub fn from_spki(spki_der: &[u8]) -> Result<P256Key, ProfileError> {
        if spki_der.len() == P256_SPKI_LEN && spki_der.starts_with(&P256_SPKI_PREFIX) {
            return Ok(P256Key {
                spki_der: spki_der.to_vec(),
            });
        }
        Err(ProfileError::KeyNotP256)
    }

    /// The DER SubjectPublicKeyInfo.
    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    /// The SHA-256 of the SubjectPublicKeyInfo: the id of a request for this key.
    pub fn key_hash(&self) -> KeyHash {
        KeyHash::of_spki(&self.spki_der)
    }

    /// The point, `0x04` followed by x and y.
    pub fn point(&self) -> &[u8] {
        &self.spki_der[P256_POINT_AT..]
    }

    /// Whether `signature_der` is an ECDSA P-256 / SHA-256 signature (ASN.1 DER) of `message`
    /// by this key.
    pub fn verify(&self, message: &[u8], signature_der: &[u8]) -> bool {
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            self.point(),
        )
        .verify(message, signature_der)
        .is_ok()
    }
}

impl fmt::Debug for P256Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P256Key({})", self.key_hash())
    }
}

/// A request that meets the profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRequest {
    key: P256Key,
    subject: HostId,
}

impl VerifiedRequest {
    /// The request's key.
    pub fn key(&self) -> &P256Key {
        &self.key
    }

    /// The id the request's subject names (`pending` for a daemon not yet approved). Never
    /// used as the certificate's subject.
    pub fn subject_id(&self) -> &HostId {
        &self.subject
    }
}

/// Holds `csr_der` to the profile.
///
/// # Errors
/// [`ProfileError`]: the first rule the request breaks, in the order the module lists them.
pub fn check_request(csr_der: &[u8]) -> Result<VerifiedRequest, ProfileError> {
    if csr_der.len() > MAX_CSR_BYTES {
        return Err(ProfileError::BadCsr(CsrDefect::TooLarge));
    }
    let csr = X509CertificationRequest::from_der(csr_der)
        .ok()
        .filter(|(rest, _)| rest.is_empty())
        .map(|(_, csr)| csr)
        .ok_or(ProfileError::BadCsr(CsrDefect::NotDer))?;
    let info = &csr.certification_request_info;
    let key = P256Key::from_spki(info.subject_pki.raw)?;
    if !key.verify(info.raw, &csr.signature_value.data) {
        return Err(ProfileError::BadCsr(CsrDefect::Signature));
    }
    let subject = subject_id(&info.subject).ok_or(ProfileError::BadCsr(CsrDefect::Subject))?;
    Ok(VerifiedRequest { key, subject })
}

/// The id of a subject that is exactly one common name `host:<id>`.
fn subject_id(name: &X509Name<'_>) -> Option<HostId> {
    let attributes: Vec<_> = name.iter_attributes().collect();
    let [only] = attributes.as_slice() else {
        return None;
    };
    let text = only.as_str().ok()?.strip_prefix("host:")?;
    HostId::parse(text)
        .ok()
        .filter(|_| *only.attr_type() == OID_X509_COMMON_NAME)
}
