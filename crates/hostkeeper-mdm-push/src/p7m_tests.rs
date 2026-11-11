use super::*;
use crate::request::pkcs1_pem;
use crate::testkit::{Cipher, Encoding, der, fixture_key, oid_der, seal_reply, seal_reply_raw};

const PLAIN: &[u8] =
    b"<?xml version=\"1.0\"?><plist><dict><key>PushCertRequestCSR</key></dict></plist>";

fn key_pem(index: usize) -> Vec<u8> {
    pkcs1_pem(fixture_key(index)).unwrap().expose().to_vec()
}

#[test]
fn every_content_cipher_in_either_encoding_decrypts_to_the_signed_request() {
    for cipher in [
        Cipher::Aes128,
        Cipher::Aes192,
        Cipher::Aes256,
        Cipher::TripleDes,
        Cipher::Des,
    ] {
        for encoding in [Encoding::Der, Encoding::Streaming] {
            let reply = seal_reply(PLAIN, fixture_key(1), cipher, encoding);
            assert_eq!(
                decrypt(&reply, &key_pem(1)).unwrap(),
                PLAIN,
                "{cipher:?} {encoding:?}"
            );
        }
    }
}

#[test]
fn the_attachment_as_hex_text_with_line_breaks_and_the_raw_bytes_both_decrypt() {
    let raw = seal_reply_raw(PLAIN, fixture_key(1), Cipher::Aes256, Encoding::Der);
    let hex_text = hex::encode(&raw);
    let wrapped: String = hex_text
        .as_bytes()
        .chunks(64)
        .map(|line| format!("{}\r\n", std::str::from_utf8(line).unwrap()))
        .collect();
    assert_eq!(decrypt(wrapped.as_bytes(), &key_pem(1)).unwrap(), PLAIN);
    assert_eq!(
        decrypt(hex_text.to_uppercase().as_bytes(), &key_pem(1)).unwrap(),
        PLAIN
    );
    assert_eq!(decrypt(&raw, &key_pem(1)).unwrap(), PLAIN);
    assert_eq!(decode_reply(wrapped.as_bytes()), raw);
    assert_eq!(decode_reply(&raw), raw);
    assert_eq!(decode_reply(b""), b"");
    // Hex of an odd length is not hex: it is taken as the bytes it is.
    assert_eq!(decode_reply(b"abc"), b"abc");
}

#[test]
fn a_reply_encrypted_to_another_key_is_refused_as_not_for_this_request() {
    let reply = seal_reply(PLAIN, fixture_key(2), Cipher::Aes256, Encoding::Der);
    assert_eq!(decrypt(&reply, &key_pem(1)), Err(ReplyError::NotForThisKey));
}

#[test]
fn bytes_that_are_not_an_enveloped_message_are_refused_without_echoing_them() {
    for upload in [
        &b"hello, this is not CMS"[..],
        b"",
        b"3000",
        &der(0x04, &[1]),
    ] {
        let error = decrypt(upload, &key_pem(1)).unwrap_err();
        assert!(matches!(error, ReplyError::NotEnveloped(_)), "{error:?}");
    }
    let signed_data = der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.2"), der(0xA0, &[])].concat(),
    );
    assert_eq!(
        decrypt(&signed_data, &key_pem(1)).unwrap_err().to_string(),
        "the reply is not a PKCS#7 enveloped message: the content type is not enveloped data"
    );
    let no_content = der(0x30, &oid_der("1.2.840.113549.1.7.3"));
    assert!(matches!(
        decrypt(&no_content, &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    let not_wrapped = der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.3"), der(0x04, &[])].concat(),
    );
    assert!(matches!(
        decrypt(&not_wrapped, &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    // Enveloped data with no recipients, or no encrypted content.
    let envelope = |inner: &[u8]| {
        der(
            0x30,
            &[
                oid_der("1.2.840.113549.1.7.3"),
                der(0xA0, &der(0x30, inner)),
            ]
            .concat(),
        )
    };
    assert!(matches!(
        decrypt(&envelope(&der(0x02, &[0])), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    assert!(matches!(
        decrypt(
            &envelope(&[der(0x02, &[0]), der(0x31, &[])].concat()),
            &key_pem(1)
        ),
        Err(ReplyError::NotEnveloped(_))
    ));
    assert!(matches!(
        decrypt(&der(0x31, &[]), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
}

#[test]
fn a_recipient_of_a_kind_this_reader_skips_leaves_no_recipient_for_the_key() {
    let short_recipient = der(0x30, &der(0x02, &[0]));
    let tagged_recipient = der(0xA1, &[]);
    let encrypted = der(
        0x30,
        &[
            oid_der("1.2.840.113549.1.7.1"),
            der(0x30, &[]),
            der(0xA0, &[]),
        ]
        .concat(),
    );
    let message = der(
        0x30,
        &[
            oid_der("1.2.840.113549.1.7.3"),
            der(
                0xA0,
                &der(
                    0x30,
                    &[
                        der(0x02, &[0]),
                        der(0x31, &[tagged_recipient, short_recipient].concat()),
                        encrypted,
                    ]
                    .concat(),
                ),
            ),
        ]
        .concat(),
    );
    assert_eq!(
        decrypt(&message, &key_pem(1)),
        Err(ReplyError::NotForThisKey)
    );
}

fn patched(raw: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let at = raw
        .windows(from.len())
        .position(|window| window == from)
        .expect("present");
    let mut out = raw.to_vec();
    out[at..at + to.len()].copy_from_slice(to);
    out
}

#[test]
fn a_key_transport_or_content_cipher_this_reader_does_not_take_is_named_in_the_refusal() {
    let raw = seal_reply_raw(PLAIN, fixture_key(1), Cipher::Aes256, Encoding::Der);
    // rsaEncryption -> RSAES-OAEP.
    let oaep = patched(
        &raw,
        &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01],
        &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x07],
    );
    assert_eq!(
        decrypt(&oaep, &key_pem(1)),
        Err(ReplyError::Unsupported("1.2.840.113549.1.1.7".to_owned()))
    );
    let gcm = seal_reply_raw(PLAIN, fixture_key(1), Cipher::Gcm, Encoding::Der);
    assert_eq!(
        decrypt(&gcm, &key_pem(1)),
        Err(ReplyError::Unsupported(
            "2.16.840.1.101.3.4.1.46".to_owned()
        ))
    );
    assert_eq!(
        ReplyError::Unsupported("1.2.3".to_owned()).to_string(),
        "the reply uses an algorithm that is not read: 1.2.3"
    );
}

#[test]
fn a_content_that_does_not_decrypt_under_its_key_is_refused() {
    let raw = seal_reply_raw(PLAIN, fixture_key(1), Cipher::Aes256, Encoding::Der);
    // AES-256's identifier made AES-128's: the 32-byte content key does not suit it.
    let wrong_size = patched(
        &raw,
        &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a],
        &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x02],
    );
    assert_eq!(decrypt(&wrong_size, &key_pem(1)), Err(ReplyError::Content));
    assert_eq!(
        ReplyError::Content.to_string(),
        "the reply's content does not decrypt"
    );
}

#[test]
fn a_content_with_no_algorithm_or_no_encrypted_part_is_refused() {
    let wrapped_key = {
        use rsa_legacy::RsaPublicKey;
        RsaPublicKey::from(fixture_key(1))
            .encrypt(&mut rand_core_legacy::OsRng, Pkcs1v15Encrypt, &[1_u8; 16])
            .unwrap()
    };
    let recipient = der(
        0x30,
        &[
            der(0x02, &[0]),
            der(0x30, &[]),
            der(
                0x30,
                &[oid_der("1.2.840.113549.1.1.1"), der(0x05, &[])].concat(),
            ),
            der(0x04, &wrapped_key),
        ]
        .concat(),
    );
    let message = |encrypted: &[u8]| {
        der(
            0x30,
            &[
                oid_der("1.2.840.113549.1.7.3"),
                der(
                    0xA0,
                    &der(
                        0x30,
                        &[der(0x02, &[0]), der(0x31, &recipient), encrypted.to_vec()].concat(),
                    ),
                ),
            ]
            .concat(),
        )
    };
    let no_algorithm = der(0x30, &oid_der("1.2.840.113549.1.7.1"));
    assert!(matches!(
        decrypt(&message(&no_algorithm), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    let no_content = der(
        0x30,
        &[
            oid_der("1.2.840.113549.1.7.1"),
            der(
                0x30,
                &[oid_der("2.16.840.1.101.3.4.1.42"), der(0x04, &[0; 16])].concat(),
            ),
        ]
        .concat(),
    );
    assert!(matches!(
        decrypt(&message(&no_content), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    let no_iv = der(
        0x30,
        &[
            oid_der("1.2.840.113549.1.7.1"),
            der(0x30, &oid_der("2.16.840.1.101.3.4.1.42")),
            der(0x80, &[0; 16]),
        ]
        .concat(),
    );
    assert!(matches!(
        decrypt(&message(&no_iv), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
    let not_a_sequence = der(0x04, &[]);
    assert!(matches!(
        decrypt(&message(&not_a_sequence), &key_pem(1)),
        Err(ReplyError::NotEnveloped(_))
    ));
}

#[test]
fn a_key_that_is_not_readable_is_refused_before_the_reply_is_looked_at() {
    let reply = seal_reply(PLAIN, fixture_key(1), Cipher::Aes256, Encoding::Der);
    assert!(matches!(
        decrypt(&reply, b"not a key"),
        Err(ReplyError::Key(_))
    ));
    let ec = rcgen::KeyPair::generate().unwrap().serialize_pem();
    assert!(matches!(
        decrypt(&reply, ec.as_bytes()),
        Err(ReplyError::Key(_))
    ));
}
