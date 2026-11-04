//! The host's key: a P-256 key made on the host from the operating system's random bytes, kept
//! as PKCS#8 DER in the pki directory and never sent anywhere.
//!
//! Its signatures are deterministic (RFC 6979), so the PKCS#10 request made from one key is the
//! same bytes every time: a daemon that restarts reposts the same request.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{DerSignature, SigningKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use x509_cert::builder::Builder;
use x509_cert::der::Encode;
use x509_cert::name::Name;
use x509_cert::request::RequestBuilder;

use crate::wire::RequestId;

/// How many times random bytes are drawn for a key before giving up. A draw fails only when its
/// 32 bytes are not a P-256 scalar (zero or at least the group order), about once in 2^32 draws.
pub const KEY_DRAWS: usize = 8;

/// Random bytes for the host's key.
pub trait Entropy: Send {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError>;
}

/// The operating system had no random bytes to give.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the operating system gave no random bytes: {0}")]
pub struct EntropyError(pub String);

/// The operating system's random bytes (`getrandom`).
#[derive(Clone, Copy, Debug, Default)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        getrandom::fill(buf).map_err(|e| EntropyError(e.to_string()))
    }
}

/// Why the host key could not be made, read or used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Entropy(#[from] EntropyError),
    #[error("{KEY_DRAWS} draws of random bytes gave no P-256 key")]
    NoKey,
    #[error("the key on disk is not a P-256 PKCS#8 key: {0}")]
    Unreadable(String),
    #[error("the key could not be encoded: {0}")]
    Encode(String),
}

fn encode_error(e: impl std::fmt::Display) -> KeyError {
    KeyError::Encode(e.to_string())
}

/// The host's P-256 key.
pub(crate) struct HostKey {
    signing: SigningKey,
    spki_der: Vec<u8>,
}

impl HostKey {
    /// A new key from `entropy`.
    pub(crate) fn generate(entropy: &mut dyn Entropy) -> Result<HostKey, KeyError> {
        let mut bytes = [0u8; 32];
        for _ in 0..KEY_DRAWS {
            entropy.fill(&mut bytes)?;
            let drawn = SigningKey::from_slice(&bytes);
            bytes.fill(0);
            if let Ok(signing) = drawn {
                return HostKey::of(signing);
            }
        }
        Err(KeyError::NoKey)
    }

    /// The key whose PKCS#8 DER is `der`.
    pub(crate) fn from_pkcs8(der: &[u8]) -> Result<HostKey, KeyError> {
        let signing =
            SigningKey::from_pkcs8_der(der).map_err(|e| KeyError::Unreadable(e.to_string()))?;
        HostKey::of(signing)
    }

    fn of(signing: SigningKey) -> Result<HostKey, KeyError> {
        let spki = signing
            .verifying_key()
            .to_public_key_der()
            .map_err(encode_error)?;
        Ok(HostKey {
            spki_der: spki.as_bytes().to_vec(),
            signing,
        })
    }

    /// The key as PKCS#8 DER, for the pki directory.
    pub(crate) fn pkcs8_der(&self) -> Result<Vec<u8>, KeyError> {
        let der = self.signing.to_pkcs8_der().map_err(encode_error)?;
        Ok(der.as_bytes().to_vec())
    }

    /// The DER SubjectPublicKeyInfo of the key.
    pub(crate) fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    /// The id of an enrollment request for this key.
    pub(crate) fn request_id(&self) -> RequestId {
        RequestId::of_spki(&self.spki_der)
    }

    /// A PKCS#10 request (DER) for this key, with an empty subject: the CA names the host.
    pub(crate) fn csr_der(&self) -> Result<Vec<u8>, KeyError> {
        let builder = RequestBuilder::new(Name::default()).map_err(encode_error)?;
        let request = builder
            .build::<_, DerSignature>(&self.signing)
            .map_err(encode_error)?;
        request.to_der().map_err(encode_error)
    }

    /// An ECDSA P-256/SHA-256 signature (DER) over `message`.
    pub(crate) fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: DerSignature = self.signing.sign(message);
        signature.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches the encode error dropping its cause.
    #[test]
    fn an_encode_error_keeps_its_cause() {
        assert_eq!(
            encode_error("too long").to_string(),
            "the key could not be encoded: too long"
        );
    }

    /// Catches the operating system's random bytes being left unfilled.
    #[test]
    fn the_operating_system_fills_the_buffer() {
        let mut buf = [0u8; 32];
        OsEntropy.fill(&mut buf).expect("getrandom");
        assert_ne!(buf, [0u8; 32]);
    }
}
