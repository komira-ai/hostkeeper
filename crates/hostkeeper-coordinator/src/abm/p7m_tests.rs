//! The `.p7m` of ABM's token ([`crate::abm::p7m`]), over envelopes made here: with the `cms`
//! crate's own builder for the usual one (RSA PKCS#1 v1.5 key transport, AES-CBC), and by hand for
//! the rest (OAEP, 3DES, a message with no content, an odd recipient).
//!
//! A round trip with this code's own envelope proves the reader against the format as the `cms`
//! crate writes it. That the same file opens with `openssl cms -decrypt` was checked once by hand
//! with a synthetic token and a throwaway key, outside the tests (the PR text says so); no `.p7m`
//! that Apple made was available.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
use cms::builder::ContentEncryptionAlgorithm;
use cms::content_info::{CmsVersion, ContentInfo};
use cms::der::asn1::{ObjectIdentifier, OctetString};
use cms::der::{Any, Encode as _};
use cms::enveloped_data::{
    EncryptedContentInfo, EnvelopedData, KeyTransRecipientInfo, OtherRecipientInfo, RecipientInfo,
    RecipientInfos,
};
use cms::spki::AlgorithmIdentifierOwned;
use rsa::{Oaep, PrivateKey};

use crate::abm::fake::{built, der_of, rid, rng};
use crate::abm::keys::{KEY_BITS, generate};
use crate::abm::p7m::{P7mError, token_json};

const TOKEN: &str = r#"{"consumer_key":"CK","consumer_secret":"CS","access_token":"AT","access_secret":"AS","access_secret_expiry":"2027-10-31T22:01:36Z"}"#;

/// The text ABM puts around the token.
fn framed() -> Vec<u8> {
    format!(
        "Content-Type: text/plain;charset=UTF8\r\n\r\n-----BEGIN MDM SERVER TOKEN-----\r\n{TOKEN}\r\n-----END MDM SERVER TOKEN-----\r\n"
    )
    .into_bytes()
}

/// How a by-hand envelope transports the key.
#[derive(Clone, Copy)]
enum Transport {
    OaepSha1,
    OaepSha256,
    Unknown,
}

/// An envelope made by hand: a random key of `cek_len` bytes, transported as `transport`, and
/// the content encrypted by the cipher `cipher` (an OID) with `encrypt`.
fn by_hand(
    plain: &[u8],
    to: &PrivateKey,
    transport: Transport,
    cipher: &str,
    cek_len: usize,
    encrypt: impl Fn(&[u8], &[u8], &[u8]) -> Vec<u8>,
) -> EnvelopedData {
    let cek: Vec<u8> = (0..cek_len)
        .map(|i| u8::try_from(i * 7 + 3).unwrap_or(1))
        .collect();
    let iv = vec![9_u8; if cipher == DES3 { 8 } else { 16 }];
    let public = to.to_public_key();
    let (oid, encrypted) = match transport {
        Transport::OaepSha1 => (
            "1.2.840.113549.1.1.7",
            public
                .encrypt(&mut rng(), Oaep::<sha1::Sha1>::new(), &cek)
                .expect("OAEP"),
        ),
        Transport::OaepSha256 => (
            "1.2.840.113549.1.1.7",
            public
                .encrypt(&mut rng(), Oaep::<sha2::Sha256>::new(), &cek)
                .expect("OAEP"),
        ),
        Transport::Unknown => ("1.3.132.1.12", vec![1, 2, 3]),
    };
    let ktri = RecipientInfo::Ktri(KeyTransRecipientInfo {
        version: CmsVersion::V2,
        rid: rid(),
        key_enc_alg: AlgorithmIdentifierOwned {
            oid: ObjectIdentifier::new_unwrap(oid),
            parameters: None,
        },
        enc_key: OctetString::new(encrypted).expect("an octet string"),
    });
    let content = encrypt(&cek, &iv, plain);
    envelope(vec![ktri], cipher, Some(&iv), Some(content))
}

/// An envelope of `recipients` whose content is `content` by `cipher` with `iv`.
fn envelope(
    recipients: Vec<RecipientInfo>,
    cipher: &str,
    iv: Option<&[u8]>,
    content: Option<Vec<u8>>,
) -> EnvelopedData {
    EnvelopedData {
        version: CmsVersion::V2,
        originator_info: None,
        recip_infos: RecipientInfos::try_from(recipients).expect("recipients"),
        encrypted_content: EncryptedContentInfo {
            content_type: ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1"),
            content_enc_alg: AlgorithmIdentifierOwned {
                oid: ObjectIdentifier::new_unwrap(cipher),
                parameters: iv.map(|iv| {
                    Any::encode_from(&OctetString::new(iv.to_vec()).expect("an octet string"))
                        .expect("encodes")
                }),
            },
            encrypted_content: content.map(|c| OctetString::new(c).expect("an octet string")),
        },
        unprotected_attrs: None,
    }
}

fn aes256(cek: &[u8], iv: &[u8], plain: &[u8]) -> Vec<u8> {
    cbc::Encryptor::<aes::Aes256>::new_from_slices(cek, iv)
        .expect("a key")
        .encrypt_padded_vec::<Pkcs7>(plain)
}

fn tdes(cek: &[u8], iv: &[u8], plain: &[u8]) -> Vec<u8> {
    cbc::Encryptor::<des::TdesEde3>::new_from_slices(cek, iv)
        .expect("a key")
        .encrypt_padded_vec::<Pkcs7>(plain)
}

const AES_256: &str = "2.16.840.1.101.3.4.1.42";
const AES_128: &str = "2.16.840.1.101.3.4.1.2";
const AES_192: &str = "2.16.840.1.101.3.4.1.22";
const DES3: &str = "1.2.840.113549.3.7";

/// The text of an S/MIME `.p7m`: headers, a blank line, the base64 body in 76-column lines.
fn smime(der: &[u8]) -> Vec<u8> {
    let body = STANDARD.encode(der);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).expect("ASCII"))
        .collect();
    format!(
        "MIME-Version: 1.0\r\nContent-Disposition: attachment; filename=\"smime.p7m\"\r\nContent-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        lines.join("\r\n")
    )
    .into_bytes()
}

/// Proves: the token is taken out of an envelope in each form the file may come in (DER, S/MIME
/// text with its headers and base64 lines, PEM armour, and base64 alone), for a token that Apple
/// frames as text and for a bare object, whichever of the three AES-CBC key sizes the content
/// uses.
///
/// Catches: a `.p7m` that opens only as DER (ABM's is a MIME file), a framing taken as part of the
/// token, and a cipher size that is read as another.
#[test]
fn the_token_comes_out_of_every_form_of_the_file() {
    let key = generate(KEY_BITS).expect("a key");
    let der = built(&framed(), &key, ContentEncryptionAlgorithm::Aes256Cbc);
    let want = TOKEN.as_bytes().to_vec();
    assert_eq!(token_json(&der, &key), Ok(want.clone()));
    assert_eq!(token_json(&smime(&der), &key), Ok(want.clone()));
    let armour = format!(
        "-----BEGIN PKCS7-----\n{}\n-----END PKCS7-----\n",
        STANDARD.encode(&der)
    );
    assert_eq!(token_json(armour.as_bytes(), &key), Ok(want.clone()));
    assert_eq!(
        token_json(STANDARD.encode(&der).as_bytes(), &key),
        Ok(want.clone())
    );
    // A text with a colon in its first line and no blank line is a body, not headers.
    for algorithm in [
        ContentEncryptionAlgorithm::Aes128Cbc,
        ContentEncryptionAlgorithm::Aes192Cbc,
    ] {
        let der = built(TOKEN.as_bytes(), &key, algorithm);
        assert_eq!(token_json(&der, &key), Ok(want.clone()));
    }
    // A blank line inside the base64 with no header before it is no header.
    let encoded = STANDARD.encode(&der);
    let (first, second) = encoded.split_at(40);
    let split = format!("{first}\n\n{second}");
    assert_eq!(token_json(split.as_bytes(), &key), Ok(want.clone()));
    // LF line ends, as a Unix tool writes the file.
    let lf = String::from_utf8(smime(&der))
        .expect("text")
        .replace("\r\n", "\n");
    assert_eq!(token_json(lf.as_bytes(), &key), Ok(want));
}

/// Proves: the key transports and ciphers beyond the builder's are read: OAEP with SHA-1 and with
/// SHA-256, 3DES-CBC and AES-128 and AES-192 with a key of their size.
///
/// Catches: a token ABM encrypts with OAEP or 3DES that the first real upload fails to open.
#[test]
fn oaep_and_3des_envelopes_are_read() {
    let key = generate(KEY_BITS).expect("a key");
    let want = TOKEN.as_bytes().to_vec();
    for (transport, cipher, cek_len, encrypt) in [
        (
            Transport::OaepSha1,
            AES_256,
            32,
            aes256 as fn(&[u8], &[u8], &[u8]) -> Vec<u8>,
        ),
        (Transport::OaepSha256, AES_256, 32, aes256),
        (Transport::OaepSha1, DES3, 24, tdes),
    ] {
        let enveloped = by_hand(&framed(), &key, transport, cipher, cek_len, encrypt);
        assert_eq!(
            token_json(&der_of(&enveloped), &key),
            Ok(want.clone()),
            "{cipher}"
        );
    }
    let aes = |size: usize, cipher: &'static str| {
        let encrypt = move |cek: &[u8], iv: &[u8], plain: &[u8]| match size {
            16 => cbc::Encryptor::<aes::Aes128>::new_from_slices(cek, iv)
                .expect("a key")
                .encrypt_padded_vec::<Pkcs7>(plain),
            _ => cbc::Encryptor::<aes::Aes192>::new_from_slices(cek, iv)
                .expect("a key")
                .encrypt_padded_vec::<Pkcs7>(plain),
        };
        by_hand(&framed(), &key, Transport::OaepSha1, cipher, size, encrypt)
    };
    assert_eq!(
        token_json(&der_of(&aes(16, AES_128)), &key),
        Ok(want.clone())
    );
    assert_eq!(token_json(&der_of(&aes(24, AES_192)), &key), Ok(want));
}

/// Proves: a recipient that is not a key transport is passed over and the next one tried, one
/// whose key algorithm is unknown is passed over too, and a message encrypted to another key is
/// [`P7mError::WrongKey`] (checked with OAEP, which refuses explicitly; PKCS#1 v1.5's implicit
/// rejection gives an error as well, but which one is the padding's chance).
///
/// Catches: a message with several recipients that fails at the first, and a message for another
/// key that opens to garbage and is taken for a token.
#[test]
fn only_a_recipient_of_our_key_opens_the_message() {
    let key = generate(KEY_BITS).expect("a key");
    let other = generate(KEY_BITS).expect("another key");
    let want = TOKEN.as_bytes().to_vec();

    let good = by_hand(&framed(), &key, Transport::OaepSha1, AES_256, 32, aes256);
    let odd = RecipientInfo::Ori(OtherRecipientInfo {
        ori_type: ObjectIdentifier::new_unwrap("1.2.3.4"),
        ori_value: Any::encode_from(&OctetString::new(vec![1]).expect("octets")).expect("encodes"),
    });
    let unknown = by_hand(&framed(), &key, Transport::Unknown, AES_256, 32, aes256);
    let mut recipients = vec![odd];
    recipients.extend(unknown.recip_infos.0.iter().cloned());
    recipients.extend(good.recip_infos.0.iter().cloned());
    let several = envelope(
        recipients,
        AES_256,
        Some(&[9; 16]),
        good.encrypted_content
            .encrypted_content
            .map(|c| c.as_bytes().to_vec()),
    );
    assert_eq!(token_json(&der_of(&several), &key), Ok(want));

    let for_other = by_hand(&framed(), &other, Transport::OaepSha1, AES_256, 32, aes256);
    assert_eq!(
        token_json(&der_of(&for_other), &key),
        Err(P7mError::WrongKey)
    );
    let only_odd_and_unknown = by_hand(&framed(), &key, Transport::Unknown, AES_256, 32, aes256);
    assert_eq!(
        token_json(&der_of(&only_odd_and_unknown), &key),
        Err(P7mError::WrongKey)
    );
    let only_odd = envelope(
        vec![RecipientInfo::Ori(OtherRecipientInfo {
            ori_type: ObjectIdentifier::new_unwrap("1.2.3.4"),
            ori_value: Any::encode_from(&OctetString::new(vec![1]).expect("octets"))
                .expect("encodes"),
        })],
        AES_256,
        Some(&[9; 16]),
        Some(vec![0; 16]),
    );
    assert_eq!(
        token_json(&der_of(&only_odd), &key),
        Err(P7mError::WrongKey)
    );
    let pkcs1 = built(&framed(), &other, ContentEncryptionAlgorithm::Aes256Cbc);
    assert!(token_json(&pkcs1, &key).is_err());
}

/// Proves: what is not a CMS enveloped-data message is [`P7mError::NotCms`] (bytes that are
/// neither DER nor text, text that is not base64, DER of another content type, an envelope with
/// no content or no IV), a cipher this release does not read is [`P7mError::Cipher`], and a
/// message that opens and holds no object is [`P7mError::NoToken`].
///
/// Catches: a file of the wrong kind that is reported as a wrong key, sending the operator to
/// ABM for a new token when the file was never a token.
#[test]
fn a_file_that_is_not_the_message_is_refused_for_what_it_is() {
    let key = generate(KEY_BITS).expect("a key");
    assert_eq!(token_json(&[0xff, 0xfe, 0x00], &key), Err(P7mError::NotCms));
    assert_eq!(
        token_json(b"this is not base64 !!", &key),
        Err(P7mError::NotCms)
    );
    assert_eq!(token_json(b"", &key), Err(P7mError::NotCms));
    assert_eq!(
        token_json(&[0x30, 0x03, 0x02, 0x01, 0x00], &key),
        Err(P7mError::NotCms)
    );
    let data = ContentInfo {
        content_type: ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1"),
        content: Any::encode_from(&OctetString::new(vec![1]).expect("octets")).expect("encodes"),
    }
    .to_der()
    .expect("DER");
    assert_eq!(token_json(&data, &key), Err(P7mError::NotCms));
    let not_enveloped = ContentInfo {
        content_type: ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.3"),
        content: Any::encode_from(&OctetString::new(vec![1]).expect("octets")).expect("encodes"),
    }
    .to_der()
    .expect("DER");
    assert_eq!(token_json(&not_enveloped, &key), Err(P7mError::NotCms));

    let good = by_hand(&framed(), &key, Transport::OaepSha1, AES_256, 32, aes256);
    let recipients: Vec<RecipientInfo> = good.recip_infos.0.iter().cloned().collect();
    let content = good
        .encrypted_content
        .encrypted_content
        .map(|c| c.as_bytes().to_vec());
    let no_content = envelope(recipients.clone(), AES_256, Some(&[9; 16]), None);
    assert_eq!(
        token_json(&der_of(&no_content), &key),
        Err(P7mError::NotCms)
    );
    let no_iv = envelope(recipients.clone(), AES_256, None, content.clone());
    assert_eq!(token_json(&der_of(&no_iv), &key), Err(P7mError::NotCms));
    let gcm = envelope(
        recipients,
        "2.16.840.1.101.3.4.1.46",
        Some(&[9; 12]),
        content,
    );
    assert_eq!(token_json(&der_of(&gcm), &key), Err(P7mError::Cipher));

    for plain in [&b"no object in here"[..], b"} backwards {"] {
        let none = by_hand(plain, &key, Transport::OaepSha1, AES_256, 32, aes256);
        assert_eq!(token_json(&der_of(&none), &key), Err(P7mError::NoToken));
    }
}

/// Proves: a content key that does not fit the cipher (a key of 5 bytes for AES-256) and an IV
/// that does not fit are a recipient that did not open, not a panic.
///
/// Catches: a message built wrongly, or a key the padding check mangled, that crashes the
/// coordinator's upload call.
#[test]
fn a_key_or_iv_that_does_not_fit_the_cipher_is_not_a_panic() {
    let key = generate(KEY_BITS).expect("a key");
    let short_key = by_hand(
        &framed(),
        &key,
        Transport::OaepSha1,
        AES_256,
        5,
        |_, _, _| vec![0; 16],
    );
    assert_eq!(
        token_json(&der_of(&short_key), &key),
        Err(P7mError::WrongKey)
    );
    let mut short_iv = by_hand(&framed(), &key, Transport::OaepSha1, AES_256, 32, aes256);
    short_iv.encrypted_content.content_enc_alg.parameters =
        Some(Any::encode_from(&OctetString::new(vec![1, 2, 3]).expect("octets")).expect("encodes"));
    assert_eq!(
        token_json(&der_of(&short_iv), &key),
        Err(P7mError::WrongKey)
    );
}
