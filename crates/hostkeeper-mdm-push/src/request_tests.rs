use rustls::pki_types::CertificateSigningRequestDer;
use rustls::pki_types::pem::PemObject as _;
use x509_parser::prelude::{FromDer as _, X509CertificationRequest};

use rsa::RsaPublicKey;

use super::*;

const EMAIL: &str = "ops@example.org";
use crate::testkit::fixture_key;

#[test]
fn a_request_has_a_pkcs1_push_key_a_csr_over_it_and_an_encryption_certificate() {
    let request = new_request("ops@example.org", 1_800_000_000).unwrap();
    assert!(
        request
            .key_pem
            .expose()
            .starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
    );
    assert!(
        request
            .encryption_key_pem
            .expose()
            .starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
    );
    assert_ne!(request.key_pem, request.encryption_key_pem);
    assert!(
        request
            .csr_pem
            .starts_with("-----BEGIN CERTIFICATE REQUEST-----")
    );
    assert!(
        request
            .encryption_cert_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert_eq!(format!("{request:?}"), "PushRequest { .. }");
}

#[test]
fn the_csr_is_rsa_2048_signed_by_the_push_key_with_the_email_in_its_subject() {
    let key = fixture_key(0);
    let pem = csr_pem(key, "ops@example.org").unwrap();
    let der = CertificateSigningRequestDer::from_pem_slice(pem.as_bytes()).unwrap();
    let (_, csr) = X509CertificationRequest::from_der(der.as_ref()).unwrap();
    let signed_by_the_push_key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        csr.certification_request_info
            .subject_pki
            .subject_public_key
            .data
            .as_ref(),
    );
    signed_by_the_push_key
        .verify(
            csr.certification_request_info.raw,
            csr.signature_value.data.as_ref(),
        )
        .unwrap();
    let subject = csr.certification_request_info.subject.to_string();
    assert!(subject.contains("C=US"), "{subject}");
    assert!(subject.contains("CN=mdm-push"), "{subject}");
    assert!(subject.contains("ops@example.org"), "{subject}");
    // The signing key is the one of the request, and it is 2048 bits.
    let spki = csr.certification_request_info.subject_pki.raw.to_vec();
    let expected = {
        use rsa::pkcs8::EncodePublicKey as _;
        RsaPublicKey::from(key)
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec()
    };
    assert_eq!(spki, expected);
    assert_eq!(rsa::traits::PublicKeyParts::size(key) * 8, KEY_BITS);
}

#[test]
fn the_encryption_certificate_is_self_signed_for_a_year_from_now_and_names_the_service() {
    let key = fixture_key(1);
    let pem = encryption_cert_pem(key, 1_800_000_000).unwrap();
    let chain = crate::identity::certificates_of(pem.as_bytes()).unwrap();
    let (_, cert) = x509_parser::parse_x509_certificate(chain[0].as_ref()).unwrap();
    let own_key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        cert.public_key().subject_public_key.data.as_ref(),
    );
    own_key
        .verify(
            cert.tbs_certificate.as_ref(),
            cert.signature_value.data.as_ref(),
        )
        .unwrap();
    assert_eq!(cert.validity().not_before.timestamp(), 1_800_000_000);
    assert_eq!(
        cert.validity().not_after.timestamp(),
        1_800_000_000 + 365 * 86_400
    );
    assert!(cert.subject().to_string().contains("CN=mdmcert.download"));
}

#[test]
fn an_email_that_is_not_one_plain_address_is_refused_before_a_key_is_made() {
    for bad in [
        "",
        "nobody",
        "@example.org",
        "a@",
        "a@b",
        "a b@example.org",
        "a@@example.org",
        "a@example.org,b@example.org",
        "\"a\"@example.org",
        "a@.example.org",
        "a@example.org.",
        "a@exa\nmple.org",
        "<a@example.org>",
    ] {
        assert!(!email_is_valid(bad), "{bad:?}");
        assert_eq!(
            new_request(bad, 0).err(),
            Some(RequestError::Email),
            "{bad:?}"
        );
        assert_eq!(
            csr_pem(fixture_key(0), bad).err(),
            Some(RequestError::Email)
        );
    }
    assert!(email_is_valid("first.last+tag@sub.example.org"));
    assert!(!email_is_valid(&format!("{}@example.org", "a".repeat(250))));
    assert_eq!(
        RequestError::Email.to_string(),
        "the email address is not valid"
    );
}

#[test]
fn a_time_out_of_range_is_a_crypto_error_and_not_a_panic() {
    assert!(matches!(
        encryption_cert_pem(fixture_key(0), i64::MAX),
        Err(RequestError::Crypto(_))
    ));
}

#[test]
fn a_key_too_small_to_sign_with_is_a_crypto_error_and_not_a_panic() {
    // ring signs with RSA keys of 2048 bits or more.
    let small = RsaPrivateKey::new(&mut rand_core::OsRng, 1024).unwrap();
    assert!(matches!(
        csr_pem(&small, EMAIL),
        Err(RequestError::Crypto(_))
    ));
    assert!(matches!(
        encryption_cert_pem(&small, 1_800_000_000),
        Err(RequestError::Crypto(_))
    ));
}
