//! The RSA key pair of the ABM server entry ([`crate::abm::keys`]).

use rsa::pkcs8::EncodePrivateKey as _;

use crate::abm::fake::NOW;
use crate::abm::keys::{
    KEY_BITS, KeyError, check_certificate, generate, parse_key, pkcs1_pem, self_signed, year_of,
};

/// Proves: a key the coordinator makes is written as the PKCS#1 PEM NanoDEP and Apple's tooling
/// write (`BEGIN RSA PRIVATE KEY`), reads back as the same key, and so does the same key written
/// as PKCS#8; its self-signed certificate says it is for that key, is named
/// `hostkeeper-mdm-abm`, and is valid from 1 January of the year before the time given to 1 January ten
/// years after that year.
///
/// Catches: a key that no tool of ABM's side can read, and a certificate made for another key,
/// which ABM would take for the server entry and then fail to encrypt a token to.
#[test]
fn a_made_key_round_trips_and_its_certificate_is_for_it() {
    let key = generate(KEY_BITS).expect("a key");
    let pem = pkcs1_pem(&key).expect("a PEM");
    assert!(
        pem.starts_with("-----BEGIN RSA PRIVATE KEY-----\n"),
        "{pem:?}"
    );
    assert_eq!(parse_key(pem.as_bytes()).expect("reads back"), key);
    let pkcs8 = key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("PKCS#8");
    assert_eq!(parse_key(pkcs8.as_bytes()).expect("PKCS#8 reads"), key);

    let cert = self_signed(&key, NOW).expect("a certificate");
    assert_eq!(check_certificate(cert.as_bytes(), Some(&key)), Ok(()));
    assert_eq!(check_certificate(cert.as_bytes(), None), Ok(()));
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert.as_bytes()).expect("PEM");
    let parsed = pem.parse_x509().expect("a certificate");
    assert_eq!(parsed.subject().to_string(), "CN=hostkeeper-mdm-abm");
    let validity = parsed.validity();
    // NOW is in November 2023.
    assert_eq!(validity.not_before.to_datetime().year(), 2022);
    assert_eq!(validity.not_after.to_datetime().year(), 2033);
}

/// Proves: text that is no RSA key (bytes that are not UTF-8, a PEM with nothing inside, a key
/// of another type) is [`KeyError::NotAKey`], and a key of fewer than 1024 bits is not made.
///
/// Catches: a file of the wrong kind taken as the ABM key, which would stop the first token's
/// decryption with an error that names nothing.
#[test]
fn text_that_is_no_rsa_key_is_refused() {
    assert_eq!(parse_key(&[0xff, 0xfe]), Err(KeyError::NotAKey));
    assert_eq!(
        parse_key(b"-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n"),
        Err(KeyError::NotAKey)
    );
    let ec = rcgen::KeyPair::generate()
        .expect("an EC key")
        .serialize_pem();
    assert_eq!(parse_key(ec.as_bytes()), Err(KeyError::NotAKey));
    assert_eq!(generate(512).map(drop), Err(KeyError::Make));
}

/// Proves: a certificate is judged: text that is no PEM, a PEM of a key and a PEM whose body is no
/// DER are [`KeyError::NotACertificate`]; a certificate of another RSA key and one of an EC key
/// are [`KeyError::Mismatch`] for a key given; and a key too small for the signature of a
/// certificate makes none.
///
/// Catches: a certificate uploaded for a key it does not belong to (ABM would encrypt the token to
/// another key and the coordinator could not open it).
#[test]
fn a_certificate_is_judged_against_the_key() {
    let key = generate(KEY_BITS).expect("a key");
    let other = generate(KEY_BITS).expect("another key");
    let cert_of_other = self_signed(&other, NOW).expect("a certificate");
    assert_eq!(
        check_certificate(cert_of_other.as_bytes(), Some(&key)),
        Err(KeyError::Mismatch)
    );
    let ec = rcgen::generate_simple_self_signed(vec!["a.test".to_owned()]).expect("an EC cert");
    assert_eq!(
        check_certificate(ec.cert.pem().as_bytes(), Some(&key)),
        Err(KeyError::Mismatch)
    );
    for not_a_cert in [
        &b"hello"[..],
        pkcs1_pem(&key).expect("a PEM").as_bytes(),
        b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        assert_eq!(
            check_certificate(not_a_cert, Some(&key)),
            Err(KeyError::NotACertificate)
        );
    }
    let small = generate(1024).expect("a small key");
    assert_eq!(self_signed(&small, NOW), Err(KeyError::Make));
}

/// Proves: the year of a unix time is the calendar year in UTC, at the edges of a year and a
/// leap day.
///
/// Catches: a certificate dated a year off at New Year.
#[test]
fn the_year_of_a_time_is_the_calendar_year() {
    for (unix, year) in [
        (0, 1970),
        (951_782_400, 2000),
        (1_700_000_000, 2023),
        (1_704_067_199, 2023),
        (1_704_067_200, 2024),
        (4_102_444_799, 2099),
        (4_102_444_800, 2100),
    ] {
        assert_eq!(year_of(unix), year, "{unix}");
    }
}
