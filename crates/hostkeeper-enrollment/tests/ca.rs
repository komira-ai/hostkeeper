//! The certificate module: the request profile, the two certificate profiles, the cap, the
//! key file and the bundle. Certificates are parsed back with x509-parser and verified with
//! rustls; the clock is fixed.

mod support;

use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enrollment::ca::{Ca, CaError, PollBundle};
use hostkeeper_enrollment::ca::{
    CaFiles, CsrDefect, HOST_VALIDITY_SECS, MAX_CSR_BYTES, P256Key, ProfileError,
    SERVER_VALIDITY_CAP_SECS, ServerNames, SignError, SignRequest, Signer, Subject, check_request,
};
use hostkeeper_enrollment::fs::RealFs;
use rcgen::{
    BasicConstraints, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    PublicKeyData, SanType,
};
use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use sha2::{Digest, Sha256};
use support::{FixedClock, HostKey, NOW, TestCa, csr_of, files, host_id, scratch, subject};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::FromDer as _;

fn parse(der: &[u8]) -> X509Certificate<'_> {
    x509_parser::parse_x509_certificate(der)
        .expect("a certificate")
        .1
}

/// The common names of a subject or issuer, in order.
fn common_names(name: &x509_parser::x509::X509Name<'_>) -> Vec<String> {
    name.iter_common_name()
        .map(|cn| cn.as_str().expect("a string").to_owned())
        .collect()
}

/// (serverAuth, clientAuth, any other purpose) of a certificate's extended key usage.
fn purposes(cert: &X509Certificate<'_>) -> (bool, bool, bool) {
    let eku = cert
        .extended_key_usage()
        .expect("one EKU")
        .expect("an EKU")
        .value;
    let other = eku.any
        || eku.code_signing
        || eku.email_protection
        || eku.time_stamping
        || eku.ocsp_signing
        || !eku.other.is_empty();
    (eku.server_auth, eku.client_auth, other)
}

/// `dns` as the names of a server certificate with no IP address.
fn dns_only(dns: &[String]) -> ServerNames<'_> {
    ServerNames { dns, ips: &[] }
}

fn server_names() -> Vec<String> {
    vec![
        "coordinator.example".to_owned(),
        "hostkeeper.example".to_owned(),
    ]
}

/// Catches a host certificate whose validity is not exactly 7 days from the injected clock,
/// whose extended key usage is not `clientAuth` alone, that is not `CA:FALSE`, or whose subject
/// is the CSR's (the placeholder or another id) instead of the approved `host:<id>`.
#[test]
fn a_p256_request_gets_a_seven_day_client_certificate() {
    let ca = TestCa::new("seven-day");
    for csr_subject in ["host:pending", "host:someone-else"] {
        let host = HostKey::new();
        let verified = check_request(&host.csr(csr_subject)).expect("in profile");
        assert_eq!(verified.subject_id().as_str(), &csr_subject[5..]);
        assert_eq!(verified.key().key_hash(), host.key_hash());
        let cert = ca
            .ca
            .issue_host(&host_id("web-1"), verified.key(), HOST_VALIDITY_SECS, 7)
            .expect("issued");
        assert_eq!(
            (cert.serial, cert.not_before, cert.not_after),
            (7, NOW, NOW + 7 * 24 * 60 * 60)
        );
        let parsed = parse(&cert.der);
        assert_eq!(parsed.validity().not_before.timestamp(), NOW as i64);
        assert_eq!(
            parsed.validity().not_after.timestamp(),
            (NOW + HOST_VALIDITY_SECS) as i64
        );
        assert_eq!(common_names(parsed.subject()), ["host:web-1"]);
        assert_eq!(common_names(parsed.issuer()), ["hostkeeper CA"]);
        assert_eq!(parsed.subject().iter_attributes().count(), 1);
        assert_eq!(purposes(&parsed), (false, true, false));
        let constraints = parsed.basic_constraints().expect("one").expect("present");
        assert!(!constraints.value.ca);
        let usage = parsed.key_usage().expect("one").expect("present").value;
        assert!(usage.digital_signature() && !usage.key_cert_sign());
        assert_eq!(parsed.raw_serial(), [7]);
        assert_eq!(parsed.public_key().raw, host.p256().spki_der());
    }
}

/// Catches `issue_host` signing for another time than the validity it is given, and taking a
/// validity of 0 or over the 7-day cap instead of refusing it.
#[test]
fn a_host_certificate_is_valid_for_the_time_asked_up_to_the_cap() {
    let ca = TestCa::new("host-validity");
    let host = HostKey::new();
    for (secs, serial) in [(1, 2), (60, 3), (HOST_VALIDITY_SECS, 4)] {
        let cert = ca
            .ca
            .issue_host(&host_id("web-1"), &host.p256(), secs, serial)
            .expect("issued");
        assert_eq!((cert.not_before, cert.not_after), (NOW, NOW + secs));
        let parsed = parse(&cert.der);
        assert_eq!(
            parsed.validity().not_after.timestamp(),
            (NOW + secs) as i64,
            "{secs}"
        );
    }
    assert_eq!(
        ca.ca.issue_host(&host_id("web-1"), &host.p256(), 0, 5),
        Err(SignError::EmptyValidity)
    );
    assert_eq!(
        ca.ca
            .issue_host(&host_id("web-1"), &host.p256(), HOST_VALIDITY_SECS + 1, 5),
        Err(SignError::ValidityOverCap {
            requested: HOST_VALIDITY_SECS + 1,
            cap: HOST_VALIDITY_SECS,
        })
    );
}

/// Catches the CA signing a key of another type or curve: P-384, Ed25519 and RSA requests
/// are each refused `key_not_p256` before their signature is looked at.
#[test]
fn a_key_that_is_not_p256_is_refused() {
    let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).expect("P-384");
    let ed25519 = KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("Ed25519");
    let rsa = include_bytes!("fixtures/rsa2048-csr.der").to_vec();
    for csr in [
        csr_of(&p384, subject("host:pending")),
        csr_of(&ed25519, subject("host:pending")),
        rsa,
    ] {
        let refused = check_request(&csr).expect_err("not P-256");
        assert_eq!(refused, ProfileError::KeyNotP256);
        assert_eq!(refused.token(), "key_not_p256");
        assert_eq!(
            refused.to_string(),
            "key_not_p256: the key is not an uncompressed ECDSA P-256 key"
        );
    }
    // A compressed P-256 point is refused too, as is a key with a byte too many.
    let spki = HostKey::new().pair.subject_public_key_info();
    let mut compressed = spki.clone();
    compressed[26] = 0x02;
    let mut longer = spki.clone();
    longer.push(0);
    for refused in [compressed, longer] {
        assert_eq!(
            P256Key::from_spki(&refused).expect_err("not P-256"),
            ProfileError::KeyNotP256
        );
    }
    assert!(P256Key::from_spki(&spki).is_ok());
}

/// Catches a request whose self-signature was altered being signed, and every other way a
/// request is `bad_csr`: too large, not DER, bytes after it, and a subject that is not one
/// common name `host:<id>`.
#[test]
fn a_csr_with_a_bad_self_signature_is_refused() {
    let host = HostKey::new();
    let good = host.csr("host:pending");
    let mut tampered = good.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let mut trailing = good.clone();
    trailing.push(0);
    let mut two_names = subject("host:pending");
    two_names
        .distinguished_name
        .push(DnType::OrganizationName, "fleet");
    let mut organisation = subject("x");
    organisation.distinguished_name = rcgen::DistinguishedName::new();
    organisation
        .distinguished_name
        .push(DnType::OrganizationName, "host:web-1");
    let cases = [
        (tampered, CsrDefect::Signature),
        (vec![0u8; MAX_CSR_BYTES + 1], CsrDefect::TooLarge),
        (b"not a request".to_vec(), CsrDefect::NotDer),
        (trailing, CsrDefect::NotDer),
        (csr_of(&host.pair, two_names), CsrDefect::Subject),
        (csr_of(&host.pair, organisation), CsrDefect::Subject),
        (host.csr("web-1"), CsrDefect::Subject),
        (host.csr("host:Web_1"), CsrDefect::Subject),
        (host.csr("host:"), CsrDefect::Subject),
    ];
    for (csr, defect) in cases {
        let refused = check_request(&csr).expect_err("bad_csr");
        assert_eq!(refused, ProfileError::BadCsr(defect), "{defect}");
        assert_eq!(refused.token(), "bad_csr");
    }
    let messages: Vec<String> = [
        CsrDefect::TooLarge,
        CsrDefect::NotDer,
        CsrDefect::Signature,
        CsrDefect::Subject,
    ]
    .into_iter()
    .map(|defect| ProfileError::BadCsr(defect).to_string())
    .collect();
    assert_eq!(
        messages,
        [
            "bad_csr: the request is larger than 4096 bytes",
            "bad_csr: the request is not one DER PKCS#10 request",
            "bad_csr: the request's self-signature does not verify",
            "bad_csr: the subject is not one common name host:<id>",
        ]
    );
    // A request of exactly the largest size is read (and then refused as not DER).
    let at_limit = check_request(&[0u8; MAX_CSR_BYTES]).expect_err("zeros");
    assert_eq!(at_limit, ProfileError::BadCsr(CsrDefect::NotDer));
}

/// Every subjectAltName of `cert`, each as `uri:`, `dns:` or `ip:` and its value.
fn alt_names(cert: &X509Certificate<'_>) -> Vec<String> {
    cert.subject_alternative_name()
        .expect("one")
        .map(|san| {
            san.value
                .general_names
                .iter()
                .map(|name| match name {
                    GeneralName::URI(uri) => format!("uri:{uri}"),
                    GeneralName::DNSName(dns) => format!("dns:{dns}"),
                    other => format!("other:{other:?}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Catches the CA copying anything the request asked for: a CSR asking for `serverAuth`, a
/// subjectAltName, `CA:TRUE` and `keyCertSign` is accepted, and its certificate has none of
/// them: its one subjectAltName is the fixed host role every host certificate names.
#[test]
fn requested_extensions_are_never_copied() {
    let ca = TestCa::new("no-copy");
    let host = HostKey::new();
    let mut asking = subject("host:pending");
    asking.subject_alt_names = vec![SanType::DnsName(
        "evil.example".try_into().expect("an IA5 string"),
    )];
    asking.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    asking.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    asking.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let csr = csr_of(&host.pair, asking);
    let csr_has_request =
        x509_parser::certification_request::X509CertificationRequest::from_der(&csr)
            .map(|(_, parsed)| parsed.requested_extensions().is_some());
    assert_eq!(csr_has_request.ok(), Some(true));
    let verified = check_request(&csr).expect("extensions are allowed in a request");
    let cert = ca
        .ca
        .issue_host(&host_id("web-2"), verified.key(), HOST_VALIDITY_SECS, 9)
        .expect("issued");
    let parsed = parse(&cert.der);
    assert_eq!(purposes(&parsed), (false, true, false));
    assert_eq!(alt_names(&parsed), ["uri:urn:hostkeeper:host-role:host"]);
    assert!(
        !parsed
            .basic_constraints()
            .expect("one")
            .expect("set")
            .value
            .ca
    );
    let usage = parsed.key_usage().expect("one").expect("present").value;
    assert!(!usage.key_cert_sign());
}

/// Catches a cap held only by callers: a host certificate asked for one second longer than 7
/// days, and a server certificate one second longer than its cap, are refused
/// `validity_over_cap`; each cap itself is accepted; an empty validity, a reserved serial and a
/// clock whose window does not fit a certificate are refused too.
#[test]
fn a_validity_over_the_cap_is_refused() {
    let ca = TestCa::new("cap");
    let host = HostKey::new();
    let key = host.p256();
    let web = host_id("web-1");
    let names = server_names();
    let sign = |subject, validity_secs, serial| {
        ca.ca.sign(&SignRequest {
            subject,
            key: &key,
            validity_secs,
            serial,
        })
    };
    let over = sign(Subject::Host(&web), HOST_VALIDITY_SECS + 1, 2).expect_err("over");
    assert_eq!(
        over,
        SignError::ValidityOverCap {
            requested: HOST_VALIDITY_SECS + 1,
            cap: HOST_VALIDITY_SECS
        }
    );
    assert_eq!(over.token(), Some("validity_over_cap"));
    assert_eq!(
        over.to_string(),
        "validity_over_cap: 604801 seconds asked for, at most 604800 allowed"
    );
    let server_over = ca
        .ca
        .issue_server(&key, dns_only(&names), SERVER_VALIDITY_CAP_SECS + 1, 2)
        .expect_err("over");
    assert_eq!(
        server_over,
        SignError::ValidityOverCap {
            requested: SERVER_VALIDITY_CAP_SECS + 1,
            cap: SERVER_VALIDITY_CAP_SECS
        }
    );
    let at_cap = ca
        .ca
        .issue_server(&key, dns_only(&names), SERVER_VALIDITY_CAP_SECS, 3)
        .expect("the cap itself");
    assert_eq!(at_cap.not_after, NOW + SERVER_VALIDITY_CAP_SECS);
    assert!(sign(Subject::Host(&web), HOST_VALIDITY_SECS, 4).is_ok());
    let short = sign(Subject::Host(&web), 1, 5).expect("one second");
    assert_eq!(short.not_after, NOW + 1);
    let refusals = [
        (sign(Subject::Host(&web), 0, 2), SignError::EmptyValidity),
        (sign(Subject::Host(&web), 60, 0), SignError::BadSerial(0)),
        (sign(Subject::Host(&web), 60, 1), SignError::BadSerial(1)),
    ];
    for (got, want) in refusals {
        let got = got.expect_err("refused");
        assert_eq!(got.token(), None);
        assert_eq!(got, want);
    }
    assert_eq!(
        SignError::EmptyValidity.to_string(),
        "a certificate needs a validity of at least one second"
    );
    assert_eq!(
        SignError::BadSerial(1).to_string(),
        "serial 1 is not one a certificate of this CA may carry"
    );
    ca.clock.set(u64::MAX - 10);
    assert_eq!(
        sign(Subject::Host(&web), 60, 2).expect_err("past u64"),
        SignError::ClockOutOfRange { now: u64::MAX - 10 }
    );
    // The year 10000 does not fit a certificate's time.
    ca.clock.set(253_402_300_800);
    let far = sign(Subject::Host(&web), 60, 2).expect_err("past the year 9999");
    assert_eq!(
        far.to_string(),
        "the clock reads 253402300800, whose validity window does not fit a certificate"
    );
}

/// Catches a server certificate that a TLS client does not accept for the coordinator's name
/// against the CA bundle, one that also works as a client certificate, a host certificate that
/// works as a server's, and DNS names outside the grammar being signed.
#[test]
fn the_server_certificate_is_server_auth_only_and_chains_to_the_ca() {
    let ca = TestCa::new("server");
    let coordinator = HostKey::new();
    let host = HostKey::new();
    let names = server_names();
    let server = ca
        .ca
        .issue_server(&coordinator.p256(), dns_only(&names), 30 * 24 * 60 * 60, 2)
        .expect("server certificate");
    let client = ca
        .ca
        .issue_host(&host_id("web-1"), &host.p256(), HOST_VALIDITY_SECS, 3)
        .expect("client certificate");
    let parsed = parse(&server.der);
    assert_eq!(purposes(&parsed), (true, false, false));
    assert_eq!(common_names(parsed.subject()), ["hostkeeper coordinator"]);
    let san = parsed
        .subject_alternative_name()
        .expect("one")
        .expect("present");
    assert_eq!(san.value.general_names.len(), 2);

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            ca.ca.bundle().certificate_der().to_vec(),
        ))
        .expect("the CA certificate is a root");
    let roots = Arc::new(roots);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let servers = WebPkiServerVerifier::builder_with_provider(roots.clone(), provider.clone())
        .build()
        .expect("a server verifier");
    let clients = WebPkiClientVerifier::builder_with_provider(roots, provider)
        .build()
        .expect("a client verifier");
    let now = UnixTime::since_unix_epoch(Duration::from_secs(NOW + 60));
    let name = ServerName::try_from("coordinator.example").expect("a name");
    let server_der = CertificateDer::from(server.der.clone());
    let client_der = CertificateDer::from(client.der.clone());
    assert!(
        servers
            .verify_server_cert(&server_der, &[], &name, &[], now)
            .is_ok()
    );
    let other = ServerName::try_from("other.example").expect("a name");
    assert!(
        servers
            .verify_server_cert(&server_der, &[], &other, &[], now)
            .is_err()
    );
    assert!(clients.verify_client_cert(&client_der, &[], now).is_ok());
    assert!(clients.verify_client_cert(&server_der, &[], now).is_err());
    assert!(
        servers
            .verify_server_cert(&client_der, &[], &name, &[], now)
            .is_err()
    );
    // Past its 7 days the host certificate is refused.
    let late = UnixTime::since_unix_epoch(Duration::from_secs(NOW + HOST_VALIDITY_SECS + 1));
    assert!(clients.verify_client_cert(&client_der, &[], late).is_err());

    let long = format!(
        "{}.{}.example",
        "a".repeat(61),
        vec!["a".repeat(60); 3].join(".")
    );
    assert_eq!(long.len(), 252);
    let too_long = format!("ab{long}");
    let refused = [
        (vec![], SignError::NoDnsName),
        (
            vec!["Bad_Name.example".to_owned()],
            SignError::BadDnsName("Bad_Name.example".to_owned()),
        ),
        (
            vec!["é.example".to_owned()],
            SignError::BadDnsName("é.example".to_owned()),
        ),
        (vec![too_long.clone()], SignError::BadDnsName(too_long)),
    ];
    for (dns, want) in refused {
        let got = ca
            .ca
            .issue_server(&coordinator.p256(), dns_only(&dns), 60, 9)
            .expect_err("refused");
        assert_eq!(got, want);
    }
    assert!(
        ca.ca
            .issue_server(&coordinator.p256(), dns_only(&[long]), 60, 9)
            .is_ok()
    );
    assert_eq!(
        SignError::NoDnsName.to_string(),
        "a server certificate needs at least one DNS name"
    );
}

/// Catches a server certificate that leaves out the IP addresses asked for, names them in
/// another form than an IP subjectAltName (a TLS client dialing the address would refuse it),
/// drops or reorders the DNS names beside them, or is accepted for an address it does not name.
#[test]
fn a_server_certificate_names_its_ip_addresses_after_its_dns_names() {
    let ca = TestCa::new("server-ips");
    let coordinator = HostKey::new();
    let dns = server_names();
    let v4: IpAddr = "192.0.2.7".parse().expect("an address");
    let v6: IpAddr = "2001:db8::7".parse().expect("an address");
    let server = ca
        .ca
        .issue_server(
            &coordinator.p256(),
            ServerNames {
                dns: &dns,
                ips: &[v4, v6],
            },
            60,
            2,
        )
        .expect("server certificate");
    let parsed = parse(&server.der);
    let san = parsed
        .subject_alternative_name()
        .expect("one")
        .expect("present");
    let named: Vec<String> = san
        .value
        .general_names
        .iter()
        .map(|name| match name {
            GeneralName::DNSName(dns) => format!("dns:{dns}"),
            GeneralName::IPAddress(bytes) => format!("ip:{bytes:?}"),
            other => panic!("an unexpected name {other:?}"),
        })
        .collect();
    assert_eq!(
        named,
        [
            "dns:coordinator.example".to_owned(),
            "dns:hostkeeper.example".to_owned(),
            "ip:[192, 0, 2, 7]".to_owned(),
            format!(
                "ip:{:?}",
                [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]
            ),
        ]
    );

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            ca.ca.bundle().certificate_der().to_vec(),
        ))
        .expect("the CA certificate is a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let servers = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .expect("a server verifier");
    let now = UnixTime::since_unix_epoch(Duration::from_secs(NOW + 30));
    let der = CertificateDer::from(server.der.clone());
    let verify = |name: &str| {
        let name = ServerName::try_from(name.to_owned()).expect("a name");
        servers
            .verify_server_cert(&der, &[], &name, &[], now)
            .is_ok()
    };
    assert!(verify("192.0.2.7"));
    assert!(verify("2001:db8::7"));
    assert!(verify("hostkeeper.example"));
    assert!(!verify("192.0.2.8"));
    assert!(!verify("2001:db8::8"));

    let no_dns = ca
        .ca
        .issue_server(
            &coordinator.p256(),
            ServerNames {
                dns: &[],
                ips: &[v4],
            },
            60,
            3,
        )
        .expect_err("an address alone is no server certificate");
    assert_eq!(no_dns, SignError::NoDnsName);
}

/// Catches a CA key file that its group or others may read, or that is a symbolic link, being
/// used; and a CA that starts from a missing, wrong or mismatched file.
#[test]
fn the_ca_key_file_must_be_owner_only() {
    let made = TestCa::new("owner-only");
    let paths = files(made.dir.path());
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o7777
    };
    assert_eq!(mode(paths.key), 0o600);
    assert_eq!(mode(paths.certificate), 0o644);
    let clock = FixedClock::at(NOW);
    let opened = Ca::open(&RealFs, paths, clock.clone()).expect("a 0600 key file loads");
    assert_eq!(opened.bundle(), made.ca.bundle());

    std::fs::set_permissions(paths.key, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let refused = Ca::open(&RealFs, paths, clock.clone()).expect_err("0644");
    assert!(
        matches!(refused, CaError::KeyNotPrivate { .. }),
        "{refused}"
    );
    assert!(
        refused.to_string().starts_with(&format!(
            "the CA key file {} must be a regular file owned by uid ",
            paths.key.display()
        )),
        "{refused}"
    );
    std::fs::set_permissions(paths.key, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let linked = made.dir.path().join("linked.key");
    std::os::unix::fs::symlink(paths.key, &linked).expect("symlink");
    let through_link = CaFiles {
        key: &linked,
        certificate: paths.certificate,
    };
    let refused = Ca::open(&RealFs, through_link, clock.clone()).expect_err("a link");
    assert!(matches!(refused, CaError::Io { .. }), "{refused}");

    let again = Ca::init(&RealFs, paths, clock.clone()).expect_err("made once");
    assert_eq!(
        again.to_string(),
        format!(
            "{} already exists; the CA is made once",
            paths.key.display()
        )
    );

    // Another CA's certificate beside this key, a P-384 key, a key that is not PKCS#8, a
    // certificate that is not one, and missing files.
    let other = TestCa::new("owner-only-other");
    let other_paths = files(other.dir.path());
    let mixed = CaFiles {
        key: paths.key,
        certificate: other_paths.certificate,
    };
    let refused = Ca::open(&RealFs, mixed, clock.clone()).expect_err("not this key's");
    assert!(
        matches!(refused, CaError::CertificateNotOfKey { .. }),
        "{refused}"
    );

    let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).expect("P-384");
    std::fs::write(other_paths.key, p384.serialize_der()).expect("write");
    let refused = Ca::open(&RealFs, other_paths, clock.clone()).expect_err("P-384");
    assert_eq!(
        refused.to_string(),
        format!(
            "the CA key file {} is not a PKCS#8 ECDSA P-256 key: the key is not ECDSA P-256",
            other_paths.key.display()
        )
    );
    std::fs::write(other_paths.key, b"not a key").expect("write");
    let refused = Ca::open(&RealFs, other_paths, clock.clone()).expect_err("garbage");
    assert!(matches!(refused, CaError::BadKey { .. }), "{refused}");

    std::fs::write(other_paths.certificate, b"not a certificate").expect("write");
    let refused = Ca::open(&RealFs, mixed, clock.clone()).expect_err("garbage");
    assert!(
        matches!(refused, CaError::BadCertificate { .. }),
        "{refused}"
    );

    let empty = scratch("owner-only-empty");
    let missing = files(empty.path());
    let refused = Ca::open(&RealFs, missing, clock.clone()).expect_err("no key");
    assert_eq!(
        refused.to_string(),
        format!(
            "{} does not exist; make the CA first",
            missing.key.display()
        )
    );
    let key_only = CaFiles {
        key: paths.key,
        certificate: missing.certificate,
    };
    let refused = Ca::open(&RealFs, key_only, clock.clone()).expect_err("no certificate");
    assert!(matches!(refused, CaError::Missing { .. }), "{refused}");
    let refused = Ca::init(&RealFs, key_only, clock.clone()).expect_err("the key exists");
    assert!(matches!(refused, CaError::Exists { .. }), "{refused}");
    let cert_only = CaFiles {
        key: missing.key,
        certificate: paths.certificate,
    };
    let refused = Ca::init(&RealFs, cert_only, clock.clone()).expect_err("the cert exists");
    assert_eq!(
        refused.to_string(),
        format!(
            "{} already exists; the CA is made once",
            paths.certificate.display()
        )
    );
    let no_name = CaFiles {
        key: &empty.path().join(".."),
        certificate: missing.certificate,
    };
    let refused = Ca::init(&RealFs, no_name, clock.clone()).expect_err("no file name");
    assert!(matches!(refused, CaError::BadPath { .. }), "{refused}");
    let no_cert_name = CaFiles {
        key: missing.key,
        certificate: &empty.path().join(".."),
    };
    let refused = Ca::init(&RealFs, no_cert_name, clock.clone()).expect_err("no file name");
    assert!(matches!(refused, CaError::BadPath { .. }), "{refused}");

    clock.set(u64::MAX);
    let refused = Ca::init(&RealFs, missing, clock).expect_err("the clock");
    assert!(
        matches!(refused, CaError::ClockOutOfRange { .. }),
        "{refused}"
    );
}

/// Catches a fingerprint that is not the SHA-256 of the CA certificate's DER (the value a
/// daemon config pins), or a CA certificate that changes when the CA is opened again.
#[test]
fn the_ca_fingerprint_is_the_sha256_of_the_ca_certificate() {
    let ca = TestCa::new("fingerprint");
    let bundle = ca.ca.bundle();
    let mut independent = [0u8; 32];
    independent.copy_from_slice(&Sha256::digest(bundle.certificate_der()));
    assert_eq!(bundle.sha256(), &independent);
    assert_eq!(bundle.sha256_hex(), hex::encode(independent));
    let reopened = Ca::open(&RealFs, files(ca.dir.path()), ca.clock.clone()).expect("open");
    assert_eq!(reopened.bundle().sha256_hex(), bundle.sha256_hex());
    let parsed = parse(bundle.certificate_der());
    assert!(
        parsed
            .basic_constraints()
            .expect("one")
            .expect("set")
            .value
            .ca
    );
    assert_eq!(common_names(parsed.subject()), ["hostkeeper CA"]);
    assert_eq!(parsed.raw_serial(), [1]);
    let poll = PollBundle {
        certificate_der: vec![1, 2, 3],
        host_id: host_id("web-1"),
        ca: bundle.clone(),
    };
    assert_eq!(poll.ca.sha256_hex(), bundle.sha256_hex());
    assert!(format!("{:?}", ca.ca).starts_with("Ca("));
}

/// Proves: a host certificate names the fixed host role `host` as its one subjectAltName, after
/// the default prefix, or after the prefix the CA is given (the coordinator's
/// `--host-role-uri-prefix`), whatever the host id; the server certificate names no URI.
///
/// Catches: a host certificate without a host-role URI, which readers that require one refuse,
/// or one naming a role of the host, which would go stale at its first move.
#[test]
fn every_host_certificate_names_the_fixed_host_role() {
    let test = TestCa::new("host-role");
    let host = HostKey::new();
    let issued = test
        .ca
        .issue_host(&host_id("db-7"), &host.p256(), HOST_VALIDITY_SECS, 5)
        .expect("issued");
    assert_eq!(
        alt_names(&parse(&issued.der)),
        ["uri:urn:hostkeeper:host-role:host"]
    );
    let server = test
        .ca
        .issue_server(
            &HostKey::new().p256(),
            ServerNames {
                dns: &["coordinator.example".to_owned()],
                ips: &[],
            },
            SERVER_VALIDITY_CAP_SECS,
            6,
        )
        .expect("server certificate");
    assert_eq!(alt_names(&parse(&server.der)), ["dns:coordinator.example"]);

    let prefixed = test.ca.with_host_role_uri_prefix("urn:kbf:host-role:");
    let issued = prefixed
        .issue_host(&host_id("web-1"), &host.p256(), HOST_VALIDITY_SECS, 7)
        .expect("issued");
    assert_eq!(
        alt_names(&parse(&issued.der)),
        ["uri:urn:kbf:host-role:host"]
    );
}

/// Proves: a CA given a host-role prefix that is not ASCII signs no host certificate, refused
/// naming the URI, and still signs a server certificate.
///
/// Catches: a host certificate signed with a host-role URI no reader can parse.
#[test]
fn a_host_role_prefix_that_is_not_ascii_signs_no_host_certificate() {
    let ca = TestCa::new("host-role-bad")
        .ca
        .with_host_role_uri_prefix("urn:r\u{f4}le:");
    let refused = ca
        .issue_host(
            &host_id("web-1"),
            &HostKey::new().p256(),
            HOST_VALIDITY_SECS,
            5,
        )
        .expect_err("not ASCII");
    assert_eq!(
        refused,
        SignError::BadHostRoleUri("urn:r\u{f4}le:host".to_owned())
    );
    assert_eq!(
        refused.to_string(),
        "the host-role URI \"urn:r\u{f4}le:host\" is not one a certificate carries"
    );
    let server = ca.issue_server(
        &HostKey::new().p256(),
        ServerNames {
            dns: &["coordinator.example".to_owned()],
            ips: &[],
        },
        SERVER_VALIDITY_CAP_SECS,
        6,
    );
    assert!(server.is_ok());
}
