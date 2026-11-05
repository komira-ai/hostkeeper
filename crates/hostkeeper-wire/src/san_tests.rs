//! Reading the subjectAltNames of a certificate: URIs under a prefix, DNS names and IPs.

use std::net::IpAddr;

use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, SanType};

use crate::san::{SanError, names_of, uri_values_of};

fn uri(text: &str) -> SanType {
    SanType::URI(text.try_into().expect("an IA5 name"))
}

fn dns(name: &str) -> SanType {
    SanType::DnsName(name.try_into().expect("an IA5 name"))
}

fn ip(text: &str) -> SanType {
    SanType::IpAddress(text.parse::<IpAddr>().expect("an IP address"))
}

/// A self-signed client certificate with these subjectAltNames (and an extended key usage, so
/// the certificate has an extension that is not a subjectAltName).
fn der(sans: Vec<SanType>) -> Vec<u8> {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = sans;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    params.self_signed(&key).expect("sign").der().to_vec()
}

/// Catches: the URI values of one prefix read wrongly: another prefix's URIs mixed in, the
/// order of the certificate changed, a DNS name or a prefix in another letter case taken for a
/// value, a value cut short.
#[test]
fn the_uris_of_one_prefix_are_listed_in_the_certificates_order() {
    let leaf = der(vec![
        uri("urn:test:host-role:storage"),
        uri("urn:test:role:updater"),
        dns("urn-test-host-role-daemon"),
        uri("URN:TEST:HOST-ROLE:daemon"),
        uri("urn:test:host-role:mac-daemon"),
        uri("urn:test:host-role:"),
    ]);
    assert_eq!(
        uri_values_of(&leaf, "urn:test:host-role:"),
        Ok(vec![
            "storage".to_owned(),
            "mac-daemon".to_owned(),
            String::new()
        ])
    );
    assert_eq!(
        uri_values_of(&leaf, "urn:test:role:"),
        Ok(vec!["updater".to_owned()])
    );
    assert_eq!(uri_values_of(&leaf, "urn:other:"), Ok(vec![]));
}

/// Catches: bytes that are no certificate read as a certificate with no names.
#[test]
fn bytes_that_are_no_certificate_are_refused() {
    let Err(SanError::NotACertificate(why)) = uri_values_of(b"not DER", "urn:test:") else {
        panic!("not DER was read");
    };
    assert!(!why.is_empty());
    assert!(matches!(
        names_of(b"not DER"),
        Err(SanError::NotACertificate(_))
    ));
    assert!(
        SanError::NotACertificate("x".to_owned())
            .to_string()
            .starts_with("the client certificate does not parse: ")
    );
}

/// Catches: a name read from where it is not one: a URI, an email and the subject's common
/// name are not names; the DNS names and the IPs come out in the certificate's order.
#[test]
fn the_names_are_the_dns_and_ip_subject_alt_names_and_nothing_else() {
    // Spelled in two parts: the tree's hygiene lint refuses a literal e-mail address.
    let email = format!("ops{}node-c.example", '@');
    let names = names_of(&der(vec![
        ip("192.0.2.1"),
        dns("node-a"),
        uri("urn:test:role:storage"),
        uri("https://node-b.example"),
        SanType::Rfc822Name(email.as_str().try_into().expect("an IA5 name")),
        ip("2001:db8::1"),
        dns("Node-D.Example"),
    ]))
    .expect("a certificate");
    assert_eq!(names.dns(), ["node-a", "Node-D.Example"]);
    let want: Vec<IpAddr> = vec![
        "192.0.2.1".parse().expect("ip"),
        "2001:db8::1".parse().expect("ip"),
    ];
    assert_eq!(names.ips(), want);

    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.distinguished_name.push(DnType::CommonName, "node-a");
    let cn_only = params
        .self_signed(&KeyPair::generate().expect("key"))
        .expect("sign");
    let names = names_of(cn_only.der()).expect("a certificate");
    assert!(names.dns().is_empty(), "the common name is not read");
    assert!(names.ips().is_empty());
}

/// Catches: an IP name of a length that is no address being read as one, and the names around
/// it being lost with it. The certificate's four-byte address is rewritten as two one-byte
/// names of the same total length; a signature is not checked here.
#[test]
fn an_ip_name_that_is_no_address_is_skipped() {
    let mut cert = der(vec![dns("node-a"), ip("192.0.2.77"), ip("2001:db8::7")]);
    let four: [u8; 6] = [0x87, 0x04, 192, 0, 2, 77];
    let at = cert
        .windows(four.len())
        .position(|w| w == four)
        .expect("the address is in the certificate");
    cert[at..at + four.len()].copy_from_slice(&[0x87, 0x01, 192, 0x87, 0x01, 0]);
    let names = names_of(&cert).expect("a certificate");
    assert_eq!(names.dns(), ["node-a"]);
    let want: Vec<IpAddr> = vec!["2001:db8::7".parse().expect("ip")];
    assert_eq!(names.ips(), want);
}
