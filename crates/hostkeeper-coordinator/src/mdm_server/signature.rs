//! The `Mdm-Signature` header of a request (`docs/design/coordinator-mdm.md`, section 4.1): a
//! detached CMS signature (RFC 5652 SignedData, base64) over the request body, made with the key of
//! the Mac's identity, which a profile with `SignMessage` makes the MDM client send. The live
//! enrollment profile has `SignMessage true` (spike `scep.md`), so a Mac may authenticate by this
//! alone.
//!
//! [`signer`] checks the signature against the certificate the message carries and answers that
//! certificate: it proves possession of the key, and the caller then asks the identity table
//! whether the coordinator issued exactly that certificate. Nothing here trusts a chain.
//!
//! What is read: one `SignerInfo`, with or without signed attributes (with them, the
//! `messageDigest` must be the digest of the body and the signature is over the attributes); a
//! digest of SHA-1, SHA-256, SHA-384 or SHA-512; an RSA PKCS#1 v1.5 signature (the algorithm
//! identifier is `rsaEncryption` or the `sha*WithRSAEncryption` of the digest). The signer is named
//! by issuer and serial or by key identifier.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_mdm_push::ber::{self, BerError, Tlv};
use ring::digest;
use ring::signature::{
    RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY, RSA_PKCS1_2048_8192_SHA256,
    RSA_PKCS1_2048_8192_SHA384, RSA_PKCS1_2048_8192_SHA512, RsaParameters, UnparsedPublicKey,
};
use x509_parser::extensions::ParsedExtension;

/// The longest header value read, in bytes (a SignedData with a few certificates is under 8 KiB).
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;

/// The most certificates a signature may carry.
const MAX_CERTIFICATES: usize = 8;

const SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const SHA1_WITH_RSA: &str = "1.2.840.113549.1.1.5";
const SHA256_WITH_RSA: &str = "1.2.840.113549.1.1.11";
const SHA384_WITH_RSA: &str = "1.2.840.113549.1.1.12";
const SHA512_WITH_RSA: &str = "1.2.840.113549.1.1.13";
const SHA1: &str = "1.3.14.3.2.26";
const SHA256: &str = "2.16.840.1.101.3.4.2.1";
const SHA384: &str = "2.16.840.1.101.3.4.2.2";
const SHA512: &str = "2.16.840.1.101.3.4.2.3";

/// Why a signature is refused. The text names the part, never a value of the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SignatureError {
    /// The header is not base64, is too long, or is not a CMS SignedData this reader takes.
    #[error("the signature is not a CMS signed-data message this server reads")]
    Encoding,
    /// The message names no signer certificate it carries.
    #[error("the signature carries no certificate of its signer")]
    NoSigner,
    /// The digest or signature algorithm is not one this reader takes.
    #[error("the signature uses an algorithm this server does not take")]
    Algorithm,
    /// The digest in the signed attributes is not the body's.
    #[error("the signed digest is not the digest of the body")]
    Digest,
    /// The signature does not verify under the signer's key.
    #[error("the signature does not verify")]
    Verify,
}

impl From<BerError> for SignatureError {
    fn from(_: BerError) -> Self {
        Self::Encoding
    }
}

/// A value and its exact encoding.
struct Raw<'a> {
    tlv: Tlv<'a>,
    bytes: &'a [u8],
}

/// The values of `content` one after the other, each with its bytes.
fn values(mut content: &[u8]) -> Result<Vec<Raw<'_>>, SignatureError> {
    let mut out = Vec::new();
    while !content.is_empty() {
        let (tlv, rest) = ber::read(content)?;
        out.push(Raw {
            tlv,
            bytes: &content[..content.len() - rest.len()],
        });
        content = rest;
    }
    Ok(out)
}

fn sequence<'a>(raw: &Raw<'a>) -> Result<Vec<Raw<'a>>, SignatureError> {
    if !raw.tlv.is_universal(ber::SEQUENCE) {
        return Err(SignatureError::Encoding);
    }
    values(raw.tlv.content)
}

fn oid_of(raw: &Raw<'_>) -> Result<String, SignatureError> {
    Ok(ber::oid(&raw.tlv)?)
}

/// The digest an object identifier names, and the RSA scheme that signs with it.
fn digest_of(
    oid: &str,
) -> Result<(&'static digest::Algorithm, &'static RsaParameters), SignatureError> {
    match oid {
        SHA1 => Ok((
            &digest::SHA1_FOR_LEGACY_USE_ONLY,
            &RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY,
        )),
        SHA256 => Ok((&digest::SHA256, &RSA_PKCS1_2048_8192_SHA256)),
        SHA384 => Ok((&digest::SHA384, &RSA_PKCS1_2048_8192_SHA384)),
        SHA512 => Ok((&digest::SHA512, &RSA_PKCS1_2048_8192_SHA512)),
        _ => Err(SignatureError::Algorithm),
    }
}

/// The RSA scheme of a signature algorithm identifier: `rsaEncryption` signs with the digest the
/// signer named (`by_digest`), a `sha*WithRSAEncryption` with its own.
fn scheme_of(
    oid: &str,
    by_digest: &'static RsaParameters,
) -> Result<&'static RsaParameters, SignatureError> {
    match oid {
        RSA_ENCRYPTION => Ok(by_digest),
        SHA1_WITH_RSA => Ok(&RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY),
        SHA256_WITH_RSA => Ok(&RSA_PKCS1_2048_8192_SHA256),
        SHA384_WITH_RSA => Ok(&RSA_PKCS1_2048_8192_SHA384),
        SHA512_WITH_RSA => Ok(&RSA_PKCS1_2048_8192_SHA512),
        _ => Err(SignatureError::Algorithm),
    }
}

/// Checks the detached signature in `header` over `body` and answers the DER of the certificate
/// that made it.
///
/// # Errors
/// [`SignatureError`]: the header is not a signature this reader takes, names no signer it
/// carries, or does not verify over `body`.
pub(crate) fn signer(body: &[u8], header: &str) -> Result<Vec<u8>, SignatureError> {
    if header.len() > MAX_HEADER_BYTES {
        return Err(SignatureError::Encoding);
    }
    let message = STANDARD
        .decode(header.trim())
        .map_err(|_| SignatureError::Encoding)?;
    let (info, rest) = ber::read(&message)?;
    if !rest.is_empty() || !info.is_universal(ber::SEQUENCE) {
        return Err(SignatureError::Encoding);
    }
    let parts = values(info.content)?;
    let [kind, wrapped] = parts.as_slice() else {
        return Err(SignatureError::Encoding);
    };
    if oid_of(kind)? != SIGNED_DATA || !wrapped.tlv.is_context(0) {
        return Err(SignatureError::Encoding);
    }
    let inner = values(wrapped.tlv.content)?;
    let [signed] = inner.as_slice() else {
        return Err(SignatureError::Encoding);
    };
    let fields = sequence(signed)?;
    // version, digestAlgorithms, encapContentInfo, [0] certificates, [1] crls, signerInfos.
    let certificates = fields
        .iter()
        .find(|field| field.tlv.is_context(0))
        .map(|field| values(field.tlv.content))
        .transpose()?
        .unwrap_or_default();
    if certificates.len() > MAX_CERTIFICATES {
        return Err(SignatureError::Encoding);
    }
    let signer_infos = fields
        .last()
        .filter(|field| field.tlv.is_universal(ber::SET))
        .ok_or(SignatureError::Encoding)?;
    let infos = values(signer_infos.tlv.content)?;
    let [info] = infos.as_slice() else {
        return Err(SignatureError::Encoding);
    };
    let info = sequence(info)?;
    verify(body, &info, &certificates)
}

/// Checks `info`, the fields of the one `SignerInfo`, over `body`.
fn verify(
    body: &[u8],
    info: &[Raw<'_>],
    certificates: &[Raw<'_>],
) -> Result<Vec<u8>, SignatureError> {
    // version, sid, digestAlgorithm, [0] signedAttrs, signatureAlgorithm, signature, [1] ...
    let [_version, sid, digest_algorithm, rest @ ..] = info else {
        return Err(SignatureError::Encoding);
    };
    let (attributes, rest) = match rest {
        [first, rest @ ..] if first.tlv.is_context(0) => (Some(first), rest),
        rest => (None, rest),
    };
    let [signature_algorithm, signature, ..] = rest else {
        return Err(SignatureError::Encoding);
    };
    let digest_oid = oid_of(
        sequence(digest_algorithm)?
            .first()
            .ok_or(SignatureError::Encoding)?,
    )?;
    let (algorithm, by_digest) = digest_of(&digest_oid)?;
    let signature_oid = oid_of(
        sequence(signature_algorithm)?
            .first()
            .ok_or(SignatureError::Encoding)?,
    )?;
    let scheme = scheme_of(&signature_oid, by_digest)?;
    let body_digest = digest::digest(algorithm, body);
    let signed: Vec<u8> = match attributes {
        None => body.to_vec(),
        Some(attributes) => {
            let found = values(attributes.tlv.content)?
                .iter()
                .map(sequence)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|attribute| {
                    attribute
                        .first()
                        .is_some_and(|name| oid_of(name).is_ok_and(|o| o == MESSAGE_DIGEST))
                })
                .and_then(|attribute| attribute.into_iter().nth(1))
                .ok_or(SignatureError::Digest)?;
            let [value] = values(found.tlv.content)?
                .try_into()
                .map_err(|_| SignatureError::Digest)?;
            let claimed = ber::octets(&value.tlv)?;
            if claimed != body_digest.as_ref() {
                return Err(SignatureError::Digest);
            }
            // The signature is over the attributes as a SET OF: the same bytes with the tag of a
            // SET in place of the context tag, which must be definite and so DER.
            if attributes.bytes.get(1) == Some(&0x80) {
                return Err(SignatureError::Encoding);
            }
            let mut set = attributes.bytes.to_vec();
            set[0] = 0x31;
            set
        }
    };
    let certificate = signer_certificate(sid, certificates)?;
    let (_, parsed) =
        x509_parser::parse_x509_certificate(certificate).map_err(|_| SignatureError::NoSigner)?;
    let key = parsed.public_key().subject_public_key.data.as_ref();
    let signature = ber::octets(&signature.tlv)?;
    UnparsedPublicKey::new(scheme, key)
        .verify(&signed, &signature)
        .map_err(|_| SignatureError::Verify)?;
    Ok(certificate.to_vec())
}

/// The certificate among `certificates` that `sid` (issuer and serial, or `[0]` key identifier)
/// names.
fn signer_certificate<'a>(
    sid: &Raw<'_>,
    certificates: &[Raw<'a>],
) -> Result<&'a [u8], SignatureError> {
    for certificate in certificates {
        let Ok((_, parsed)) = x509_parser::parse_x509_certificate(certificate.bytes) else {
            continue;
        };
        let named = if sid.tlv.is_universal(ber::SEQUENCE) {
            let parts = values(sid.tlv.content)?;
            let [issuer, serial] = parts.as_slice() else {
                return Err(SignatureError::Encoding);
            };
            parsed.tbs_certificate.issuer.as_raw() == issuer.bytes
                && trimmed(parsed.raw_serial()) == trimmed(serial.tlv.content)
        } else if sid.tlv.is_context(0) {
            let wanted = sid.tlv.content;
            parsed.extensions().iter().any(|extension| {
                matches!(
                    extension.parsed_extension(),
                    ParsedExtension::SubjectKeyIdentifier(id) if id.0 == wanted
                )
            })
        } else {
            return Err(SignatureError::Encoding);
        };
        if named {
            return Ok(certificate.bytes);
        }
    }
    Err(SignatureError::NoSigner)
}

/// An INTEGER's content without its leading zeros.
fn trimmed(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

#[cfg(test)]
#[path = "signature_tests.rs"]
mod tests;
