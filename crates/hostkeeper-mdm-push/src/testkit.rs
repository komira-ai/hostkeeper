//! Fakes and fixtures for the tests of this crate and of the coordinator's (feature `testkit`):
//! a clock that is set, an APNs sender and an mdmcert.download transport that answer as told, RSA
//! keys made once per test binary, certificates of chosen topics and validity, and the reply of
//! mdmcert.download built for a key (hex text of a PKCS#7 enveloped message), in DER and in the
//! BER of a streaming encoder.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use aes::{Aes128, Aes192, Aes256};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cbc::Encryptor;
use cbc::cipher::block_padding::Pkcs7;

use des::{Des, TdesEde3};
use rand_core::OsRng;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};
use rsa::pkcs8::EncodePrivateKey as _;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use rustls::pki_types::PrivatePkcs8KeyDer;

use crate::BoxFuture;
use crate::apns::{Delivered, Push, PushError, PushSender};
use crate::clock::Clock;
use crate::mdmcert::{HttpReply, MdmcertTransport, TransportError};

/// A clock that says what it was set to.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock at `now_unix`.
    #[must_use]
    pub fn at(now_unix: u64) -> Self {
        Self(AtomicU64::new(now_unix))
    }

    /// Moves the clock to `now_unix`.
    pub fn set(&self, now_unix: u64) {
        self.0.store(now_unix, Ordering::SeqCst);
    }

    /// Moves the clock on by `seconds`.
    pub fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// An APNs sender that records every push and answers from a script (`Ok` once the script is
/// empty).
#[derive(Debug, Default)]
pub struct FakeApns {
    sent: Mutex<Vec<Push>>,
    script: Mutex<VecDeque<Result<Delivered, PushError>>>,
}

impl FakeApns {
    /// A fake that answers `Ok` to every push.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the answer of the next push not yet answered from the script.
    pub fn answer_next(&self, answer: Result<Delivered, PushError>) {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(answer);
    }

    /// The pushes sent so far, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<Push> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl PushSender for FakeApns {
    fn send<'a>(&'a self, push: &'a Push) -> BoxFuture<'a, Result<Delivered, PushError>> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(push.clone());
        let answer = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Ok(Delivered {
                    apns_id: Some("fake-apns-id".to_owned()),
                })
            });
        Box::pin(async move { answer })
    }
}

/// An mdmcert.download transport that records the bodies posted and answers as told: `success` by
/// default.
#[derive(Debug, Default)]
pub struct FakeMdmcert {
    posted: Mutex<Vec<Vec<u8>>>,
    script: Mutex<VecDeque<Result<HttpReply, TransportError>>>,
}

impl FakeMdmcert {
    /// A fake that answers `200 {"result":"success"}`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the answer of the next request.
    pub fn answer_next(&self, answer: Result<HttpReply, TransportError>) {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(answer);
    }

    /// The bodies posted so far, in order.
    #[must_use]
    pub fn posted(&self) -> Vec<Vec<u8>> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl MdmcertTransport for FakeMdmcert {
    fn post_sign_request(
        &self,
        body: Vec<u8>,
        _timeout: std::time::Duration,
    ) -> BoxFuture<'_, Result<HttpReply, TransportError>> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(body);
        let answer = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Ok(HttpReply {
                    status: 200,
                    body: br#"{"result":"success"}"#.to_vec(),
                })
            });
        Box::pin(async move { answer })
    }
}

/// How many distinct keys [`fixture_key`] holds.
const FIXTURE_KEYS: usize = 4;

/// The `index`th RSA 2048 key of this test binary, made on first use and kept: making one is the
/// slow part of a test, so a test names the key it needs and two tests share it.
///
/// # Panics
/// `index` is not below 4, or no key could be made.
#[must_use]
pub fn fixture_key(index: usize) -> &'static RsaPrivateKey {
    static KEYS: [OnceLock<RsaPrivateKey>; FIXTURE_KEYS] =
        [const { OnceLock::new() }; FIXTURE_KEYS];
    KEYS[index].get_or_init(|| {
        crate::request::new_rsa_key().expect("an RSA key is made from the system's randomness")
    })
}

/// A self-signed certificate over `key` with the topic `topic` as its `UID`, valid from
/// `not_before_unix` to `not_after_unix`, PEM.
///
/// # Panics
/// A time is out of range, or the certificate cannot be signed.
#[must_use]
pub fn push_certificate_pem(
    key: &RsaPrivateKey,
    topic: &str,
    not_before_unix: i64,
    not_after_unix: i64,
) -> String {
    let der = key.to_pkcs8_der().expect("the key encodes");
    let pair = KeyPair::from_pkcs8_der_and_sign_algo(
        &PrivatePkcs8KeyDer::from(der.as_bytes().to_vec()),
        &PKCS_RSA_SHA256,
    )
    .expect("ring takes the key");
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "APSP:test");
    name.push(
        DnType::CustomDnType(vec![0, 9, 2342, 19_200_300, 100, 1, 1]),
        topic,
    );
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    params.not_before = time::OffsetDateTime::from_unix_timestamp(not_before_unix).expect("time");
    params.not_after = time::OffsetDateTime::from_unix_timestamp(not_after_unix).expect("time");
    params.self_signed(&pair).expect("signs").pem()
}

/// The content ciphers [`seal_reply`] can encrypt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    /// AES-128-CBC.
    Aes128,
    /// AES-192-CBC.
    Aes192,
    /// AES-256-CBC.
    Aes256,
    /// DES-EDE3-CBC.
    TripleDes,
    /// DES-CBC.
    Des,
    /// An identifier no reader takes (AES-256-GCM's).
    Gcm,
}

/// How [`seal_reply`] encodes the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// DER, definite lengths.
    Der,
    /// BER as a streaming encoder writes it: indefinite lengths, content in chunks.
    Streaming,
}

/// The DER value with `tag` and `content`.
#[must_use]
pub fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match content.len() {
        n if n < 0x80 => out.push(u8::try_from(n).expect("short")),
        n if n < 0x100 => out.extend([0x81, u8::try_from(n).expect("one byte")]),
        n => out.extend([0x82, u8::try_from(n >> 8).expect("two"), (n & 0xff) as u8]),
    }
    out.extend_from_slice(content);
    out
}

/// The BER value with `tag`, an indefinite length and `content`.
#[must_use]
pub fn indefinite(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag, 0x80];
    out.extend_from_slice(content);
    out.extend([0, 0]);
    out
}

#[must_use]
/// The DER of the object identifier `dotted`.
pub fn oid_der(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted
        .split('.')
        .map(|arc| arc.parse().expect("digits"))
        .collect();
    let mut body = vec![u8::try_from(arcs[0] * 40 + arcs[1]).expect("first byte")];
    for &arc in &arcs[2..] {
        let mut chunks = vec![u8::try_from(arc & 0x7f).expect("seven bits")];
        let mut rest = arc >> 7;
        while rest > 0 {
            chunks.push(u8::try_from(rest & 0x7f).expect("seven bits") | 0x80);
            rest >>= 7;
        }
        chunks.reverse();
        body.extend(chunks);
    }
    der(0x06, &body)
}

fn cbc_seal<E: cbc::cipher::BlockEncryptMut + cbc::cipher::KeyIvInit>(
    key: &[u8],
    iv: &[u8],
    plain: &[u8],
) -> Vec<u8> {
    E::new_from_slices(key, iv)
        .expect("key and iv suit the cipher")
        .encrypt_padded_vec_mut::<Pkcs7>(plain)
}

/// The reply of mdmcert.download for `plain`, encrypted to the public key of `recipient` as a
/// PKCS#7 enveloped-data message with the content cipher `cipher`, encoded as `encoding`, and as
/// the email attachment has it: hex text.
///
/// # Panics
/// The key cannot encrypt.
#[must_use]
pub fn seal_reply(
    plain: &[u8],
    recipient: &RsaPrivateKey,
    cipher: Cipher,
    encoding: Encoding,
) -> Vec<u8> {
    hex::encode(seal_reply_raw(plain, recipient, cipher, encoding)).into_bytes()
}

/// [`seal_reply`] without the hex text: the bytes of the message.
///
/// # Panics
/// The key cannot encrypt.
#[must_use]
pub fn seal_reply_raw(
    plain: &[u8],
    recipient: &RsaPrivateKey,
    cipher: Cipher,
    encoding: Encoding,
) -> Vec<u8> {
    let (cipher_oid, key_len, iv_len) = match cipher {
        Cipher::Aes128 => ("2.16.840.1.101.3.4.1.2", 16, 16),
        Cipher::Aes192 => ("2.16.840.1.101.3.4.1.22", 24, 16),
        Cipher::Aes256 => ("2.16.840.1.101.3.4.1.42", 32, 16),
        Cipher::TripleDes => ("1.2.840.113549.3.7", 24, 8),
        Cipher::Des => ("1.3.14.3.2.7", 8, 8),
        Cipher::Gcm => ("2.16.840.1.101.3.4.1.46", 32, 16),
    };
    let content_key: Vec<u8> = (0..key_len)
        .map(|i| u8::try_from(i * 7 + 3).expect("byte"))
        .collect();
    let iv: Vec<u8> = (0..iv_len)
        .map(|i| u8::try_from(i * 5 + 1).expect("byte"))
        .collect();
    let ciphertext = match cipher {
        Cipher::Aes128 => cbc_seal::<Encryptor<Aes128>>(&content_key, &iv, plain),
        Cipher::Aes192 => cbc_seal::<Encryptor<Aes192>>(&content_key, &iv, plain),
        Cipher::Aes256 | Cipher::Gcm => cbc_seal::<Encryptor<Aes256>>(&content_key, &iv, plain),
        Cipher::TripleDes => cbc_seal::<Encryptor<TdesEde3>>(&content_key, &iv, plain),
        Cipher::Des => cbc_seal::<Encryptor<Des>>(&content_key, &iv, plain),
    };
    let wrapped_key = RsaPublicKey::from(recipient)
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, &content_key)
        .expect("the key encrypts");
    let issuer_and_serial = der(0x30, &[der(0x30, &[]), der(0x02, &[1])].concat());
    let recipient_info = der(
        0x30,
        &[
            der(0x02, &[0]),
            issuer_and_serial,
            der(
                0x30,
                &[oid_der("1.2.840.113549.1.1.1"), der(0x05, &[])].concat(),
            ),
            der(0x04, &wrapped_key),
        ]
        .concat(),
    );
    let chunked = |bytes: &[u8]| -> Vec<u8> {
        let (first, second) = bytes.split_at(bytes.len() / 2);
        indefinite(0xA0, &[der(0x04, first), der(0x04, second)].concat())
    };
    let content = match encoding {
        Encoding::Der => der(0x80, &ciphertext),
        Encoding::Streaming => chunked(&ciphertext),
    };
    let encrypted_info = der(
        0x30,
        &[
            oid_der("1.2.840.113549.1.7.1"),
            der(0x30, &[oid_der(cipher_oid), der(0x04, &iv)].concat()),
            content,
        ]
        .concat(),
    );
    let enveloped = der(
        0x30,
        &[der(0x02, &[0]), der(0x31, &recipient_info), encrypted_info].concat(),
    );
    let body = [
        oid_der("1.2.840.113549.1.7.3"),
        match encoding {
            Encoding::Der => der(0xA0, &enveloped),
            Encoding::Streaming => indefinite(0xA0, &enveloped),
        },
    ]
    .concat();
    match encoding {
        Encoding::Der => der(0x30, &body),
        Encoding::Streaming => indefinite(0x30, &body),
    }
}

/// The property list Apple's push portal takes, as the signed request for the CSR `csr_pem`:
/// the CSR as base64 of its PEM, a chain and a signature.
///
/// # Panics
/// The list cannot be written.
#[must_use]
pub fn signed_request_plist(csr_pem: &str) -> Vec<u8> {
    let mut dict = plist::Dictionary::new();
    dict.insert(
        "PushCertRequestCSR".to_owned(),
        plist::Value::String(STANDARD.encode(csr_pem)),
    );
    dict.insert(
        "PushCertCertificateChain".to_owned(),
        plist::Value::String(
            "-----BEGIN CERTIFICATE-----\nchain\n-----END CERTIFICATE-----\n".to_owned(),
        ),
    );
    dict.insert(
        "PushCertSignature".to_owned(),
        plist::Value::String("c2lnbmF0dXJl".to_owned()),
    );
    let mut out = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut out)
        .expect("the list is written");
    out
}
