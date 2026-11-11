//! A fake Apple: a root, an intermediate and a device identity made here, and a builder of the
//! blob a Mac would post, by hand, so that every part can be left out or wrong.
#![allow(dead_code, reason = "each test binary uses a part of the helpers")]

use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use der::asn1::ObjectIdentifier;
use der::{Any, Decode, Encode, Tag, TagNumber};
use p256::ecdsa::{DerSignature, SigningKey as EcKey};
use rsa::PrivateKey;
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};
use signature::{SignatureEncoding, Signer};
use spki::SubjectPublicKeyInfoOwned;
use x509_cert::Certificate;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::{Time, Validity};

pub const SERIAL: &str = "P24LH9H6JT";
pub const UDID: &str = "00008112-001A2B3C4D5E6F70";
pub const DATA: &str = "1.2.840.113549.1.7.1";
pub const SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
pub const RSA: &str = "1.2.840.113549.1.1.1";
pub const SHA1_RSA: &str = "1.2.840.113549.1.1.5";
pub const SHA256_RSA: &str = "1.2.840.113549.1.1.11";
pub const SHA384_RSA: &str = "1.2.840.113549.1.1.12";
pub const SHA512_RSA: &str = "1.2.840.113549.1.1.13";
pub const EC_KEY: &str = "1.2.840.10045.2.1";
pub const ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
pub const ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
pub const SHA1: &str = "1.3.14.3.2.26";
pub const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub const SHA384: &str = "2.16.840.1.101.3.4.2.2";
pub const SHA512: &str = "2.16.840.1.101.3.4.2.3";
pub const SHA224: &str = "2.16.840.1.101.3.4.2.4";
pub const CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
pub const MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";

pub fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

pub fn tlv(tag: Tag, content: &[u8]) -> Vec<u8> {
    Any::new(tag, content)
        .expect("a TLV")
        .to_der()
        .expect("DER")
}
pub fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(Tag::Sequence, &parts.concat())
}
pub fn set(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(Tag::Set, &parts.concat())
}
pub fn context(number: u8, constructed: bool, content: &[u8]) -> Vec<u8> {
    tlv(
        Tag::ContextSpecific {
            constructed,
            number: TagNumber(u32::from(number)),
        },
        content,
    )
}
pub fn int(n: u8) -> Vec<u8> {
    n.to_der().expect("DER")
}
pub fn oid_der(text: &str) -> Vec<u8> {
    ObjectIdentifier::new_unwrap(text).to_der().expect("DER")
}
pub fn octets(bytes: &[u8]) -> Vec<u8> {
    tlv(Tag::OctetString, bytes)
}
pub fn alg(text: &str) -> Vec<u8> {
    seq(&[oid_der(text), vec![5, 0]])
}
pub fn attr(text: &str, value: Vec<u8>) -> Vec<u8> {
    seq(&[oid_der(text), set(&[value])])
}

/// The plist a Mac posts.
pub fn plist(serial: &str, udid: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>SERIAL</key><string>{serial}</string><key>UDID</key><string>{udid}</string>\
         <key>PRODUCT</key><string>Mac14,13</string><key>VERSION</key><string>26A434</string>\
         <key>LANGUAGE</key><string>en</string></dict></plist>"
    )
    .into_bytes()
}

/// A key a certificate can be signed with.
#[derive(Clone)]
pub enum Key {
    Rsa(PrivateKey),
    Ec(EcKey),
}

impl Key {
    pub fn spki(&self) -> SubjectPublicKeyInfoOwned {
        let der = match self {
            Self::Rsa(key) => key.to_public_key().to_public_key_der().expect("DER"),
            Self::Ec(key) => {
                use p256::pkcs8::EncodePublicKey as _;
                key.verifying_key().to_public_key_der().expect("DER")
            }
        };
        SubjectPublicKeyInfoOwned::from_der(der.as_bytes()).expect("an spki")
    }
}

fn rsa_key(index: usize) -> &'static PrivateKey {
    static KEYS: [OnceLock<PrivateKey>; 4] = [const { OnceLock::new() }; 4];
    KEYS[index].get_or_init(|| PrivateKey::new(&mut rand::rng(), 2048).expect("a key"))
}

pub fn rsa(index: usize) -> Key {
    Key::Rsa(rsa_key(index).clone())
}

pub fn ec() -> Key {
    Key::Ec(EcKey::from_slice(&[7_u8; 32]).expect("a key"))
}

struct Profile {
    issuer: Name,
    subject: Name,
}
impl BuilderProfile for Profile {
    fn get_issuer(&self, _: &Name) -> Name {
        self.issuer.clone()
    }
    fn get_subject(&self) -> Name {
        self.subject.clone()
    }
    fn build_extensions(
        &self,
        _: spki::SubjectPublicKeyInfoRef<'_>,
        _: spki::SubjectPublicKeyInfoRef<'_>,
        _: &x509_cert::certificate::TbsCertificate,
    ) -> x509_cert::builder::Result<Vec<x509_cert::ext::Extension>> {
        Ok(Vec::new())
    }
}

/// A validity from `from` to `until`, in seconds around [`now`].
pub fn validity(from: i64, until: i64) -> Validity {
    let at = |s: i64| {
        let base = now();
        let time = if s >= 0 {
            base + Duration::from_secs(s.unsigned_abs())
        } else {
            base - Duration::from_secs(s.unsigned_abs())
        };
        Time::try_from(time).expect("time")
    };
    Validity::new(at(from), at(until))
}

/// A certificate for `subject_key` named `subject`, issued by `issuer_name` and signed by `signer`.
pub fn cert(
    subject: &str,
    issuer_name: &str,
    subject_key: &Key,
    signer: &Key,
    v: Validity,
) -> Certificate {
    let profile = Profile {
        issuer: Name::from_str(issuer_name).expect("a name"),
        subject: Name::from_str(subject).expect("a name"),
    };
    let builder = CertificateBuilder::new(
        profile,
        SerialNumber::generate(&mut rand::rng()),
        v,
        subject_key.spki(),
    )
    .expect("a builder");
    match signer {
        Key::Rsa(key) => builder
            .build::<_, Signature>(&SigningKey::<Sha256>::new(key.clone()))
            .expect("a certificate"),
        Key::Ec(key) => builder
            .build::<_, DerSignature>(key)
            .expect("a certificate"),
    }
}

pub fn valid() -> Validity {
    validity(-86_400, 86_400 * 365)
}

/// A fake Apple: root, intermediate, a device identity under the intermediate.
pub struct Pki {
    pub root_key: Key,
    pub root: Certificate,
    pub inter_key: Key,
    pub inter: Certificate,
    pub device_key: Key,
    pub device: Certificate,
}

pub fn pki() -> &'static Pki {
    static PKI: OnceLock<Pki> = OnceLock::new();
    PKI.get_or_init(|| {
        let (root_key, inter_key, device_key) = (rsa(0), rsa(1), rsa(2));
        let root = cert(
            "CN=Fake Apple Root",
            "CN=Fake Apple Root",
            &root_key,
            &root_key,
            valid(),
        );
        let inter = cert(
            "CN=Fake Device CA",
            "CN=Fake Apple Root",
            &inter_key,
            &root_key,
            valid(),
        );
        let device = cert(
            "CN=Fake Device",
            "CN=Fake Device CA",
            &device_key,
            &inter_key,
            valid(),
        );
        Pki {
            root_key,
            root,
            inter_key,
            inter,
            device_key,
            device,
        }
    })
}

pub fn der(cert: &Certificate) -> Vec<u8> {
    cert.to_der().expect("DER")
}

pub fn roots() -> Vec<Vec<u8>> {
    vec![der(&pki().root)]
}

/// What the blob says; `Default` is a valid blob signed by the fake device with SHA-256.
#[derive(Clone)]
pub struct Spec {
    pub content: Vec<u8>,
    pub certs: Vec<Certificate>,
    pub signer: Key,
    pub signer_cert: Certificate,
    pub signed_attributes: bool,
    pub content_type: Option<&'static str>,
    pub wrong_digest: bool,
    pub digest_alg: &'static str,
    pub signature_alg: &'static str,
    pub break_signature: bool,
    pub signature_der: Option<Vec<u8>>,
    pub sid_serial_off: bool,
    pub detached: bool,
    pub outer_type: &'static str,
    pub signer_count: usize,
    /// An unsigned attribute `[1]` after the signature.
    pub unsigned_attributes: bool,
    /// A signed attribute of a kind the verifier does not read.
    pub extra_attribute: bool,
}

impl Default for Spec {
    fn default() -> Self {
        let pki = pki();
        Self {
            content: plist(SERIAL, UDID),
            certs: vec![pki.device.clone(), pki.inter.clone()],
            signer: pki.device_key.clone(),
            signer_cert: pki.device.clone(),
            signed_attributes: true,
            content_type: Some(DATA),
            wrong_digest: false,
            digest_alg: SHA256,
            signature_alg: SHA256_RSA,
            break_signature: false,
            signature_der: None,
            sid_serial_off: false,
            detached: false,
            outer_type: SIGNED_DATA,
            signer_count: 1,
            unsigned_attributes: false,
            extra_attribute: false,
        }
    }
}

fn hash(oid: &str, data: &[u8]) -> Vec<u8> {
    match oid {
        SHA1 => sha1::Sha1::digest(data).to_vec(),
        SHA384 => sha2::Sha384::digest(data).to_vec(),
        SHA512 => sha2::Sha512::digest(data).to_vec(),
        _ => Sha256::digest(data).to_vec(),
    }
}

fn sign(spec: &Spec, message: &[u8]) -> Vec<u8> {
    match &spec.signer {
        Key::Ec(key) => {
            let signature: DerSignature = key.sign(message);
            signature.to_vec()
        }
        Key::Rsa(key) => {
            // `rsaEncryption` names no hash: the signer info's digest algorithm does.
            let hash = match spec.signature_alg {
                RSA => spec.digest_alg,
                SHA1_RSA => SHA1,
                SHA384_RSA => SHA384,
                SHA512_RSA => SHA512,
                _ => SHA256,
            };
            match hash {
                SHA1 => SigningKey::<sha1::Sha1>::new(key.clone())
                    .sign(message)
                    .to_vec(),
                SHA384 => SigningKey::<sha2::Sha384>::new(key.clone())
                    .sign(message)
                    .to_vec(),
                SHA512 => SigningKey::<sha2::Sha512>::new(key.clone())
                    .sign(message)
                    .to_vec(),
                _ => SigningKey::<Sha256>::new(key.clone())
                    .sign(message)
                    .to_vec(),
            }
        }
    }
}

/// The DER of the blob.
pub fn blob(spec: &Spec) -> Vec<u8> {
    let tbs = spec.signer_cert.tbs_certificate();
    let sid = seq(&[
        tbs.issuer().to_der().expect("DER"),
        if spec.sid_serial_off {
            SerialNumber::<x509_cert::certificate::Rfc5280>::new(&[0x7f, 0x7f])
                .expect("a serial")
                .to_der()
                .expect("DER")
        } else {
            tbs.serial_number().to_der().expect("DER")
        },
    ]);
    let mut attributes = Vec::new();
    if let Some(kind) = spec.content_type {
        attributes.push(attr(CONTENT_TYPE, oid_der(kind)));
    }
    let mut wanted = hash(spec.digest_alg, &spec.content);
    if spec.wrong_digest {
        wanted[0] ^= 1;
    }
    attributes.push(attr(MESSAGE_DIGEST, octets(&wanted)));
    if spec.extra_attribute {
        attributes.push(attr("1.2.840.113549.1.9.5", octets(b"a signing time")));
    }
    let attributes = attributes.concat();
    let signed = if spec.signed_attributes {
        set_bytes(&attributes)
    } else {
        spec.content.clone()
    };
    let mut signature = spec
        .signature_der
        .clone()
        .unwrap_or_else(|| sign(spec, &signed));
    if spec.break_signature {
        let last = signature.len() - 1;
        signature[last] ^= 1;
    }
    let mut fields = vec![int(1), sid, alg(spec.digest_alg)];
    if spec.signed_attributes {
        fields.push(context(0, true, &attributes));
    }
    fields.push(alg(spec.signature_alg));
    fields.push(octets(&signature));
    if spec.unsigned_attributes {
        fields.push(context(1, true, &attr("1.2.3.4.5", octets(b"x"))));
    }
    let signer = seq(&fields);
    let signers: Vec<Vec<u8>> = (0..spec.signer_count).map(|_| signer.clone()).collect();
    let content = if spec.detached {
        seq(&[oid_der(DATA)])
    } else {
        seq(&[oid_der(DATA), context(0, true, &octets(&spec.content))])
    };
    let certs: Vec<u8> = spec.certs.iter().flat_map(der).collect();
    let signed_data = seq(&[
        int(1),
        set(&[alg(spec.digest_alg)]),
        content,
        context(0, true, &certs),
        set(&signers),
    ]);
    seq(&[oid_der(spec.outer_type), context(0, true, &signed_data)])
}

fn set_bytes(content: &[u8]) -> Vec<u8> {
    tlv(Tag::Set, content)
}
