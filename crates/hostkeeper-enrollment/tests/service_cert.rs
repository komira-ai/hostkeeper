//! The service certificate: what the CA signs for a service on a host (its profile, its 8-hour
//! validity), and what [`CaBundle::service_certificate`] reads back (exactly that profile, signed
//! by this CA). Certificates the CA would never sign are made with the CA's own key, so each
//! refusal is of one rule alone. The clock is fixed.

mod support;

use hostkeeper_enrollment::ca::{
    Ca, CaBundle, SERVICE_VALIDITY_SECS, ServerNames, ServiceCertificate, ServiceCertificateError,
    SignError, SignRequest, Signer, Subject,
};
use hostkeeper_enrollment::names::{HostId, ServiceName};
use rcgen::string::Ia5String;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use support::{HostKey, NOW, TestCa, files, host_id, subject};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{GeneralName, ParsedExtension};

fn service(name: &str) -> ServiceName {
    ServiceName::parse(name).expect("a service name")
}

fn parse(der: &[u8]) -> X509Certificate<'_> {
    x509_parser::parse_x509_certificate(der)
        .expect("a certificate")
        .1
}

fn uri(text: &str) -> SanType {
    SanType::URI(Ia5String::try_from(text).expect("ASCII"))
}

fn dns(text: &str) -> SanType {
    SanType::DnsName(Ia5String::try_from(text).expect("ASCII"))
}

/// The parameters of a certificate that passes every rule of the reader for `js-agent` on
/// `web-1`: each test changes one.
fn good() -> CertificateParams {
    let mut params = subject("service:js-agent");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.subject_alt_names = vec![uri("urn:hostkeeper:service:js-agent"), dns("web-1")];
    params
}

/// `params` signed for `key` by the CA's own key.
fn forge(test: &TestCa, key: &KeyPair, params: CertificateParams) -> Vec<u8> {
    let paths = files(test.dir.path());
    let ca_key = KeyPair::try_from(std::fs::read(paths.key).expect("the CA key").as_slice())
        .expect("a PKCS#8 key");
    let issuer = Issuer::new(subject("hostkeeper CA"), ca_key);
    params
        .signed_by(key, &issuer)
        .expect("signed")
        .der()
        .to_vec()
}

/// Asserts the reader refuses `params` signed by the CA, with `defect`.
fn refused(test: &TestCa, params: CertificateParams, defect: ServiceCertificateError) {
    let host = HostKey::new();
    let der = forge(test, &host.pair, params);
    assert_eq!(test.ca.bundle().service_certificate(&der), Err(defect));
}

/// Proves: a service certificate has the profile of the design: the common name
/// `service:<name>`, the URI and the host's DNS name as its only names, `clientAuth` alone,
/// `digitalSignature`, not a CA, the key of the request, 8 hours from the clock, the serial
/// given, and an authority key identifier as a host certificate has.
///
/// Catches: a service certificate a host certificate's reader could take for a host's, one valid
/// for a server, or one that lasts longer than 8 hours.
#[test]
fn a_service_certificate_names_the_service_and_the_host_for_eight_hours() {
    let test = TestCa::new("service-profile");
    let key = HostKey::new();
    let cert = test
        .ca
        .issue_service(&service("js-agent"), &host_id("web-1"), &key.p256(), 9)
        .expect("issued");
    assert_eq!(SERVICE_VALIDITY_SECS, 8 * 60 * 60);
    assert_eq!(
        (cert.serial, cert.not_before, cert.not_after),
        (9, NOW, NOW + 8 * 60 * 60)
    );
    let parsed = parse(&cert.der);
    let names: Vec<_> = parsed
        .subject()
        .iter_attributes()
        .map(|a| a.as_str().expect("text").to_owned())
        .collect();
    assert_eq!(names, ["service:js-agent"]);
    let alternatives: Vec<String> = parsed
        .subject_alternative_name()
        .expect("one")
        .expect("present")
        .value
        .general_names
        .iter()
        .map(|name| match name {
            GeneralName::URI(text) => format!("uri:{text}"),
            GeneralName::DNSName(text) => format!("dns:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        alternatives,
        ["uri:urn:hostkeeper:service:js-agent", "dns:web-1"]
    );
    let usage = parsed
        .extended_key_usage()
        .expect("one")
        .expect("present")
        .value;
    assert!(usage.client_auth);
    assert!(!(usage.any || usage.server_auth || usage.code_signing || usage.email_protection));
    assert!(!usage.time_stamping && !usage.ocsp_signing && usage.other.is_empty());
    let key_usage = parsed.key_usage().expect("one").expect("present").value;
    assert!(key_usage.digital_signature() && !key_usage.key_cert_sign());
    assert!(
        !parsed
            .basic_constraints()
            .expect("one")
            .expect("present")
            .value
            .ca
    );
    assert_eq!(parsed.raw_serial(), [9]);
    assert_eq!(parsed.public_key().raw, key.p256().spki_der());
    assert_eq!(parsed.validity().not_before.timestamp(), NOW as i64);
    assert_eq!(
        parsed.validity().not_after.timestamp(),
        (NOW + SERVICE_VALIDITY_SECS) as i64
    );
    let host = test
        .ca
        .issue_host(&host_id("web-1"), &key.p256(), 60, 10)
        .expect("a host certificate");
    let authority = |der: &[u8]| {
        parse(der)
            .extensions()
            .iter()
            .find_map(|ext| match ext.parsed_extension() {
                ParsedExtension::AuthorityKeyIdentifier(id) => Some(format!("{id:?}")),
                _ => None,
            })
            .expect("an authority key identifier")
    };
    assert_eq!(authority(&cert.der), authority(&host.der));
}

/// Proves: the 8 hours are also the cap, a validity of 0 is refused, and so are serial 0 and the
/// CA certificate's own, for a service certificate signed through the [`Signer`] seam.
///
/// Catches: a service certificate that can be asked for longer than 8 hours, or signed with a
/// serial a certificate of this CA may not carry.
#[test]
fn a_service_certificate_is_capped_at_eight_hours_and_takes_a_fresh_serial() {
    let test = TestCa::new("service-cap");
    let key = HostKey::new().p256();
    let name = service("js-agent");
    let host = host_id("web-1");
    let sign = |validity_secs, serial| {
        test.ca.sign(&SignRequest {
            subject: Subject::Service {
                service: &name,
                host: &host,
            },
            key: &key,
            validity_secs,
            serial,
        })
    };
    assert_eq!(
        sign(SERVICE_VALIDITY_SECS, 5)
            .expect("at the cap")
            .not_after,
        NOW + SERVICE_VALIDITY_SECS
    );
    assert_eq!(sign(1, 5).expect("a short one").not_after, NOW + 1);
    assert_eq!(
        sign(SERVICE_VALIDITY_SECS + 1, 5),
        Err(SignError::ValidityOverCap {
            requested: SERVICE_VALIDITY_SECS + 1,
            cap: SERVICE_VALIDITY_SECS
        })
    );
    assert_eq!(sign(0, 5), Err(SignError::EmptyValidity));
    assert_eq!(sign(60, 0), Err(SignError::BadSerial(0)));
    assert_eq!(sign(60, 1), Err(SignError::BadSerial(1)));
}

/// Proves: what the CA issues is read back as the service, the host, the key's hash and the
/// window of unix seconds, for a service name with every character of the grammar.
///
/// Catches: a reader that drops a character a service name may hold (`.`, `_`, upper case) or
/// reads another host or key than the certificate's.
#[test]
fn a_service_certificate_is_read_back_as_its_service_host_key_and_window() {
    let test = TestCa::new("service-read");
    for name in ["js-agent", "Js.Agent_1", "a"] {
        let key = HostKey::new();
        let cert = test
            .ca
            .issue_service(&service(name), &host_id("web-1"), &key.p256(), 7)
            .expect("issued");
        assert_eq!(
            test.ca.bundle().service_certificate(&cert.der),
            Ok(ServiceCertificate {
                service: service(name),
                host_id: host_id("web-1"),
                key_hash: key.key_hash(),
                not_before_unix: NOW,
                not_after_unix: NOW + SERVICE_VALIDITY_SECS,
            }),
            "{name}"
        );
    }
}

/// Proves: a host certificate is not read as a service certificate, a service certificate is not
/// read as a host certificate, and neither is the coordinator's server certificate read as
/// either.
///
/// Catches: one reader taking a certificate of the other kind, which would let a service
/// certificate authenticate as a host on `Heartbeat`, `Renew` and the consoles.
#[test]
fn a_service_certificate_is_no_host_certificate_and_a_host_certificate_no_service_certificate() {
    let test = TestCa::new("service-vs-host");
    let key = HostKey::new();
    let service_cert = test
        .ca
        .issue_service(&service("js-agent"), &host_id("web-1"), &key.p256(), 7)
        .expect("a service certificate");
    let host_cert = test
        .ca
        .issue_host(&host_id("web-1"), &key.p256(), 60, 8)
        .expect("a host certificate");
    let server_cert = test
        .ca
        .issue_server(
            &key.p256(),
            ServerNames {
                dns: &["coordinator.example".to_owned()],
                ips: &[],
            },
            60,
            9,
        )
        .expect("a server certificate");
    let bundle = test.ca.bundle();
    assert_eq!(bundle.host_certificate(&service_cert.der), None);
    assert_eq!(
        bundle.service_certificate(&host_cert.der),
        Err(ServiceCertificateError::Subject)
    );
    assert_eq!(
        bundle.service_certificate(&server_cert.der),
        Err(ServiceCertificateError::Subject)
    );
    assert!(bundle.host_certificate(&host_cert.der).is_some());
    assert!(bundle.service_certificate(&service_cert.der).is_ok());
}

/// Proves: bytes that are not one DER certificate, a certificate with a byte after it, one signed
/// by another CA, and any certificate read by a bundle whose CA certificate has no P-256 key, are
/// refused as not signed by this CA, or not DER.
///
/// Catches: a reader that trusts a certificate for its contents and not its signature.
#[test]
fn a_certificate_not_signed_by_this_ca_is_refused() {
    let test = TestCa::new("service-signature");
    let key = HostKey::new();
    let cert = test
        .ca
        .issue_service(&service("js-agent"), &host_id("web-1"), &key.p256(), 7)
        .expect("issued");
    let bundle = test.ca.bundle();
    assert_eq!(
        bundle.service_certificate(b"not a certificate"),
        Err(ServiceCertificateError::NotDer)
    );
    let mut trailing = cert.der.clone();
    trailing.push(0);
    assert_eq!(
        bundle.service_certificate(&trailing),
        Err(ServiceCertificateError::NotDer)
    );
    let other = TestCa::new("service-other-ca");
    let by_other = other
        .ca
        .issue_service(&service("js-agent"), &host_id("web-1"), &key.p256(), 7)
        .expect("issued by another CA");
    assert_eq!(
        bundle.service_certificate(&by_other.der),
        Err(ServiceCertificateError::NotSignedByThisCa)
    );
    let mut tampered = cert.der.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(
        bundle.service_certificate(&tampered),
        Err(ServiceCertificateError::NotSignedByThisCa)
    );
    let keyless = CaBundle::of(b"not a CA certificate".to_vec());
    assert_eq!(
        keyless.service_certificate(&cert.der),
        Err(ServiceCertificateError::NotSignedByThisCa)
    );
    assert_eq!(
        keyless.service_certificate(b"x"),
        Err(ServiceCertificateError::NotDer)
    );
}

/// Proves: the certificate made here by hand reads, so each refusal below is of one rule alone.
#[test]
fn the_certificate_the_refusal_tests_start_from_is_accepted() {
    let test = TestCa::new("service-baseline");
    let key = HostKey::new();
    let der = forge(&test, &key.pair, good());
    let read = test
        .ca
        .bundle()
        .service_certificate(&der)
        .expect("the baseline reads");
    assert_eq!(
        (read.service, read.host_id, read.key_hash),
        (service("js-agent"), host_id("web-1"), key.key_hash())
    );
}

/// Proves: a subject that is not exactly one common name `service:<name>` is refused: two
/// attributes, one that is no common name, a common name without the prefix, and a name that is
/// not a service name.
///
/// Catches: a service named by a certificate whose subject says something else too, or a
/// certificate of another kind whose name begins another way.
#[test]
fn a_subject_that_is_not_one_service_name_is_refused() {
    let test = TestCa::new("service-subject");
    let mut two = good();
    two.distinguished_name
        .push(DnType::OrganizationName, "other");
    refused(&test, two, ServiceCertificateError::Subject);

    let mut not_cn = good();
    not_cn.distinguished_name = rcgen::DistinguishedName::new();
    not_cn
        .distinguished_name
        .push(DnType::OrganizationName, "service:js-agent");
    refused(&test, not_cn, ServiceCertificateError::Subject);

    for common_name in ["js-agent", "host:web-1", "service-js-agent"] {
        let mut params = good();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        refused(&test, params, ServiceCertificateError::Subject);
    }
    let mut empty = good();
    empty.distinguished_name = rcgen::DistinguishedName::new();
    refused(&test, empty, ServiceCertificateError::Subject);

    for bad in ["", "-js", ".hidden", "js agent"] {
        let mut params = good();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, format!("service:{bad}"));
        let host = HostKey::new();
        let der = forge(&test, &host.pair, params);
        let defect = test
            .ca
            .bundle()
            .service_certificate(&der)
            .expect_err("refused");
        assert!(
            matches!(defect, ServiceCertificateError::ServiceName(_)),
            "{bad:?}: {defect:?}"
        );
    }
    assert_eq!(
        ServiceCertificateError::ServiceName(
            ServiceName::parse("-js").expect_err("not a service name")
        )
        .to_string(),
        "the service in the subject is not a service name: service name \"-js\" does not match its grammar (1 to 255 of A-Z, a-z, 0-9, ., _ and -, not beginning with - or .)"
    );
}

/// Proves: the alternative names must be the service's URI once and one DNS name that is a host
/// id, and nothing else: none, a URI of another service, no URI, the URI twice, no DNS name, two,
/// a DNS name that is no host id, an IP address, or an e-mail name each refuse.
///
/// Catches: a certificate that names another service or host than its subject, or names more
/// than one host, being read as one service on one host.
#[test]
fn alternative_names_other_than_the_services_uri_and_one_host_are_refused() {
    let test = TestCa::new("service-names");
    let own = || uri("urn:hostkeeper:service:js-agent");
    let cases: [(&str, Vec<SanType>); 9] = [
        ("none", vec![]),
        (
            "another service",
            vec![uri("urn:hostkeeper:service:other"), dns("web-1")],
        ),
        ("no URI", vec![dns("web-1")]),
        ("two URIs", vec![own(), own(), dns("web-1")]),
        ("no DNS name", vec![own()]),
        ("two DNS names", vec![own(), dns("web-1"), dns("web-2")]),
        (
            "a DNS name that is no host id",
            vec![own(), dns("Web_1.example")],
        ),
        (
            "an IP address",
            vec![
                own(),
                dns("web-1"),
                SanType::IpAddress("192.0.2.1".parse().expect("an IP")),
            ],
        ),
        (
            "an e-mail name",
            vec![
                own(),
                dns("web-1"),
                SanType::Rfc822Name(Ia5String::try_from("a@example.com").expect("ASCII")),
            ],
        ),
    ];
    for (what, names) in cases {
        let mut params = good();
        params.subject_alt_names = names;
        let host = HostKey::new();
        let der = forge(&test, &host.pair, params);
        assert_eq!(
            test.ca.bundle().service_certificate(&der),
            Err(ServiceCertificateError::AltNames),
            "{what}"
        );
    }
}

/// Proves: the extended key usage must be `clientAuth` alone: absent, `serverAuth`, both, and
/// `clientAuth` with `any`, code signing or another purpose are refused.
///
/// Catches: a service certificate that could also serve as a server's, or whose usage is not
/// stated, which a relying party would take as any.
#[test]
fn an_extended_key_usage_other_than_client_auth_alone_is_refused() {
    let test = TestCa::new("service-eku");
    let cases: [(&str, Vec<ExtendedKeyUsagePurpose>); 7] = [
        ("absent", vec![]),
        ("server only", vec![ExtendedKeyUsagePurpose::ServerAuth]),
        (
            "both",
            vec![
                ExtendedKeyUsagePurpose::ClientAuth,
                ExtendedKeyUsagePurpose::ServerAuth,
            ],
        ),
        (
            "with any",
            vec![
                ExtendedKeyUsagePurpose::ClientAuth,
                ExtendedKeyUsagePurpose::Any,
            ],
        ),
        (
            "with code signing",
            vec![
                ExtendedKeyUsagePurpose::ClientAuth,
                ExtendedKeyUsagePurpose::CodeSigning,
            ],
        ),
        (
            "with e-mail protection",
            vec![
                ExtendedKeyUsagePurpose::ClientAuth,
                ExtendedKeyUsagePurpose::EmailProtection,
            ],
        ),
        (
            "with another purpose",
            vec![
                ExtendedKeyUsagePurpose::ClientAuth,
                ExtendedKeyUsagePurpose::Other(vec![1, 3, 6, 1, 4, 1, 99999, 1]),
            ],
        ),
    ];
    for (what, usages) in cases {
        let mut params = good();
        params.extended_key_usages = usages;
        let host = HostKey::new();
        let der = forge(&test, &host.pair, params);
        assert_eq!(
            test.ca.bundle().service_certificate(&der),
            Err(ServiceCertificateError::ExtendedKeyUsage),
            "{what}"
        );
    }
    for other in [
        ExtendedKeyUsagePurpose::TimeStamping,
        ExtendedKeyUsagePurpose::OcspSigning,
    ] {
        let mut params = good();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth, other];
        refused(&test, params, ServiceCertificateError::ExtendedKeyUsage);
    }
}

/// Proves: a certificate whose basic constraints say CA, constrained or not, is refused; one
/// with `CA:FALSE` or with no basic constraints reads.
///
/// Catches: a certificate that could sign others being read as a service's.
#[test]
fn a_certificate_that_is_a_ca_is_refused() {
    let test = TestCa::new("service-ca");
    for is_ca in [
        IsCa::Ca(BasicConstraints::Unconstrained),
        IsCa::Ca(BasicConstraints::Constrained(0)),
    ] {
        let mut params = good();
        params.is_ca = is_ca;
        refused(&test, params, ServiceCertificateError::IsCa);
    }
    let mut no_constraints = good();
    no_constraints.is_ca = IsCa::NoCa;
    let key = HostKey::new();
    let der = forge(&test, &key.pair, no_constraints);
    assert!(test.ca.bundle().service_certificate(&der).is_ok());
}

/// Proves: a certificate for a key that is not an uncompressed P-256 key is refused.
///
/// Catches: a service certificate whose key the host-side check and the CA's profile do not
/// hold.
#[test]
fn a_certificate_for_a_key_that_is_not_p256_is_refused() {
    let test = TestCa::new("service-key");
    let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).expect("P-384");
    let der = forge(&test, &p384, good());
    assert_eq!(
        test.ca.bundle().service_certificate(&der),
        Err(ServiceCertificateError::Key)
    );
}

/// Proves: a certificate that starts before the epoch is refused as having no window of unix
/// seconds, and the refusals read as the sentence each names.
///
/// Catches: a window that wraps to a huge `notBefore`, which a half-life rule would read as
/// valid far into the future.
#[test]
fn a_validity_before_the_epoch_is_refused() {
    let test = TestCa::new("service-validity");
    let mut params = good();
    params.not_before = rcgen::date_time_ymd(1960, 1, 1);
    params.not_after = rcgen::date_time_ymd(2100, 1, 1);
    refused(&test, params, ServiceCertificateError::Validity);
    for (defect, text) in [
        (
            ServiceCertificateError::NotDer,
            "the certificate is not one DER certificate",
        ),
        (
            ServiceCertificateError::NotSignedByThisCa,
            "the certificate is not signed by this CA",
        ),
        (
            ServiceCertificateError::Subject,
            "the subject is not one common name service:<name>",
        ),
        (
            ServiceCertificateError::AltNames,
            "the subjectAltName is not exactly the URI urn:hostkeeper:service:<name> and one DNS host id",
        ),
        (
            ServiceCertificateError::ExtendedKeyUsage,
            "the extended key usage is not clientAuth alone",
        ),
        (ServiceCertificateError::IsCa, "the certificate is a CA"),
        (
            ServiceCertificateError::Key,
            "the key is not an uncompressed ECDSA P-256 key",
        ),
        (
            ServiceCertificateError::Validity,
            "the validity is not a window of unix seconds",
        ),
    ] {
        assert_eq!(defect.to_string(), text);
    }
}

/// Proves: the refusal of a service URI the certificate cannot carry names the URI, as a log line
/// reads it.
#[test]
fn the_service_uri_error_names_the_uri() {
    assert_eq!(
        SignError::BadServiceUri("urn:hostkeeper:service:x".to_owned()).to_string(),
        "the service URI \"urn:hostkeeper:service:x\" is not one a certificate carries"
    );
}

/// Proves: a CA opened again from its files signs service certificates a bundle of the first
/// reads, and the host id of a certificate that names a host with a dot-free label reads back.
#[test]
fn a_reopened_ca_signs_what_its_bundle_reads() {
    let test = TestCa::new("service-reopen");
    let reopened = Ca::open(
        &hostkeeper_enrollment::fs::RealFs,
        files(test.dir.path()),
        support::FixedClock::at(NOW),
    )
    .expect("open the CA");
    let key = HostKey::new();
    let host: HostId = host_id("a");
    let cert = reopened
        .issue_service(&service("js-agent"), &host, &key.p256(), 3)
        .expect("issued");
    assert_eq!(
        test.ca
            .bundle()
            .service_certificate(&cert.der)
            .map(|read| read.host_id),
        Ok(host)
    );
}
