//! What is read of a certificate and whether a key is its.

use super::CertFacts;
use crate::mdm_pki_support::{TOPIC, other_key, pair};

/// Proves: the end of validity, the topic and the DNS names are read from the first certificate
/// of a PEM, and a certificate with no UID has no topic.
#[test]
fn a_certificates_facts_are_read() {
    let push = pair(Some(TOPIC), &[], 1_900_000_000);
    let facts = CertFacts::read(push.cert.as_bytes()).expect("a certificate");
    assert_eq!(facts.not_after_unix, 1_900_000_000);
    assert_eq!(facts.topic.as_deref(), Some(TOPIC));
    assert!(facts.names.is_empty());

    let tls = pair(
        None,
        &["mdm.example.com", "*.lab.example.com"],
        1_800_000_000,
    );
    let chain = format!("{}{}", tls.cert, push.cert);
    let facts = CertFacts::read(chain.as_bytes()).expect("a certificate");
    assert_eq!(facts.not_after_unix, 1_800_000_000);
    assert_eq!(facts.topic, None);
    assert_eq!(facts.names, ["mdm.example.com", "*.lab.example.com"]);
}

/// Proves: text that is no PEM, a PEM of another kind and a certificate whose DER is damaged read
/// as none.
#[test]
fn what_is_no_certificate_reads_as_none() {
    let made = pair(None, &[], 1_900_000_000);
    assert_eq!(CertFacts::read(b""), None);
    assert_eq!(CertFacts::read(b"not a certificate"), None);
    assert_eq!(CertFacts::read(made.key.as_bytes()), None);
    let damaged = "-----BEGIN CERTIFICATE-----\nAAAAAAAA\n-----END CERTIFICATE-----\n";
    assert_eq!(CertFacts::read(damaged.as_bytes()), None);
}

/// Proves: a key is the certificate's when its public half is the certificate's, and is not when
/// it is another key, no PEM, or a PEM whose key does not read.
#[test]
fn a_key_is_the_certificates_only_when_its_public_half_is() {
    let made = pair(None, &["mdm.example.com"], 1_900_000_000);
    let facts = CertFacts::read(made.cert.as_bytes()).expect("a certificate");
    assert!(facts.is_key(made.key.as_bytes()));
    assert!(!facts.is_key(other_key().as_bytes()));
    assert!(!facts.is_key(b"not a key"));
    assert!(!facts.is_key(made.cert.as_bytes()));
    let damaged = "-----BEGIN PRIVATE KEY-----\nAAAAAAAA\n-----END PRIVATE KEY-----\n";
    assert!(!facts.is_key(damaged.as_bytes()));
}

/// Proves: a name is named by an exact name or by a wildcard of one first label, without regard
/// to case, and not by a wildcard of a deeper name, of the bare domain or of another domain.
#[test]
fn a_name_is_named_exactly_or_by_a_wildcard_of_its_first_label() {
    let facts = |names: &[&str]| {
        CertFacts::read(pair(None, names, 1_900_000_000).cert.as_bytes()).expect("a certificate")
    };
    let exact = facts(&["Mdm.Example.com"]);
    assert!(exact.names_host("mdm.example.com"));
    assert!(!exact.names_host("other.example.com"));
    let wildcard = facts(&["*.example.com"]);
    assert!(wildcard.names_host("mdm.example.com"));
    assert!(wildcard.names_host("MDM.EXAMPLE.COM"));
    assert!(!wildcard.names_host("a.mdm.example.com"));
    assert!(!wildcard.names_host("example.com"));
    assert!(!wildcard.names_host("mdm.example.org"));
    assert!(!wildcard.names_host("localhost"));
    assert!(!facts(&[]).names_host("mdm.example.com"));
}
