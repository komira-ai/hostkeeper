//! The `CertRep`: a CMS `SignedData` by the CA, whose content, on success, is the issued
//! certificate and the CA's in a certs-only `SignedData`, enveloped to the sender's certificate.
use cms::builder::{
    ContentEncryptionAlgorithm, EnvelopedDataBuilder, KeyEncryptionInfo,
    KeyTransRecipientInfoBuilder, SignedDataBuilder, SignerInfoBuilder,
};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::enveloped_data::RecipientIdentifier;
use cms::signed_data::{EncapsulatedContentInfo, SignerIdentifier};
use const_oid::ObjectIdentifier;
use der::asn1::{PrintableString, SetOfVec};
use der::{Any, Decode, Encode, Tag};
use rsa::PublicKey;
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::rand_core::CryptoRng;
use sha2::{Digest, Sha256};
use spki::{AlgorithmIdentifierOwned, DecodePublicKey};
use x509_cert::Certificate;
use x509_cert::attr::{Attribute, AttributeValue};

use crate::envelope::Cipher;
use crate::oids;
use crate::request::{Message, Outcome};
use crate::{Ca, Error, Refusal, Reply, Verdict};

const FAIL_BAD_ALG: &str = "0";
const FAIL_BAD_MESSAGE_CHECK: &str = "1";
const FAIL_BAD_REQUEST: &str = "2";
const STATUS_SUCCESS: &str = "0";
const STATUS_FAILURE: &str = "2";
const CERT_REP: &str = "3";
const NONCE_LEN: usize = 16;

fn fail_info(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::BadAlg(_) => FAIL_BAD_ALG,
        Refusal::BadMessage(_) => FAIL_BAD_MESSAGE_CHECK,
        Refusal::BadRequest(_)
        | Refusal::ChallengeUnknown
        | Refusal::ChallengeConsumed
        | Refusal::IssueFailed => FAIL_BAD_REQUEST,
    }
}

pub(crate) fn build_reply(
    ca: &Ca,
    message: &Message,
    outcome: Outcome,
    rng: &mut impl CryptoRng,
) -> Result<Reply, Error> {
    let (status, fail, content, verdict) = match outcome {
        Outcome::Certificate {
            der,
            replayed,
            cipher,
        } => {
            let signer = message
                .signer
                .as_ref()
                .ok_or(Error::Malformed("the signer's certificate"))?;
            let sealed = seal(ca, &der, signer, cipher, rng)?;
            let verdict = if replayed {
                Verdict::Replayed
            } else {
                Verdict::Issued
            };
            (STATUS_SUCCESS, None, Some(sealed), verdict)
        }
        Outcome::Refused(refusal) => (
            STATUS_FAILURE,
            Some(fail_info(refusal)),
            None,
            Verdict::Refused(refusal),
        ),
    };
    let der = sign(ca, message, status, fail, content, rng)?;
    Ok(Reply { der, verdict })
}

/// The certs-only `SignedData` of the issued certificate and the CA's, enveloped to `to`, as the
/// DER of a `ContentInfo`.
fn seal(
    ca: &Ca,
    issued: &[u8],
    to: &Certificate,
    cipher: Cipher,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, Error> {
    let empty = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        econtent: None,
    };
    let issued = Certificate::from_der(issued)
        .ok()
        .ok_or(Error::Build("the certificates"))?;
    let certs_only = SignedDataBuilder::new(&empty)
        .add_certificate(CertificateChoices::Certificate(issued))
        .ok()
        .ok_or(Error::Build("the certificates"))?
        .add_certificate(CertificateChoices::Certificate(ca.certificate().clone()))
        .ok()
        .ok_or(Error::Build("the certificates"))?
        .build()
        .ok()
        .and_then(|signed| signed.to_der().ok())
        .ok_or(Error::Build("the certificates"))?;
    let spki = to
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .ok()
        .ok_or(Error::Build("the recipient"))?;
    let key = PublicKey::from_public_key_der(&spki)
        .ok()
        .ok_or(Error::Build("the recipient"))?;
    let recipient = RecipientIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: to.tbs_certificate().issuer().clone(),
        serial_number: to.tbs_certificate().serial_number().clone(),
    });
    let recipient = KeyTransRecipientInfoBuilder::new(recipient, KeyEncryptionInfo::Rsa(key))
        .ok()
        .ok_or(Error::Build("the recipient"))?;
    let algorithm = match cipher {
        Cipher::Aes192 => ContentEncryptionAlgorithm::Aes192Cbc,
        Cipher::Aes256 => ContentEncryptionAlgorithm::Aes256Cbc,
        Cipher::Aes128 | Cipher::Des3 => ContentEncryptionAlgorithm::Aes128Cbc,
    };
    let enveloped = EnvelopedDataBuilder::new(None, &certs_only, algorithm, None)
        .ok()
        .ok_or(Error::Build("the envelope"))?
        .add_recipient_info(recipient)
        .ok()
        .ok_or(Error::Build("the envelope"))?
        .build_with_rng(rng)
        .ok()
        .ok_or(Error::Build("the envelope"))?
        .to_der()
        .ok()
        .ok_or(Error::Build("the envelope"))?;
    ContentInfo {
        content_type: const_oid::db::rfc5911::ID_ENVELOPED_DATA,
        content: Any::from_der(&enveloped)
            .ok()
            .ok_or(Error::Build("the envelope"))?,
    }
    .to_der()
    .ok()
    .ok_or(Error::Build("the envelope"))
}

fn attribute(oid: ObjectIdentifier, value: Any) -> Result<Attribute, Error> {
    let mut values: SetOfVec<AttributeValue> = SetOfVec::default();
    values
        .insert(value)
        .ok()
        .ok_or(Error::Build("a signed attribute"))?;
    Ok(Attribute { oid, values })
}

fn printable(text: &str) -> Result<Any, Error> {
    let text = PrintableString::new(text)
        .ok()
        .ok_or(Error::Build("a signed attribute"))?;
    Ok(Any::from(&text))
}

fn octets(bytes: &[u8]) -> Result<Any, Error> {
    Any::new(Tag::OctetString, bytes)
        .ok()
        .ok_or(Error::Build("a signed attribute"))
}

fn sign(
    ca: &Ca,
    message: &Message,
    status: &str,
    fail: Option<&str>,
    content: Option<Vec<u8>>,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, Error> {
    let content_info = EncapsulatedContentInfo {
        econtent_type: const_oid::db::rfc5911::ID_DATA,
        econtent: match &content {
            Some(bytes) => Some(octets(bytes)?),
            None => None,
        },
    };
    let digest_algorithm = AlgorithmIdentifierOwned {
        oid: oids::SHA256,
        parameters: None,
    };
    let cert = ca.certificate().tbs_certificate();
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: cert.issuer().clone(),
        serial_number: cert.serial_number().clone(),
    });
    // The builder adds `contentType` and `messageDigest` only when there is content; a failure
    // reply has none and RFC 5652 section 5.3 wants both whenever there are signed attributes, so
    // it is given the digest of the empty content.
    let empty = Sha256::digest(b"");
    let external = content.is_none().then_some(empty.as_slice());
    let mut signer_info =
        SignerInfoBuilder::new(sid, digest_algorithm.clone(), &content_info, external)
            .ok()
            .ok_or(Error::Build("the signer"))?;
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    let mut attributes = vec![
        attribute(oids::MESSAGE_TYPE, printable(CERT_REP)?)?,
        attribute(oids::PKI_STATUS, printable(status)?)?,
    ];
    if let Some(fail) = fail {
        attributes.push(attribute(oids::FAIL_INFO, printable(fail)?)?);
    }
    attributes.push(attribute(oids::TRANSACTION_ID, message.tid.clone())?);
    attributes.push(attribute(oids::SENDER_NONCE, octets(&nonce)?)?);
    attributes.push(attribute(oids::RECIPIENT_NONCE, octets(&message.nonce)?)?);
    for attr in attributes {
        signer_info
            .add_signed_attribute(attr)
            .ok()
            .ok_or(Error::Build("the signer"))?;
    }
    let key = SigningKey::<Sha256>::new(ca.private_key().clone());
    SignedDataBuilder::new(&content_info)
        .add_digest_algorithm(digest_algorithm)
        .ok()
        .ok_or(Error::Build("the reply"))?
        .add_certificate(CertificateChoices::Certificate(ca.certificate().clone()))
        .ok()
        .ok_or(Error::Build("the reply"))?
        .add_signer_info::<SigningKey<Sha256>, Signature>(signer_info, &key)
        .ok()
        .ok_or(Error::Build("the reply"))?
        .build()
        .ok()
        .ok_or(Error::Build("the reply"))?
        .to_der()
        .ok()
        .ok_or(Error::Build("the reply"))
}
