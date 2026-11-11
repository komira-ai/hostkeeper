//! The device-information blob of `/enroll` (`docs/design/coordinator-mdm.md`, 4.2).
//!
//! Setup Assistant posts a PKCS#7 `SignedData` whose content is a property list naming the device
//! (`SERIAL`, `UDID`, `PRODUCT`, `VERSION`, ...) and which the device's Apple-issued identity
//! signed. [`verify`] checks the signature over the bytes as received (the signed attributes are
//! hashed as sent, not re-encoded), the certificate chain from the signer to one of the roots given,
//! the validity of every certificate at the time given, and only then reads the plist. [`claims`]
//! reads the plist of a blob without checking anything: its answer is what a stranger said, and
//! the coordinator shows it to an operator and trusts it with nothing.
use std::time::SystemTime;

use cms::cert::IssuerAndSerialNumber;
use const_oid::ObjectIdentifier;
use der::asn1::ObjectIdentifier as Oid;
use der::{Any, Decode, Encode, Tag, TagNumber, Tagged};
use rsa::PublicKey;
use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey as RsaVerifyingKey};
use sha2::{Digest, Sha256, Sha384, Sha512};
use signature::Verifier;
use spki::DecodePublicKey;
use x509_cert::Certificate;

use crate::asn1::{Bad, R, as_set, children, children_of, decode};
use crate::oids;

const CONTEXT_0: Tag = Tag::ContextSpecific {
    constructed: true,
    number: TagNumber(0),
};
/// The longest chain walked from the signer to a root; a longer one is refused.
const MAX_CHAIN: usize = 6;

/// Who a blob says the device is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The serial number, as Apple spells it.
    pub serial: String,
    /// The UDID.
    pub udid: String,
    /// The product (`Mac14,13`), when the blob names one.
    pub product: Option<String>,
    /// The OS build (`26A434`), when the blob names one.
    pub version: Option<String>,
}

/// Why a blob was not read or not verified. The text names the part; it never holds a byte of the
/// blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The bytes are not a `SignedData` of the layout this crate reads.
    #[error("the blob is not readable: {0}")]
    Malformed(&'static str),
    /// A signature or digest algorithm this crate does not implement.
    #[error("the blob uses an algorithm this server does not implement: {0}")]
    Unsupported(&'static str),
    /// A signature, a digest or a certificate's signature does not verify.
    #[error("the blob does not verify: {0}")]
    BadSignature(&'static str),
    /// The signature is good and the chain does not reach a trusted root.
    #[error("the blob is not trusted: {0}")]
    Untrusted(&'static str),
    /// The plist does not name a serial and a UDID.
    #[error("the blob does not name a device: {0}")]
    NoDevice(&'static str),
}

impl From<Bad> for Error {
    fn from(Bad(what): Bad) -> Self {
        Self::Malformed(what)
    }
}

/// What the layout of the message gives, before anything is checked.
struct Parsed {
    content: Vec<u8>,
    certificates: Vec<Certificate>,
    signer: Signer,
}

struct Signer {
    sid: IssuerAndSerialNumber,
    digest: Oid,
    algorithm: Oid,
    signed_attributes: Option<Attributes>,
    signature: Vec<u8>,
}

struct Attributes {
    /// The DER of the `SET` the signature covers.
    signed: Vec<u8>,
    content_type: Option<Oid>,
    message_digest: Option<Vec<u8>>,
}

fn parse(blob: &[u8]) -> R<Parsed> {
    let top = children(blob, "message")?;
    let [info] = top.as_slice() else {
        return Err(Bad("message"));
    };
    let info = children_of(info, Tag::Sequence, "content info")?;
    let [kind, explicit] = info.as_slice() else {
        return Err(Bad("content info"));
    };
    if decode::<Oid>(kind, "content type")? != oids::SIGNED_DATA {
        return Err(Bad("not signed data"));
    }
    let wrapped = children_of(explicit, CONTEXT_0, "signed data")?;
    let [signed] = wrapped.as_slice() else {
        return Err(Bad("signed data"));
    };
    let parts = children_of(signed, Tag::Sequence, "signed data")?;
    let [_, _, encap, rest @ ..] = parts.as_slice() else {
        return Err(Bad("signed data"));
    };
    let encap = children_of(encap, Tag::Sequence, "encapsulated content")?;
    let [_, content] = encap.as_slice() else {
        return Err(Bad("no content"));
    };
    let content = children_of(content, CONTEXT_0, "content")?;
    let [octets] = content.as_slice() else {
        return Err(Bad("content"));
    };
    if octets.tag() != Tag::OctetString {
        return Err(Bad("content"));
    }
    let certificates = match rest.iter().find(|part| part.tag() == CONTEXT_0) {
        Some(set) => children(set.value(), "certificates")?
            .iter()
            .filter(|any| any.tag() == Tag::Sequence)
            .filter_map(|any| decode::<Certificate>(any, "certificate").ok())
            .collect(),
        None => Vec::new(),
    };
    let signers = rest.last().ok_or(Bad("no signer"))?;
    let signers = children_of(signers, Tag::Set, "signers")?;
    let [signer] = signers.as_slice() else {
        return Err(Bad("not one signer"));
    };
    Ok(Parsed {
        content: octets.value().to_vec(),
        certificates,
        signer: read_signer(signer)?,
    })
}

fn read_signer(signer: &Any) -> R<Signer> {
    let fields = children_of(signer, Tag::Sequence, "signer")?;
    let (sid, digest, rest) = match fields.as_slice() {
        [_, sid, digest, rest @ ..] => (sid, digest, rest),
        _ => return Err(Bad("signer")),
    };
    let (attributes, algorithm, signature) = match rest {
        [attributes, algorithm, signature, ..] if attributes.tag() == CONTEXT_0 => {
            (Some(attributes), algorithm, signature)
        }
        [algorithm, signature, ..] => (None, algorithm, signature),
        _ => return Err(Bad("signer")),
    };
    if signature.tag() != Tag::OctetString {
        return Err(Bad("signature"));
    }
    Ok(Signer {
        sid: decode(sid, "signer identifier")?,
        digest: algorithm_oid(digest)?,
        algorithm: algorithm_oid(algorithm)?,
        signed_attributes: attributes.map(read_attributes).transpose()?,
        signature: signature.value().to_vec(),
    })
}

fn algorithm_oid(any: &Any) -> R<Oid> {
    let parts = children_of(any, Tag::Sequence, "algorithm")?;
    let first = parts.first().ok_or(Bad("algorithm"))?;
    decode(first, "algorithm")
}

fn read_attributes(attributes: &Any) -> R<Attributes> {
    let mut out = Attributes {
        signed: as_set(attributes.value())?,
        content_type: None,
        message_digest: None,
    };
    for attribute in children(attributes.value(), "attributes")? {
        let parts = children_of(&attribute, Tag::Sequence, "attribute")?;
        let [oid, values] = parts.as_slice() else {
            return Err(Bad("attribute"));
        };
        let kind: Oid = decode(oid, "attribute")?;
        let values = children_of(values, Tag::Set, "attribute values")?;
        let first = values.first().ok_or(Bad("attribute values"))?;
        if kind == oids::CONTENT_TYPE {
            out.content_type = decode::<Oid>(first, "contentType").ok();
        } else if kind == oids::MESSAGE_DIGEST {
            out.message_digest = Some(first.value().to_vec());
        }
    }
    Ok(out)
}

/// The device the plist of `content` names.
fn device_of(content: &[u8]) -> Result<Device, Error> {
    let value = plist::Value::from_reader(std::io::Cursor::new(content))
        .map_err(|_| Error::NoDevice("the content is not a property list"))?;
    let dict = value
        .as_dictionary()
        .ok_or(Error::NoDevice("the plist is not a dictionary"))?;
    let text = |key: &str| {
        dict.get(key)
            .and_then(plist::Value::as_string)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(Device {
        serial: text("SERIAL").ok_or(Error::NoDevice("no SERIAL"))?,
        udid: text("UDID").ok_or(Error::NoDevice("no UDID"))?,
        product: text("PRODUCT"),
        version: text("VERSION"),
    })
}

/// What `blob` says, checked against nothing: the serial and UDID of its plist.
///
/// # Errors
/// [`Error::Malformed`]: not a `SignedData` that carries its content. [`Error::NoDevice`].
pub fn claims(blob: &[u8]) -> Result<Device, Error> {
    device_of(&parse(blob)?.content)
}

/// The device `blob` names, once its signature and chain are verified at `now` against `roots`
/// (each the DER of a certificate that is trusted as it stands).
///
/// # Errors
/// [`Error::Malformed`], [`Error::Unsupported`], [`Error::BadSignature`], [`Error::Untrusted`]
/// (the signer is not in the blob, a certificate is outside its validity, or the chain does not
/// reach a root) and [`Error::NoDevice`].
pub fn verify(blob: &[u8], roots: &[Vec<u8>], now: SystemTime) -> Result<Device, Error> {
    let parsed = parse(blob)?;
    let signer = &parsed.signer;
    let leaf = parsed
        .certificates
        .iter()
        .find(|cert| {
            let tbs = cert.tbs_certificate();
            *tbs.issuer() == signer.sid.issuer && *tbs.serial_number() == signer.sid.serial_number
        })
        .ok_or(Error::Untrusted(
            "the signer's certificate is not in the blob",
        ))?;
    let signed = match &signer.signed_attributes {
        Some(attributes) => {
            if attributes.content_type != Some(oids::DATA) {
                return Err(Error::BadSignature("contentType attribute"));
            }
            let wanted = digest(signer.digest, &parsed.content)?;
            if attributes.message_digest.as_deref() != Some(wanted.as_slice()) {
                return Err(Error::BadSignature("messageDigest attribute"));
            }
            &attributes.signed
        }
        None => &parsed.content,
    };
    verify_signature(
        leaf,
        signer.algorithm,
        Some(signer.digest),
        signed,
        &signer.signature,
    )?;
    let roots: Vec<Certificate> = roots
        .iter()
        .filter_map(|der| Certificate::from_der(der).ok())
        .collect();
    chain(leaf, &parsed.certificates, &roots, now)?;
    device_of(&parsed.content)
}

fn digest(oid: Oid, data: &[u8]) -> Result<Vec<u8>, Error> {
    match oid {
        oids::SHA1 => Ok(sha1::Sha1::digest(data).to_vec()),
        oids::SHA256 => Ok(Sha256::digest(data).to_vec()),
        oids::SHA384 => Ok(Sha384::digest(data).to_vec()),
        oids::SHA512 => Ok(Sha512::digest(data).to_vec()),
        _ => Err(Error::Unsupported("digest algorithm")),
    }
}

/// Walks from `leaf` to a root: each certificate must be inside its validity at `now` and signed by
/// the next, which is a certificate of the blob that is not a root, or a root.
fn chain(
    leaf: &Certificate,
    pool: &[Certificate],
    roots: &[Certificate],
    now: SystemTime,
) -> Result<(), Error> {
    let mut current = leaf;
    for _ in 0..MAX_CHAIN {
        valid_at(current, now)?;
        if roots.iter().any(|root| root == current) {
            return Ok(());
        }
        let tbs = current.tbs_certificate();
        let issuers = roots.iter().chain(pool).filter(|candidate| {
            candidate.tbs_certificate().subject() == tbs.issuer() && *candidate != current
        });
        let signed = tbs.to_der().ok().ok_or(Error::Malformed("certificate"))?;
        let signature = current.signature().as_bytes().unwrap_or_default();
        let algorithm = current.signature_algorithm().oid;
        let mut issuers = issuers;
        let next = issuers.find(|candidate| {
            verify_signature(candidate, algorithm, None, &signed, signature).is_ok()
        });
        current = next.ok_or(Error::Untrusted(
            "a certificate has no issuer that signed it",
        ))?;
    }
    Err(Error::Untrusted("the chain is too long"))
}

fn valid_at(cert: &Certificate, now: SystemTime) -> Result<(), Error> {
    let validity = cert.tbs_certificate().validity();
    let from = validity.not_before.to_system_time();
    let until = validity.not_after.to_system_time();
    if now < from || now > until {
        return Err(Error::Untrusted("a certificate is outside its validity"));
    }
    Ok(())
}

/// Checks `signature` over `message` against the key of `signer`, under `algorithm`; for
/// `rsaEncryption` and `id-ecPublicKey`, which name no hash, the digest algorithm of the signer
/// info `digest_hint`.
fn verify_signature(
    signer: &Certificate,
    algorithm: ObjectIdentifier,
    digest_hint: Option<ObjectIdentifier>,
    message: &[u8],
    signature: &[u8],
) -> Result<(), Error> {
    let spki = signer
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .ok()
        .ok_or(Error::Malformed("signer key"))?;
    let hash = match algorithm {
        oids::SHA1_WITH_RSA => oids::SHA1,
        oids::SHA256_WITH_RSA | oids::ECDSA_WITH_SHA256 => oids::SHA256,
        oids::SHA384_WITH_RSA => oids::SHA384,
        oids::SHA512_WITH_RSA => oids::SHA512,
        oids::RSA_ENCRYPTION | oids::EC_PUBLIC_KEY => {
            digest_hint.ok_or(Error::Unsupported("signature algorithm"))?
        }
        _ => return Err(Error::Unsupported("signature algorithm")),
    };
    let rsa = matches!(
        algorithm,
        oids::RSA_ENCRYPTION
            | oids::SHA1_WITH_RSA
            | oids::SHA256_WITH_RSA
            | oids::SHA384_WITH_RSA
            | oids::SHA512_WITH_RSA
    );
    let good = if rsa {
        verify_rsa(&spki, hash, message, signature)?
    } else {
        verify_ecdsa(&spki, hash, message, signature)?
    };
    if good {
        Ok(())
    } else {
        Err(Error::BadSignature("a signature does not verify"))
    }
}

fn verify_rsa(spki: &[u8], hash: Oid, message: &[u8], signature: &[u8]) -> Result<bool, Error> {
    let key = PublicKey::from_public_key_der(spki)
        .ok()
        .ok_or(Error::Unsupported("the signer's key is not RSA"))?;
    let Ok(signature) = RsaSignature::try_from(signature) else {
        return Ok(false);
    };
    Ok(match hash {
        oids::SHA1 => RsaVerifyingKey::<sha1::Sha1>::new(key)
            .verify(message, &signature)
            .is_ok(),
        oids::SHA256 => RsaVerifyingKey::<Sha256>::new(key)
            .verify(message, &signature)
            .is_ok(),
        oids::SHA384 => RsaVerifyingKey::<Sha384>::new(key)
            .verify(message, &signature)
            .is_ok(),
        oids::SHA512 => RsaVerifyingKey::<Sha512>::new(key)
            .verify(message, &signature)
            .is_ok(),
        _ => return Err(Error::Unsupported("digest algorithm")),
    })
}

fn verify_ecdsa(spki: &[u8], hash: Oid, message: &[u8], signature: &[u8]) -> Result<bool, Error> {
    if hash != oids::SHA256 {
        return Err(Error::Unsupported("ECDSA digest other than SHA-256"));
    }
    let key = p256::ecdsa::VerifyingKey::from_public_key_der(spki)
        .ok()
        .ok_or(Error::Unsupported("the signer's key is not P-256"))?;
    let Ok(signature) = p256::ecdsa::DerSignature::try_from(signature) else {
        return Ok(false);
    };
    Ok(key.verify(message, &signature).is_ok())
}
