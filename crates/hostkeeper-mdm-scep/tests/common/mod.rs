//! A SCEP client and an independent checker of replies, for the tests. The client assembles the
//! DER by hand, so that every part of a request can be left out or wrong; the checker decodes with
//! the typed `cms` decoders where the crate under test walks the bytes itself.
#![allow(dead_code, reason = "each test binary uses a part of the helpers")]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use aes::{Aes128, Aes192, Aes256};
use cipher::block_padding::Pkcs7;
use cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use cms::content_info::ContentInfo;
use cms::enveloped_data::{EnvelopedData, RecipientInfo};
use cms::signed_data::SignedData;
use const_oid::ObjectIdentifier;
use der::asn1::OctetString;
use der::{Any, Decode, Encode, Tag, TagNumber};
use hostkeeper_mdm_scep::{Ca, Challenges, Error, KeyHash, Redeem};
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, PrivateKey};
use sha2::{Digest, Sha256};
use signature::{SignatureEncoding, Signer, Verifier};
use spki::DecodePublicKey;
use x509_cert::Certificate;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::time::Validity;

pub const SERIAL: &str = "P24LH9H6JT";
pub const CHALLENGE: &str = "6f1d8c0a-challenge-of-one-hour";

pub fn oid(text: &str) -> ObjectIdentifier {
    ObjectIdentifier::new_unwrap(text)
}

pub fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

pub fn ca() -> &'static Ca {
    static CA: OnceLock<Ca> = OnceLock::new();
    CA.get_or_init(|| {
        Ca::generate(
            &mut rand::rng(),
            2048,
            "CN=hostkeeper MDM CA,O=Komira",
            now(),
            3650,
            Default::default(),
        )
        .expect("a CA")
    })
}

// DER by hand.

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
pub fn context(constructed: bool, content: &[u8]) -> Vec<u8> {
    tlv(
        Tag::ContextSpecific {
            constructed,
            number: TagNumber(0),
        },
        content,
    )
}
pub fn int(n: u8) -> Vec<u8> {
    n.to_der().expect("DER")
}
pub fn oid_der(text: &str) -> Vec<u8> {
    oid(text).to_der().expect("DER")
}
pub fn printable(text: &str) -> Vec<u8> {
    tlv(Tag::PrintableString, text.as_bytes())
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

pub const DATA: &str = "1.2.840.113549.1.7.1";
pub const SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
pub const ENVELOPED_DATA: &str = "1.2.840.113549.1.7.3";
pub const RSA: &str = "1.2.840.113549.1.1.1";
pub const SHA256_RSA: &str = "1.2.840.113549.1.1.11";
pub const SHA1_RSA: &str = "1.2.840.113549.1.1.5";
pub const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub const SHA1: &str = "1.3.14.3.2.26";
pub const AES128: &str = "2.16.840.1.101.3.4.1.2";
pub const AES192: &str = "2.16.840.1.101.3.4.1.22";
pub const AES256: &str = "2.16.840.1.101.3.4.1.42";
pub const DES3: &str = "1.2.840.113549.3.7";
pub const MESSAGE_TYPE: &str = "2.16.840.1.113733.1.9.2";
pub const PKI_STATUS: &str = "2.16.840.1.113733.1.9.3";
pub const FAIL_INFO: &str = "2.16.840.1.113733.1.9.4";
pub const SENDER_NONCE: &str = "2.16.840.1.113733.1.9.5";
pub const RECIPIENT_NONCE: &str = "2.16.840.1.113733.1.9.6";
pub const TRANSACTION_ID: &str = "2.16.840.1.113733.1.9.7";
pub const CHALLENGE_PASSWORD: &str = "1.2.840.113549.1.9.7";
pub const CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
pub const MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";

struct Leaf(Name);
impl BuilderProfile for Leaf {
    fn get_issuer(&self, subject: &Name) -> Name {
        subject.clone()
    }
    fn get_subject(&self) -> Name {
        self.0.clone()
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

/// A Mac's SCEP client: an RSA key and the temporary self-signed certificate it signs with.
pub struct Client {
    pub key: PrivateKey,
    pub cert: Certificate,
}

impl Client {
    pub fn new(bits: usize) -> Self {
        let key = PrivateKey::new(&mut rand::rng(), bits).expect("a key");
        let spki = spki::SubjectPublicKeyInfoOwned::from_der(
            key.to_public_key()
                .to_public_key_der()
                .expect("DER")
                .as_bytes(),
        )
        .expect("an spki");
        let name = Name::from_str("CN=temporary").expect("a name");
        let validity = Validity::new(
            x509_cert::time::Time::try_from(now() - Duration::from_secs(60)).expect("time"),
            x509_cert::time::Time::try_from(now() + Duration::from_secs(600)).expect("time"),
        );
        let cert = CertificateBuilder::new(
            Leaf(name),
            SerialNumber::generate(&mut rand::rng()),
            validity,
            spki,
        )
        .expect("a builder")
        .build::<_, Signature>(&SigningKey::<Sha256>::new(key.clone()))
        .expect("a certificate");
        Self { key, cert }
    }

    pub fn spki_der(&self) -> Vec<u8> {
        self.key
            .to_public_key()
            .to_public_key_der()
            .expect("DER")
            .as_bytes()
            .to_vec()
    }
}

/// What a request says; `Default` is a valid PKCSReq with an AES-128 envelope.
#[derive(Clone)]
pub struct Spec {
    pub challenge: Option<String>,
    pub transaction_id: Option<String>,
    pub nonce: Option<Vec<u8>>,
    pub message_type: String,
    pub cipher: &'static str,
    pub sha1: bool,
    pub csr_sha1: bool,
    pub csr_algorithm: &'static str,
    pub csr_spki: Option<Vec<u8>>,
    pub break_csr_signature: bool,
    pub plaintext: Option<Vec<u8>>,
    pub envelope_to: Option<rsa::PublicKey>,
    pub key_transport: &'static str,
    pub signer_in_message: bool,
    pub content_type_attribute: bool,
    pub digest_attribute: bool,
    pub wrong_digest: bool,
    pub signature_algorithm: &'static str,
    pub digest_algorithm: &'static str,
    pub reorder_attributes: bool,
    pub outer_type: &'static str,
    pub iv_mode: u8,
    pub no_recipient: bool,
    pub wrap_to: Option<rsa::PublicKey>,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            challenge: Some(CHALLENGE.to_owned()),
            transaction_id: Some("ABCDEF0123456789".to_owned()),
            nonce: Some(vec![7; 16]),
            message_type: "19".to_owned(),
            cipher: AES128,
            sha1: false,
            csr_sha1: false,
            csr_algorithm: "",
            csr_spki: None,
            break_csr_signature: false,
            plaintext: None,
            envelope_to: None,
            key_transport: RSA,
            signer_in_message: true,
            content_type_attribute: true,
            digest_attribute: true,
            wrong_digest: false,
            signature_algorithm: RSA,
            digest_algorithm: "",
            reorder_attributes: false,
            outer_type: SIGNED_DATA,
            iv_mode: 0,
            no_recipient: false,
            wrap_to: None,
        }
    }
}

fn digest_of(sha1: bool, data: &[u8]) -> Vec<u8> {
    if sha1 {
        sha1::Sha1::digest(data).to_vec()
    } else {
        Sha256::digest(data).to_vec()
    }
}

fn sign(client: &Client, sha1: bool, message: &[u8]) -> Vec<u8> {
    if sha1 {
        SigningKey::<sha1::Sha1>::new(client.key.clone())
            .sign(message)
            .to_vec()
    } else {
        SigningKey::<Sha256>::new(client.key.clone())
            .sign(message)
            .to_vec()
    }
}

/// The certificate request, DER.
pub fn csr(client: &Client, spec: &Spec) -> Vec<u8> {
    let subject = Name::from_str("CN=client-chosen-subject")
        .expect("a name")
        .to_der()
        .expect("DER");
    let spki = spec.csr_spki.clone().unwrap_or_else(|| client.spki_der());
    let attributes = match &spec.challenge {
        Some(challenge) => attr(CHALLENGE_PASSWORD, printable(challenge)),
        None => Vec::new(),
    };
    let info = seq(&[int(0), subject, spki, context(true, &attributes)]);
    let algorithm = if !spec.csr_algorithm.is_empty() {
        spec.csr_algorithm
    } else if spec.csr_sha1 {
        SHA1_RSA
    } else {
        SHA256_RSA
    };
    let mut signature = sign(client, spec.csr_sha1, &info);
    if spec.break_csr_signature {
        signature[0] ^= 1;
    }
    let mut bits = vec![0u8];
    bits.extend(signature);
    seq(&[info, alg(algorithm), tlv(Tag::BitString, &bits)])
}

/// The `ContentInfo` of an `EnvelopedData` of `plain` to `to`, by hand.
pub fn envelope(plain: &[u8], to: &rsa::PublicKey, ca_cert: &Certificate, spec: &Spec) -> Vec<u8> {
    let mut rng = rand::rng();
    let (key_len, iv_len) = match spec.cipher {
        AES192 => (24, 16),
        AES256 => (32, 16),
        DES3 => (24, 8),
        _ => (16, 16),
    };
    let cek: Vec<u8> = (0..key_len).map(|i| i as u8 ^ 0x5a).collect();
    let iv: Vec<u8> = (0..iv_len).map(|i| 0xa0 + i as u8).collect();
    let ciphertext = match spec.cipher {
        AES192 => cbc::Encryptor::<Aes192>::new_from_slices(&cek, &iv)
            .expect("key")
            .encrypt_padded_vec::<Pkcs7>(plain),
        AES256 => cbc::Encryptor::<Aes256>::new_from_slices(&cek, &iv)
            .expect("key")
            .encrypt_padded_vec::<Pkcs7>(plain),
        DES3 => cbc::Encryptor::<des::TdesEde3>::new_from_slices(&cek, &iv)
            .expect("key")
            .encrypt_padded_vec::<Pkcs7>(plain),
        _ => cbc::Encryptor::<Aes128>::new_from_slices(&cek, &iv)
            .expect("key")
            .encrypt_padded_vec::<Pkcs7>(plain),
    };
    let wrap_to = spec.wrap_to.as_ref().unwrap_or(to);
    let wrapped = wrap_to
        .encrypt(&mut rng, Pkcs1v15Encrypt, &cek)
        .expect("rsa");
    let tbs = ca_cert.tbs_certificate();
    let rid = seq(&[
        tbs.issuer().to_der().expect("DER"),
        tbs.serial_number().to_der().expect("DER"),
    ]);
    let recipient = seq(&[int(0), rid, alg(spec.key_transport), octets(&wrapped)]);
    let parameters = match spec.iv_mode {
        1 => seq(&[oid_der(spec.cipher)]),
        2 => seq(&[oid_der(spec.cipher), int(1)]),
        3 => seq(&[oid_der(spec.cipher), octets(&iv[..3])]),
        _ => seq(&[oid_der(spec.cipher), octets(&iv)]),
    };
    let content = seq(&[oid_der(DATA), parameters, context(false, &ciphertext)]);
    let recipients = if spec.no_recipient {
        set(&[])
    } else {
        set(&[recipient])
    };
    let enveloped = seq(&[int(0), recipients, content]);
    seq(&[oid_der(ENVELOPED_DATA), context(true, &enveloped)])
}

/// A whole PKCSReq, DER, from `client` to the CA.
pub fn request(client: &Client, spec: &Spec) -> Vec<u8> {
    let ca_cert = Certificate::from_der(ca().cert_der()).expect("the CA certificate");
    let to = match &spec.envelope_to {
        Some(key) => key.clone(),
        None => rsa::PublicKey::from_public_key_der(
            &ca_cert
                .tbs_certificate()
                .subject_public_key_info()
                .to_der()
                .expect("DER"),
        )
        .expect("the CA key"),
    };
    let plain = spec.plaintext.clone().unwrap_or_else(|| csr(client, spec));
    let econtent = envelope(&plain, &to, &ca_cert, spec);
    signed(client, spec, &econtent)
}

pub fn signed(client: &Client, spec: &Spec, econtent: &[u8]) -> Vec<u8> {
    let digest_oid = if !spec.digest_algorithm.is_empty() {
        spec.digest_algorithm
    } else if spec.sha1 {
        SHA1
    } else {
        SHA256
    };
    let mut digest = digest_of(spec.sha1, econtent);
    if spec.wrong_digest {
        digest[0] ^= 1;
    }
    let mut attrs = Vec::new();
    if spec.content_type_attribute {
        attrs.push(attr(CONTENT_TYPE, oid_der(DATA)));
    }
    if spec.digest_attribute {
        attrs.push(attr(MESSAGE_DIGEST, octets(&digest)));
    }
    attrs.push(attr(MESSAGE_TYPE, printable(&spec.message_type)));
    if let Some(tid) = &spec.transaction_id {
        attrs.push(attr(TRANSACTION_ID, printable(tid)));
    }
    if let Some(nonce) = &spec.nonce {
        attrs.push(attr(SENDER_NONCE, octets(nonce)));
    }
    if spec.reorder_attributes {
        // A sender that does not sort its SET OF: the largest encoding first. DER sorts them
        // the other way, so these bytes are valid BER and a re-encoding would differ.
        attrs.sort_by(|a, b| b.cmp(a));
    } else {
        attrs.sort();
    }
    let attrs = attrs.concat();
    let signature = sign(client, spec.sha1, &tlv(Tag::Set, &attrs));
    let tbs = client.cert.tbs_certificate();
    let sid = seq(&[
        tbs.issuer().to_der().expect("DER"),
        tbs.serial_number().to_der().expect("DER"),
    ]);
    let signer = seq(&[
        int(1),
        sid,
        alg(digest_oid),
        context(true, &attrs),
        alg(spec.signature_algorithm),
        octets(&signature),
    ]);
    let certificates = if spec.signer_in_message {
        context(true, &client.cert.to_der().expect("DER"))
    } else {
        Vec::new()
    };
    let data = seq(&[
        int(1),
        set(&[alg(digest_oid)]),
        seq(&[oid_der(DATA), context(true, &octets(econtent))]),
        certificates,
        set(&[signer]),
    ]);
    seq(&[oid_der(spec.outer_type), context(true, &data)])
}

/// The default signed request of a fresh client.
pub fn standard() -> (Client, Vec<u8>) {
    let client = Client::new(2048);
    let message = request(&client, &Spec::default());
    (client, message)
}

// The checker.

/// A decoded `CertRep`, after its signature was checked against `ca()`.
pub struct Rep {
    pub status: String,
    pub fail_info: Option<String>,
    pub message_type: String,
    pub transaction_id: Vec<u8>,
    pub recipient_nonce: Vec<u8>,
    pub sender_nonce: Vec<u8>,
    pub content: Option<Vec<u8>>,
    pub attribute_oids: Vec<ObjectIdentifier>,
}

fn text(any: &Any) -> String {
    String::from_utf8(any.value().to_vec()).expect("text")
}

pub fn check_rep(der: &[u8]) -> Rep {
    let info = ContentInfo::from_der(der).expect("a ContentInfo");
    assert_eq!(info.content_type, oid(SIGNED_DATA));
    let signed = SignedData::from_der(&info.content.to_der().expect("DER")).expect("SignedData");
    let signer = signed.signer_infos.0.iter().next().expect("one signer");
    let attrs = signer.signed_attrs.as_ref().expect("signed attributes");
    let get = |text_oid: &str| {
        attrs
            .iter()
            .find(|a| a.oid == oid(text_oid))
            .and_then(|a| a.values.iter().next().cloned())
    };
    // RFC 5652 section 5.3: both are required whenever there are signed attributes.
    let content_type = get(CONTENT_TYPE).expect("a contentType attribute");
    assert_eq!(
        content_type
            .decode_as::<ObjectIdentifier>()
            .expect("an OID"),
        oid(DATA)
    );
    let message_digest = get(MESSAGE_DIGEST).expect("a messageDigest attribute");
    let econtent = signed
        .encap_content_info
        .econtent
        .as_ref()
        .map(|c| c.value().to_vec());
    let hashed = Sha256::digest(econtent.as_deref().unwrap_or(&[]));
    assert_eq!(
        message_digest.value(),
        hashed.as_slice(),
        "the messageDigest is the hash of the content"
    );
    let mut to_verify = attrs.to_der().expect("DER");
    to_verify[0] = 0x31;
    let ca_cert = Certificate::from_der(ca().cert_der()).expect("the CA certificate");
    let key = rsa::PublicKey::from_public_key_der(
        &ca_cert
            .tbs_certificate()
            .subject_public_key_info()
            .to_der()
            .expect("DER"),
    )
    .expect("the CA key");
    VerifyingKey::<Sha256>::new(key)
        .verify(
            &to_verify,
            &Signature::try_from(signer.signature.as_bytes()).expect("a signature"),
        )
        .expect("the reply is signed by the CA");
    Rep {
        status: text(&get(PKI_STATUS).expect("pkiStatus")),
        fail_info: get(FAIL_INFO).map(|a| text(&a)),
        message_type: text(&get(MESSAGE_TYPE).expect("messageType")),
        transaction_id: get(TRANSACTION_ID).expect("transactionID").value().to_vec(),
        recipient_nonce: get(RECIPIENT_NONCE)
            .expect("recipientNonce")
            .value()
            .to_vec(),
        sender_nonce: get(SENDER_NONCE).expect("senderNonce").value().to_vec(),
        content: econtent,
        attribute_oids: attrs.iter().map(|a| a.oid).collect(),
    }
}

/// What `client` can read of a successful reply: the certificates inside, issued one first, and the
/// content cipher of the envelope.
pub fn open_rep(rep: &Rep, client: &Client) -> (Vec<Certificate>, ObjectIdentifier) {
    let content = rep.content.as_ref().expect("a success has content");
    let info = ContentInfo::from_der(content).expect("a ContentInfo");
    assert_eq!(info.content_type, oid(ENVELOPED_DATA));
    let env = EnvelopedData::from_der(&info.content.to_der().expect("DER")).expect("EnvelopedData");
    let Some(RecipientInfo::Ktri(ktri)) = env.recip_infos.0.iter().next() else {
        panic!("ktri")
    };
    let tbs = client.cert.tbs_certificate();
    match &ktri.rid {
        cms::enveloped_data::RecipientIdentifier::IssuerAndSerialNumber(is) => {
            assert_eq!(is.issuer, *tbs.issuer());
            assert_eq!(is.serial_number, *tbs.serial_number());
        }
        other => panic!("{other:?}"),
    }
    let cek = client
        .key
        .decrypt(Pkcs1v15Encrypt, ktri.enc_key.as_bytes())
        .expect("the key");
    let alg = &env.encrypted_content.content_enc_alg;
    let iv: OctetString = alg
        .parameters
        .clone()
        .expect("an iv")
        .decode_as()
        .expect("an iv");
    let data = env
        .encrypted_content
        .encrypted_content
        .as_ref()
        .expect("content");
    let plain = match alg.oid.to_string().as_str() {
        AES128 => cbc::Decryptor::<Aes128>::new_from_slices(&cek, iv.as_bytes())
            .expect("key")
            .decrypt_padded_vec::<Pkcs7>(data.as_bytes()),
        AES192 => cbc::Decryptor::<Aes192>::new_from_slices(&cek, iv.as_bytes())
            .expect("key")
            .decrypt_padded_vec::<Pkcs7>(data.as_bytes()),
        AES256 => cbc::Decryptor::<Aes256>::new_from_slices(&cek, iv.as_bytes())
            .expect("key")
            .decrypt_padded_vec::<Pkcs7>(data.as_bytes()),
        other => panic!("a reply cipher {other}"),
    }
    .expect("padding");
    let inner = ContentInfo::from_der(&plain).expect("certs-only ContentInfo");
    let certs_only =
        SignedData::from_der(&inner.content.to_der().expect("DER")).expect("SignedData");
    assert!(
        certs_only.signer_infos.0.is_empty(),
        "a degenerate SignedData has no signer"
    );
    let certs = certs_only
        .certificates
        .expect("certificates")
        .0
        .iter()
        .map(|c| match c {
            cms::cert::CertificateChoices::Certificate(c) => c.clone(),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<Certificate>>();
    // A SET OF has no insertion order: the identity is the one the CA did not name itself.
    let (ca_certs, mut identities): (Vec<_>, Vec<_>) = certs
        .into_iter()
        .partition(|c| c.to_der().expect("DER") == ca().cert_der());
    assert_eq!(ca_certs.len(), 1, "the CA certificate rides along once");
    assert_eq!(identities.len(), 1, "and one identity");
    identities.extend(ca_certs);
    (identities, alg.oid)
}

/// A map of challenges: the serial each was issued for, and what consumed it.
#[derive(Default)]
pub struct Store {
    pub open: HashMap<String, String>,
    pub consumed: HashMap<String, (KeyHash, Vec<u8>)>,
    pub redeemed: usize,
}

impl Store {
    pub fn with(challenge: &str, serial: &str) -> Self {
        let mut store = Self::default();
        store.open.insert(challenge.to_owned(), serial.to_owned());
        store
    }
}

impl Challenges for Store {
    fn redeem(
        &mut self,
        challenge: &str,
        key_hash: KeyHash,
        issue: &mut dyn FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Redeem {
        self.redeemed += 1;
        if let Some((hash, der)) = self.consumed.get(challenge) {
            return if *hash == key_hash {
                Redeem::Replayed(der.clone())
            } else {
                Redeem::Consumed
            };
        }
        let Some(serial) = self.open.get(challenge) else {
            return Redeem::Unknown;
        };
        match issue(serial) {
            Ok(der) => {
                self.consumed
                    .insert(challenge.to_owned(), (key_hash, der.clone()));
                Redeem::Issued(der)
            }
            Err(e) => Redeem::Failed(e),
        }
    }
}

pub fn pki(message: &[u8], store: &mut Store) -> Result<hostkeeper_mdm_scep::Reply, Error> {
    ca().pki_operation(message, store, now(), &mut rand::rng())
}
