use rsa::pkcs8::EncodePrivateKey as _;

use super::*;
use crate::request::pkcs1_pem;
use crate::testkit::{fixture_key, push_certificate_pem};

const TOPIC: &str = "com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09";
const NOW: i64 = 1_800_000_000;

fn cert(topic: &str, not_before: i64, not_after: i64) -> String {
    push_certificate_pem(fixture_key(0), topic, not_before, not_after)
}

#[test]
fn a_certificate_says_its_topic_and_its_validity() {
    let info = read_certificate(cert(TOPIC, 1_700_000_000, 1_900_000_000).as_bytes()).unwrap();
    assert_eq!(info.topic, TOPIC);
    assert_eq!(info.not_before_unix, 1_700_000_000);
    assert_eq!(info.not_after_unix, 1_900_000_000);
}

#[test]
fn a_pkcs1_key_the_migrated_shape_and_a_pkcs8_key_both_match_their_certificate() {
    let key = fixture_key(0);
    let pem = cert(TOPIC, 1_700_000_000, 1_900_000_000);
    let pkcs1 = pkcs1_pem(key).unwrap();
    assert!(
        pkcs1
            .expose()
            .starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
    );
    let identity = PushIdentity::parse(pem.as_bytes(), pkcs1.expose()).unwrap();
    assert_eq!(identity.info.topic, TOPIC);
    assert_eq!(identity.chain.len(), 1);
    let pkcs8 = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
    assert!(pkcs8.starts_with("-----BEGIN PRIVATE KEY-----"));
    PushIdentity::parse(pem.as_bytes(), pkcs8.as_bytes()).unwrap();
    let (chain, _key) = identity.tls_identity();
    assert_eq!(chain, identity.chain);
}

#[test]
fn a_key_that_is_not_the_certificates_is_a_mismatch() {
    let pem = cert(TOPIC, 1_700_000_000, 1_900_000_000);
    let other = pkcs1_pem(fixture_key(1)).unwrap();
    assert_eq!(
        PushIdentity::parse(pem.as_bytes(), other.expose()).unwrap_err(),
        IdentityError::KeyMismatch
    );
}

#[test]
fn text_that_is_no_certificate_or_no_key_is_refused_with_the_reason() {
    assert_eq!(
        read_certificate(b"hello").unwrap_err(),
        IdentityError::NoCertificate
    );
    assert!(matches!(
        read_certificate(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"),
        Err(IdentityError::Certificate(_))
    ));
    assert!(matches!(
        read_certificate(b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n"),
        Err(IdentityError::Certificate(_))
    ));
    assert!(matches!(read_key(b"hello"), Err(IdentityError::Key(_))));
    // A certificate with no UID in its subject has no topic.
    let plain = rcgen::generate_simple_self_signed(vec!["x".to_owned()]).unwrap();
    assert_eq!(
        read_certificate(plain.cert.pem().as_bytes()).unwrap_err(),
        IdentityError::NoTopic
    );
}

#[test]
fn a_key_that_is_not_rsa_cannot_match_and_is_refused() {
    let ec = rcgen::KeyPair::generate().unwrap();
    let pem = cert(TOPIC, 1_700_000_000, 1_900_000_000);
    assert!(matches!(
        PushIdentity::parse(pem.as_bytes(), ec.serialize_pem().as_bytes()),
        Err(IdentityError::Key(_))
    ));
    // The certificate's key is read as RSA too: an EC certificate is no push certificate.
    let ec_cert = {
        let mut params = rcgen::CertificateParams::default();
        let mut name = rcgen::DistinguishedName::new();
        name.push(
            rcgen::DnType::CustomDnType(vec![0, 9, 2342, 19_200_300, 100, 1, 1]),
            TOPIC,
        );
        params.distinguished_name = name;
        params.self_signed(&ec).unwrap().pem()
    };
    let info = read_certificate(ec_cert.as_bytes()).unwrap();
    let key = read_key(pkcs1_pem(fixture_key(0)).unwrap().expose()).unwrap();
    assert!(matches!(
        info.matches(&key),
        Err(IdentityError::Certificate(_))
    ));
}

#[test]
fn a_certificate_is_judged_for_expiry_the_topic_prefix_and_a_renewal_topic() {
    let good = read_certificate(cert(TOPIC, 1_700_000_000, 1_900_000_000).as_bytes()).unwrap();
    assert_eq!(good.judge(NOW, None), Ok(()));
    assert_eq!(good.judge(NOW, Some(TOPIC)), Ok(()));
    assert_eq!(
        good.judge(1_900_000_000, None),
        Err(IdentityError::Expired {
            not_after: 1_900_000_000
        })
    );
    assert_eq!(
        good.judge(NOW, Some("com.apple.mgmt.External.other")),
        Err(IdentityError::TopicChanged)
    );
    let foreign =
        read_certificate(cert("app.example.org", 1_700_000_000, 1_900_000_000).as_bytes()).unwrap();
    assert_eq!(foreign.judge(NOW, None), Err(IdentityError::NotMdmTopic));
}

#[test]
fn the_error_texts_hold_no_certificate_or_key_bytes() {
    assert_eq!(
        IdentityError::KeyMismatch.to_string(),
        "the key does not match the certificate"
    );
    assert_eq!(
        IdentityError::Expired { not_after: 5 }.to_string(),
        "the certificate expired at unix time 5"
    );
    assert_eq!(
        IdentityError::TopicChanged.to_string(),
        "the topic is not the topic of the certificate in use"
    );
    assert_eq!(
        IdentityError::NotMdmTopic.to_string(),
        "the topic is not an MDM push topic"
    );
}

#[test]
fn a_key_of_a_kind_that_is_not_rsa_or_that_does_not_parse_is_refused_with_the_reason() {
    let pem = cert(TOPIC, 1_700_000_000, 1_900_000_000);
    // An EC key in its own SEC1 shape, and a PKCS#1 and a PKCS#8 block of bytes that are no key.
    let sec1 = "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAoAoGCCqGSM49\nAwEHoUQDQgAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n-----END EC PRIVATE KEY-----\n";
    let junk = |label: &str| {
        format!("-----BEGIN {label}-----\nAAAAAAAAAAAAAAAAAAAAAAAA\n-----END {label}-----\n")
    };
    for key in [
        sec1.to_owned(),
        junk("RSA PRIVATE KEY"),
        junk("PRIVATE KEY"),
    ] {
        let error = PushIdentity::parse(pem.as_bytes(), key.as_bytes()).unwrap_err();
        assert!(matches!(error, IdentityError::Key(_)), "{error:?}");
    }
}
