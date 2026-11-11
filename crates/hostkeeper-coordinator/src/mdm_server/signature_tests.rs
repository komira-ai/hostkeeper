//! The `Mdm-Signature` check (`docs/design/coordinator-mdm.md`, section 4.1): messages built the way
//! a client builds them (CMS SignedData, detached, RSA), accepted when they verify and refused,
//! each for its own reason, when any part is wrong.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_mdm_push::testkit::{der, oid_der};

use super::{MAX_HEADER_BYTES, SignatureError, signer};
use crate::mdm_server::support::{
    BODY, Ca, Identity2, RSA, SHA1, SHA256, SHA256_RSA, SHA384, SHA512, Spec, header, message,
};

fn mac() -> Identity2 {
    Ca::new().issue(1, "serial")
}

/// Proves: a signature made as a client makes it (signed attributes with the body's digest, one
/// signer named by issuer and serial) verifies, for each digest the reader takes and each way the
/// signature algorithm is named, and the answer is the signer's certificate.
///
/// Catches: a client with `SignMessage` locked out, or a signature accepted for another body.
#[test]
fn a_signature_made_by_the_key_of_a_certificate_names_that_certificate() {
    let mac = mac();
    for digest in [SHA1, SHA256, SHA384, SHA512] {
        for signature in [
            RSA,
            SHA256_RSA,
            "1.2.840.113549.1.1.5",
            "1.2.840.113549.1.1.12",
            "1.2.840.113549.1.1.13",
        ] {
            // A signature algorithm that names its own digest must be the digest signed with.
            let own = match signature {
                SHA256_RSA => SHA256,
                "1.2.840.113549.1.1.5" => SHA1,
                "1.2.840.113549.1.1.12" => SHA384,
                "1.2.840.113549.1.1.13" => SHA512,
                _ => digest,
            };
            if own != digest {
                continue;
            }
            let spec = Spec {
                digest,
                signature,
                ..Spec::default()
            };
            assert_eq!(
                signer(BODY, &header(BODY, &mac, &spec)),
                Ok(mac.der.clone()),
                "{digest} {signature}"
            );
        }
    }
}

/// Proves: a signature without signed attributes is over the body itself, and a signer named by its
/// key identifier is found among several certificates.
///
/// Catches: a client that signs the body directly, or names its signer by key, being refused.
#[test]
fn a_signature_over_the_body_and_a_signer_named_by_key_identifier_verify() {
    let ca = Ca::new();
    let ski = b"key-identifier-1".to_vec();
    let mac = ca.issue_with(2, "serial", Some(&ski));
    let spec = Spec {
        attributes: false,
        ski: Some(ski),
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &spec)),
        Ok(mac.der.clone())
    );
    let other = Spec {
        ski: Some(b"another-identifier".to_vec()),
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &other)),
        Err(SignatureError::NoSigner)
    );
    // The certificate has no key identifier at all.
    let bare = ca.issue(3, "bare");
    let named = Spec {
        ski: Some(b"key-identifier-1".to_vec()),
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &bare, &named)),
        Err(SignatureError::NoSigner)
    );
}

/// Proves: a body that is not the one signed is refused, with the signed attributes (the digest
/// differs) and without them (the signature does not verify); so is a changed signature.
///
/// Catches: a replayed signature carried over a different body.
#[test]
fn a_signature_does_not_verify_for_another_body() {
    let mac = mac();
    let with = header(BODY, &mac, &Spec::default());
    assert_eq!(signer(b"another body", &with), Err(SignatureError::Digest));
    let without = header(
        BODY,
        &mac,
        &Spec {
            attributes: false,
            ..Spec::default()
        },
    );
    assert_eq!(
        signer(b"another body", &without),
        Err(SignatureError::Verify)
    );
    let flipped = header(
        BODY,
        &mac,
        &Spec {
            flip_signature: true,
            ..Spec::default()
        },
    );
    assert_eq!(signer(BODY, &flipped), Err(SignatureError::Verify));
}

/// Proves: a message that is not a signature this reader takes is refused as such, for each way it
/// can be wrong: not base64, too long, not DER, not signed data, trailing bytes, more than one
/// signer, too many certificates, a signer id of a shape that is neither of the two, indefinite
/// signed attributes (the signature is over their DER).
///
/// Catches: a malformed header crashing the reader or being half read.
#[test]
fn a_header_that_is_not_a_signature_is_refused_as_encoding() {
    let mac = mac();
    let good = message(BODY, &mac, &Spec::default());
    assert_eq!(signer(BODY, "not base64 !!"), Err(SignatureError::Encoding));
    assert_eq!(
        signer(BODY, &"A".repeat(MAX_HEADER_BYTES + 1)),
        Err(SignatureError::Encoding)
    );
    assert_eq!(
        signer(BODY, &STANDARD.encode(&good[..good.len() - 5])),
        Err(SignatureError::Encoding)
    );
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(
        signer(BODY, &STANDARD.encode(trailing)),
        Err(SignatureError::Encoding)
    );
    let not_signed = der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.1"), der(0xa0, &der(0x30, &[]))].concat(),
    );
    assert_eq!(
        signer(BODY, &STANDARD.encode(not_signed)),
        Err(SignatureError::Encoding)
    );
    let lone = der(0x30, &oid_der("1.2.840.113549.1.7.2"));
    assert_eq!(
        signer(BODY, &STANDARD.encode(lone)),
        Err(SignatureError::Encoding)
    );
    let empty_wrap = der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.2"), der(0xa0, &[])].concat(),
    );
    assert_eq!(
        signer(BODY, &STANDARD.encode(empty_wrap)),
        Err(SignatureError::Encoding)
    );
    let not_a_sequence = der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.2"), der(0xa0, &der(0x04, b"x"))].concat(),
    );
    assert_eq!(
        signer(BODY, &STANDARD.encode(not_a_sequence)),
        Err(SignatureError::Encoding)
    );
    let two = Spec {
        signers: 2,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &two)),
        Err(SignatureError::Encoding)
    );
    let many = Spec {
        certificates: 9,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &many)),
        Err(SignatureError::Encoding)
    );
    let odd = Spec {
        sid_tag: Some(0x81),
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &odd)),
        Err(SignatureError::Encoding)
    );
    let indefinite = Spec {
        indefinite_attributes: true,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &indefinite)),
        Err(SignatureError::Encoding)
    );
    // A signer's info cut short: before its digest algorithm, and before its signature.
    for fields in [2, 5] {
        let cut = Spec {
            info_fields: Some(fields),
            ..Spec::default()
        };
        assert_eq!(
            signer(BODY, &header(BODY, &mac, &cut)),
            Err(SignatureError::Encoding),
            "{fields} fields"
        );
    }
    // A signer named by an issuer and no serial number.
    let short = Spec {
        short_sid: true,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &short)),
        Err(SignatureError::Encoding)
    );
}

/// Proves: a signature that carries no certificate of its signer, or names another issuer, is
/// refused as having no signer; an algorithm this reader does not take is refused as such; and a
/// signed digest that is missing or not the body's is refused as a digest mismatch.
///
/// Catches: a signature checked against a certificate it did not name.
#[test]
fn a_signature_without_its_signer_or_with_an_unknown_algorithm_is_refused() {
    let mac = mac();
    let none = Spec {
        drop_certificates: true,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &none)),
        Err(SignatureError::NoSigner)
    );
    let other = Spec {
        names_other_serial: true,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &other)),
        Err(SignatureError::NoSigner)
    );
    let md5 = Spec {
        digest: "1.2.840.113549.2.5",
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &md5)),
        Err(SignatureError::Algorithm)
    );
    let ec = Spec {
        signature: "1.2.840.10045.4.3.2",
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &ec)),
        Err(SignatureError::Algorithm)
    );
    let mismatch = Spec {
        claimed: Some(vec![0; 32]),
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &mismatch)),
        Err(SignatureError::Digest)
    );
    let missing = Spec {
        leave_out_digest_attribute: true,
        ..Spec::default()
    };
    assert_eq!(
        signer(BODY, &header(BODY, &mac, &missing)),
        Err(SignatureError::Digest)
    );
}

/// Proves: the signature of a signer whose key is not RSA is refused rather than read.
///
/// Catches: a certificate with another key type reaching the RSA verifier.
#[test]
fn a_certificate_without_an_rsa_key_does_not_verify() {
    let ca = Ca::new();
    // The CA's own certificate has an ECDSA key; the signature is made with an RSA key.
    let ecdsa = Identity2 {
        index: 1,
        der: ca.der(),
        key_der: Vec::new(),
        key: None,
    };
    assert_eq!(
        signer(BODY, &header(BODY, &ecdsa, &Spec::default())),
        Err(SignatureError::Verify)
    );
}

/// Proves: a message corrupted in any one byte, in any of three ways, is answered with a verdict and
/// never a panic, and none of them is taken for the signer's certificate unless the byte did not
/// matter to it.
///
/// Catches: a reader that indexes or slices a field it did not check.
#[test]
fn a_message_corrupted_anywhere_is_judged_and_never_crashes_the_reader() {
    let mac = mac();
    let good = message(BODY, &mac, &Spec::default());
    let mut taken = 0;
    for at in 0..good.len() {
        for flip in [0xff_u8, 0x01, 0x80] {
            let mut bad = good.clone();
            bad[at] ^= flip;
            if signer(BODY, &STANDARD.encode(&bad)).is_ok() {
                taken += 1;
            }
        }
    }
    // Only bytes the signature does not cover can change nothing: the certificate's own signature
    // and a few encodings of lengths that stay valid. Nearly all corruption is refused.
    assert!(
        taken < good.len(),
        "{taken} of {} corruptions were taken",
        good.len() * 3
    );
}
