//! A `PKIOperation` request: parsed from the bytes as sent, verified, opened, redeemed.
use std::time::SystemTime;

use cms::cert::IssuerAndSerialNumber;
use const_oid::ObjectIdentifier;
use der::asn1::ObjectIdentifier as Oid;
use der::{Any, Encode, Tag, TagNumber, Tagged};
use rsa::PublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::rand_core::CryptoRng;
use sha2::{Digest, Sha256};
use signature::Verifier;
use spki::DecodePublicKey;
use x509_cert::Certificate;
use x509_cert::request::CertReq;

use crate::asn1::{Bad, R, as_set, children, children_of, decode};
use crate::envelope::{self, Cipher, Opened};
use crate::oids;
use crate::{Ca, Challenges, Error, KeyHash, Redeem, Refusal};

const PKCS_REQ: &str = "19";
const SIGNED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.2");
const CONTEXT_0: Tag = Tag::ContextSpecific {
    constructed: true,
    number: TagNumber(0),
};

/// A request as received: what the sender signed, and what it said.
pub(crate) struct Message {
    /// The `transactionID` attribute as sent, echoed as sent.
    pub tid: Any,
    /// The `senderNonce`, echoed as the `recipientNonce`.
    pub nonce: Vec<u8>,
    /// The certificate the sender signed with and wants the answer enveloped to.
    pub signer: Option<Certificate>,
    message_type: Option<Vec<u8>>,
    econtent: Vec<u8>,
    digest: Oid,
    signature_algorithm: Oid,
    signed_attributes: Vec<u8>,
    message_digest: Option<Vec<u8>>,
    content_type: Option<Oid>,
    signature: Vec<u8>,
}

struct Attributes(Vec<(Oid, Any)>);

impl Attributes {
    fn get(&self, oid: ObjectIdentifier) -> Option<&Any> {
        self.0.iter().find(|(o, _)| *o == oid).map(|(_, v)| v)
    }
}

impl Message {
    /// Read the layout of the message; nothing is verified here. An error is a message from which
    /// no reply can be made: its layout, or its `transactionID`, is missing.
    pub(crate) fn parse(body: &[u8]) -> Result<Self, Error> {
        Self::layout(body).map_err(|Bad(what)| Error::Malformed(what))
    }

    fn layout(body: &[u8]) -> R<Self> {
        let top = children(body, "message")?;
        let [info] = top.as_slice() else {
            return Err(Bad("message"));
        };
        let info = children_of(info, Tag::Sequence, "content info")?;
        let [kind, explicit] = info.as_slice() else {
            return Err(Bad("content info"));
        };
        if decode::<Oid>(kind, "content type")? != SIGNED_DATA {
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
        let econtent = octets.value().to_vec();

        let certificates = rest.iter().find(|part| part.tag() == CONTEXT_0);
        let certificates = match certificates {
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
        let fields = children_of(signer, Tag::Sequence, "signer")?;
        let [_, sid, digest, attributes, algorithm, signature] = fields.as_slice() else {
            return Err(Bad("signer"));
        };
        let sid: IssuerAndSerialNumber = decode(sid, "signer identifier")?;
        let digest = algorithm_oid(digest)?;
        let signature_algorithm = algorithm_oid(algorithm)?;
        if signature.tag() != Tag::OctetString {
            return Err(Bad("signature"));
        }
        if attributes.tag() != CONTEXT_0 {
            return Err(Bad("no signed attributes"));
        }
        let attrs = read_attributes(attributes.value())?;
        let tid = attrs
            .get(oids::TRANSACTION_ID)
            .cloned()
            .ok_or(Bad("no transactionID"))?;
        let nonce = attrs
            .get(oids::SENDER_NONCE)
            .map(|v| v.value().to_vec())
            .ok_or(Bad("no senderNonce"))?;
        Ok(Self {
            signer: certificates.into_iter().find(|c| same_signer(c, &sid)),
            tid,
            nonce,
            message_type: attrs.get(oids::MESSAGE_TYPE).map(|v| v.value().to_vec()),
            econtent,
            digest,
            signature_algorithm,
            signed_attributes: as_set(attributes.value())?,
            message_digest: attrs.get(oids::MESSAGE_DIGEST).map(|v| v.value().to_vec()),
            content_type: attrs
                .get(oids::CONTENT_TYPE)
                .and_then(|v| decode::<Oid>(v, "contentType").ok()),
            signature: signature.value().to_vec(),
        })
    }
}

fn algorithm_oid(any: &Any) -> R<Oid> {
    let parts = children_of(any, Tag::Sequence, "algorithm")?;
    let first = parts.first().ok_or(Bad("algorithm"))?;
    decode(first, "algorithm")
}

fn read_attributes(value: &[u8]) -> R<Attributes> {
    let mut out = Vec::new();
    for attribute in children(value, "attributes")? {
        let parts = children_of(&attribute, Tag::Sequence, "attribute")?;
        let [oid, values] = parts.as_slice() else {
            return Err(Bad("attribute"));
        };
        let values = children_of(values, Tag::Set, "attribute values")?;
        let first = values.first().ok_or(Bad("attribute values"))?;
        out.push((decode::<Oid>(oid, "attribute")?, first.clone()));
    }
    Ok(Attributes(out))
}

fn same_signer(cert: &Certificate, sid: &IssuerAndSerialNumber) -> bool {
    let tbs = cert.tbs_certificate();
    *tbs.issuer() == sid.issuer && *tbs.serial_number() == sid.serial_number
}

/// How a request ended, before the reply is built.
pub(crate) enum Outcome {
    Certificate {
        der: Vec<u8>,
        replayed: bool,
        cipher: Cipher,
    },
    Refused(Refusal),
}

pub(crate) fn process(
    ca: &Ca,
    message: &Message,
    store: &mut dyn Challenges,
    now: SystemTime,
    rng: &mut impl CryptoRng,
) -> Outcome {
    match decide(ca, message, store, now, rng) {
        Ok(outcome) => outcome,
        Err(refusal) => Outcome::Refused(refusal),
    }
}

fn decide(
    ca: &Ca,
    message: &Message,
    store: &mut dyn Challenges,
    now: SystemTime,
    rng: &mut impl CryptoRng,
) -> Result<Outcome, Refusal> {
    verify(message).map_err(|Bad(why)| Refusal::BadMessage(why))?;
    if message.message_type.as_deref() != Some(PKCS_REQ.as_bytes()) {
        return Err(Refusal::BadRequest("a message type that is not PKCSReq"));
    }
    let (plain, cipher) =
        envelope::open(&message.econtent, ca.private_key(), rng).map_err(|e| match e {
            Opened::Alg(why) => Refusal::BadAlg(why),
            Opened::Bad(Bad(why)) => Refusal::BadMessage(why),
        })?;
    let request = CertRequest::read(&plain).map_err(|Bad(why)| Refusal::BadMessage(why))?;
    if request.key_bits < ca.policy().min_key_bits {
        return Err(Refusal::BadRequest("a key shorter than the policy's"));
    }
    let challenge = request
        .challenge
        .as_deref()
        .ok_or(Refusal::BadRequest("no challenge"))?;
    let mut issue = |serial: &str| ca.issue(serial, &request.spki, now, rng);
    match store.redeem(challenge, request.key_hash, &mut issue) {
        Redeem::Issued(der) => Ok(Outcome::Certificate {
            der,
            replayed: false,
            cipher,
        }),
        Redeem::Replayed(der) => Ok(Outcome::Certificate {
            der,
            replayed: true,
            cipher,
        }),
        Redeem::Consumed => Err(Refusal::ChallengeConsumed),
        Redeem::Unknown => Err(Refusal::ChallengeUnknown),
        Redeem::Failed(_) => Err(Refusal::IssueFailed),
    }
}

/// The signature over the signed attributes as received, and the attributes' bond to the content.
fn verify(message: &Message) -> R<()> {
    let signer = message
        .signer
        .as_ref()
        .ok_or(Bad("the signer's certificate is not in the message"))?;
    if message.content_type != Some(const_oid::db::rfc5911::ID_DATA) {
        return Err(Bad("contentType attribute"));
    }
    let hash = Hash::of(message.digest)?;
    let wanted = hash.digest(&message.econtent);
    if message.message_digest.as_deref() != Some(wanted.as_slice()) {
        return Err(Bad("messageDigest attribute"));
    }
    if !matches!(
        message.signature_algorithm,
        oids::RSA_ENCRYPTION | oids::SHA1_WITH_RSA | oids::SHA256_WITH_RSA
    ) {
        return Err(Bad("signature algorithm"));
    }
    let spki = signer
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .ok()
        .ok_or(Bad("signer key"))?;
    let key = PublicKey::from_public_key_der(&spki)
        .ok()
        .ok_or(Bad("signer key is not RSA"))?;
    verify_rsa(hash, key, &message.signed_attributes, &message.signature)
}

/// The hash a signature or a digest attribute names.
#[derive(Clone, Copy)]
enum Hash {
    Sha1,
    Sha256,
}

impl Hash {
    fn of(oid: Oid) -> R<Self> {
        match oid {
            oids::SHA256 => Ok(Self::Sha256),
            oids::SHA1 => Ok(Self::Sha1),
            _ => Err(Bad("digest algorithm")),
        }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha1 => sha1::Sha1::digest(data).to_vec(),
        }
    }

    fn verify(self, key: PublicKey, message: &[u8], signature: &Signature) -> bool {
        match self {
            Self::Sha256 => VerifyingKey::<Sha256>::new(key)
                .verify(message, signature)
                .is_ok(),
            Self::Sha1 => VerifyingKey::<sha1::Sha1>::new(key)
                .verify(message, signature)
                .is_ok(),
        }
    }
}

fn verify_rsa(hash: Hash, key: PublicKey, message: &[u8], signature: &[u8]) -> R<()> {
    let signature = Signature::try_from(signature)
        .ok()
        .ok_or(Bad("signature"))?;
    if hash.verify(key, message, &signature) {
        Ok(())
    } else {
        Err(Bad("signature does not verify"))
    }
}

/// The certificate request inside the envelope.
struct CertRequest {
    spki: spki::SubjectPublicKeyInfoOwned,
    key_hash: KeyHash,
    key_bits: usize,
    challenge: Option<String>,
}

impl CertRequest {
    fn read(plain: &[u8]) -> R<Self> {
        let parts = children(plain, "certificate request")?;
        let [outer] = parts.as_slice() else {
            return Err(Bad("certificate request"));
        };
        let fields = children_of(outer, Tag::Sequence, "certificate request")?;
        let [info, ..] = fields.as_slice() else {
            return Err(Bad("certificate request"));
        };
        let csr: CertReq = decode(outer, "certificate request")?;
        let spki_der = csr
            .info
            .public_key
            .to_der()
            .ok()
            .ok_or(Bad("request key"))?;
        let key = PublicKey::from_public_key_der(&spki_der)
            .ok()
            .ok_or(Bad("request key is not RSA"))?;
        let digest = match csr.algorithm.oid {
            oids::SHA256_WITH_RSA => Hash::Sha256,
            oids::SHA1_WITH_RSA => Hash::Sha1,
            _ => return Err(Bad("request signature algorithm")),
        };
        let signed = info.to_der().ok().ok_or(Bad("certificate request"))?;
        verify_rsa(digest, key.clone(), &signed, csr.signature.raw_bytes())
            .ok()
            .ok_or(Bad("the request's own signature does not verify"))?;
        let challenge = csr
            .info
            .attributes
            .iter()
            .find(|a| a.oid == oids::CHALLENGE_PASSWORD)
            .and_then(|a| a.values.iter().next())
            .and_then(|v| std::str::from_utf8(v.value()).ok())
            .map(str::to_owned);
        Ok(Self {
            key_hash: Sha256::digest(&spki_der).into(),
            key_bits: rsa::traits::PublicKeyParts::n(&key).bit_width() as usize,
            spki: csr.info.public_key,
            challenge,
        })
    }
}
