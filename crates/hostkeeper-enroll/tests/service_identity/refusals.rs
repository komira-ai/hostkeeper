//! Each check a certificate must pass before it is installed, and the coordinator's refusals: in
//! every case nothing is installed (the key stays, the certificate and the CA file are not made),
//! the reason is the service's reported error, word for word, and what is installed stays.

use std::sync::Arc;

use hostkeeper_enroll::{HostId, IdentityFile, IssueAnswer, Trust};
use x509_cert::der::asn1::Ia5String;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::spki::SubjectPublicKeyInfoOwned;

use crate::support::{CLIENT_AUTH, Ca, HOST, HOUR, Leaf, SERVER_AUTH, SERVICE, T0, World, wanted};

/// A world whose coordinator answers with `leaf` signed by `signer`, certifying the request's
/// key unless the leaf says another.
fn answering(name: &str, leaf: Leaf, signer: Ca) -> World {
    let world = World::new(name);
    let ca = world.ca.pem.clone();
    world.issuer.answer_with(move |_, csr| {
        Ok(IssueAnswer::Issued {
            cert_der: leaf.sign(&signer, csr),
            ca_bundle_pem: ca.clone(),
        })
    });
    world
}

fn good() -> Leaf {
    Leaf::good(SERVICE, HOST, T0)
}

/// One pass of a world that must end refused with `message`, nothing installed.
fn refused(mut world: World, message: &str) {
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.last_error(SERVICE).as_deref(), Some(message));
    assert_eq!(world.issuer.calls().len(), 1);
    assert!(
        world.read(SERVICE, IdentityFile::Key).is_some(),
        "the key is kept"
    );
    assert_eq!(
        world.read(SERVICE, IdentityFile::Cert),
        None,
        "no certificate"
    );
    assert_eq!(world.read(SERVICE, IdentityFile::Ca), None, "no CA file");
    assert_eq!(world.report(SERVICE).0, None);
}

/// Catches a certificate that is not DER being installed.
#[test]
fn a_certificate_that_is_not_der_is_refused() {
    let world = World::new("not-der");
    world.issuer.answer_with(|_, _| {
        Ok(IssueAnswer::Issued {
            cert_der: b"this is not a certificate".to_vec(),
            ca_bundle_pem: String::new(),
        })
    });
    refused(
        world,
        "the certificate is neither DER nor one PEM certificate",
    );
}

/// Catches a PEM certificate whose body is not a certificate being installed, and a PEM block
/// that is cut short.
#[test]
fn a_certificate_that_is_malformed_pem_is_refused() {
    for body in [
        "-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n",
        "-----BEGIN CERTIFICATE-----\nZm9v\n",
    ] {
        let world = World::new("not-pem");
        world.issuer.answer_with(move |_, _| {
            Ok(IssueAnswer::Issued {
                cert_der: body.as_bytes().to_vec(),
                ca_bundle_pem: String::new(),
            })
        });
        refused(
            world,
            "the certificate is neither DER nor one PEM certificate",
        );
    }
}

/// Catches a certificate whose DER is cut short or has bytes after it being installed.
#[test]
fn a_certificate_with_malformed_der_is_refused() {
    let world = World::new("bad-der");
    let ca = Arc::clone(&world.ca);
    world.issuer.answer_with(move |_, csr| {
        let mut der = good().sign(&ca, csr);
        der.extend_from_slice(b"trailing");
        Ok(IssueAnswer::Issued {
            cert_der: der,
            ca_bundle_pem: ca.pem.clone(),
        })
    });
    refused(
        world,
        "the certificate is neither DER nor one PEM certificate",
    );
}

/// Catches a good certificate being installed with a CA bundle that is not PEM certificates.
#[test]
fn a_ca_bundle_that_is_not_pem_is_refused() {
    let world = World::new("bad-bundle");
    let ca = Arc::clone(&world.ca);
    world.issuer.answer_with(move |_, csr| {
        Ok(IssueAnswer::Issued {
            cert_der: good().sign(&ca, csr),
            ca_bundle_pem: "-----BEGIN CERTIFICATE-----\n!!\n".to_owned(),
        })
    });
    refused(
        world,
        "the CA bundle that came with the certificate is not PEM certificates",
    );
}

/// Catches a host whose own CA file is damaged accepting any certificate.
#[test]
fn a_trusted_bundle_that_is_not_pem_refuses_every_certificate() {
    let world = World::new("bad-trust");
    world.host.set(Ok(Trust {
        host_id: HostId::new(HOST).expect("a host id"),
        ca_bundle_pem: "-----BEGIN CERTIFICATE-----\n!!\n".to_owned(),
    }));
    refused(world, "the host's own CA bundle is not PEM certificates");
}

/// Catches a certificate valid only later (not before the clock) being installed.
#[test]
fn a_certificate_valid_only_in_the_future_is_refused() {
    let mut leaf = good();
    leaf.window = (T0 + 1, T0 + 8 * HOUR);
    let world = answering("future", leaf, Ca::new(1));
    refused(
        world,
        &format!(
            "the certificate is not valid now: it is valid from {} until {}",
            T0 + 1,
            T0 + 8 * HOUR
        ),
    );
}

/// Catches a certificate whose end has passed being installed, at the very second it ends and
/// after it.
#[test]
fn a_certificate_whose_end_has_passed_is_refused() {
    for end in [T0, T0 - 1] {
        let mut leaf = good();
        leaf.window = (T0 - 8 * HOUR, end);
        let world = answering("past", leaf, Ca::new(1));
        refused(
            world,
            &format!(
                "the certificate is not valid now: it is valid from {} until {end}",
                T0 - 8 * HOUR
            ),
        );
    }
}

/// Catches a certificate good for server authentication too, or for it alone, or for no
/// extended use at all, being installed.
#[test]
fn a_certificate_that_is_not_for_client_authentication_alone_is_refused() {
    let cases = [
        Some(vec![CLIENT_AUTH, SERVER_AUTH]),
        Some(vec![SERVER_AUTH]),
        Some(Vec::new()),
        None,
    ];
    let mut garbled = good();
    garbled.garbled_eku = true;
    refused(
        answering("eku-garbled", garbled, Ca::new(1)),
        "the certificate's extended key usage is not client authentication alone",
    );
    for ekus in cases {
        let mut leaf = good();
        leaf.ekus = ekus;
        let world = answering("eku", leaf, Ca::new(1));
        refused(
            world,
            "the certificate's extended key usage is not client authentication alone",
        );
    }
}

/// Catches a certificate for another service being installed: its URI names another service, its
/// subject names another, or it names one more thing than the service and the host.
#[test]
fn a_certificate_that_names_another_service_is_refused() {
    let uri = "urn:hostkeeper:service:js-agent";
    let mut other_uri = good();
    other_uri.uri = "urn:hostkeeper:service:other".to_owned();
    let mut other_dns_kind = good();
    other_dns_kind.extra_names = vec![GeneralName::UniformResourceIdentifier(
        Ia5String::new("urn:hostkeeper:service:other").expect("a URI"),
    )];
    let mut extra = good();
    extra.extra_names = vec![GeneralName::DnsName(
        Ia5String::new("other").expect("a name"),
    )];
    for leaf in [other_uri, other_dns_kind, extra] {
        let world = answering("service-san", leaf, Ca::new(1));
        refused(
            world,
            &format!("the certificate's subject alternative name is not exactly {uri} and {HOST}"),
        );
    }
}

/// Catches a certificate for another host being installed.
#[test]
fn a_certificate_for_another_host_is_refused() {
    let mut leaf = good();
    leaf.dns = "db-7".to_owned();
    let world = answering("other-host", leaf, Ca::new(1));
    refused(
        world,
        &format!(
            "the certificate's subject alternative name is not exactly urn:hostkeeper:service:{SERVICE} and {HOST}"
        ),
    );
}

/// Catches a certificate whose subject is a host's (`host:<id>`) or another service's being
/// installed, so that no reader of host certificates takes it for a host's.
#[test]
fn a_certificate_whose_subject_is_not_the_services_is_refused() {
    for subject in [
        "CN=host:web-1",
        "CN=service:other",
        "CN=service:js-agent,O=extra",
    ] {
        let mut leaf = good();
        leaf.subject = subject.to_owned();
        let world = answering("subject", leaf, Ca::new(1));
        refused(
            world,
            "the certificate's subject is not exactly service:js-agent",
        );
    }
}

/// Catches a certificate for another key than the one the service holds being installed.
#[test]
fn a_certificate_for_another_key_is_refused() {
    let other_key = p256::ecdsa::SigningKey::from_slice(&[9; 32]).expect("a key");
    let other = SubjectPublicKeyInfoOwned::from_key(other_key.verifying_key()).expect("SPKI");
    let mut leaf = good();
    leaf.other_key = Some(other);
    let world = answering("other-key", leaf, Ca::new(1));
    refused(
        world,
        "the certificate is for another key than the one this host holds for the service",
    );
}

/// Catches a certificate signed by a CA the host does not trust being installed, though its
/// bundle says otherwise.
#[test]
fn a_certificate_not_signed_by_the_trusted_ca_is_refused() {
    let world = answering("untrusted", good(), Ca::new(2));
    refused(
        world,
        "the certificate is not signed by a CA this host trusts",
    );
}

/// Catches a certificate whose signature algorithm is not ECDSA with SHA-256 being accepted: the
/// algorithm identifier of a good certificate is changed to ECDSA with SHA-384 in both its
/// places.
#[test]
fn a_certificate_with_another_signature_algorithm_is_refused() {
    const SHA256: [u8; 10] = [6, 8, 0x2a, 0x86, 0x48, 0xce, 0x3d, 4, 3, 2];
    let world = World::new("algorithm");
    let ca = Arc::clone(&world.ca);
    world.issuer.answer_with(move |_, csr| {
        let mut der = good().sign(&ca, csr);
        let mut replaced = 0;
        for at in 0..der.len() - SHA256.len() {
            if der[at..at + SHA256.len()] == SHA256 {
                der[at + SHA256.len() - 1] = 3;
                replaced += 1;
            }
        }
        assert_eq!(replaced, 2, "the algorithm is named twice");
        Ok(IssueAnswer::Issued {
            cert_der: der,
            ca_bundle_pem: ca.pem.clone(),
        })
    });
    refused(
        world,
        "the certificate's signature algorithm is not ECDSA with SHA-256 in both its fields",
    );
}

/// Catches a CA certificate, a certificate with a critical extension this daemon does not read,
/// and one that does not allow digital signatures being installed.
#[test]
fn a_certificate_that_is_not_an_end_entity_for_signatures_is_refused() {
    let mut ca = good();
    ca.is_ca = true;
    let mut critical = good();
    critical.critical_unknown = true;
    let mut no_signature = good();
    no_signature.digital_signature = false;
    for (leaf, message) in [
        (
            ca,
            "the certificate is a CA certificate, or an extension of it is not one this daemon accepts: the certificate is a CA certificate",
        ),
        (
            critical,
            "the certificate is a CA certificate, or an extension of it is not one this daemon accepts: the certificate marks the extension 1.3.6.1.4.1.99999.1 critical, which this daemon does not check",
        ),
        (
            no_signature,
            "the certificate is a CA certificate, or an extension of it is not one this daemon accepts: the certificate's key usage does not allow digital signatures",
        ),
    ] {
        refused(answering("profile", leaf, Ca::new(1)), message);
    }
}

/// Catches an older renewal arriving after a newer certificate was installed replacing it: the
/// installed certificate stays, byte for byte, for a certificate that starts before it and for one
/// that starts the same second.
#[test]
fn a_certificate_that_is_not_newer_than_the_installed_one_is_refused() {
    for earlier in [T0 - 100, T0] {
        let mut world = World::new("not-newer");
        world.keeper.step(&[wanted(SERVICE)]);
        let installed = world.read(SERVICE, IdentityFile::Cert).expect("cert.pem");
        let ca = Arc::clone(&world.ca);
        world.issuer.answer_with(move |_, csr| {
            let mut leaf = good();
            leaf.window = (earlier, T0 + 12 * HOUR);
            Ok(IssueAnswer::Issued {
                cert_der: leaf.sign(&ca, csr),
                ca_bundle_pem: ca.pem.clone(),
            })
        });
        world.set_wall(T0 + 4 * HOUR);
        world.keeper.step(&[wanted(SERVICE)]);
        assert_eq!(
            world.last_error(SERVICE),
            Some(format!(
                "the certificate is not newer than the installed one: it is valid from {earlier}, the installed one from {T0}"
            ))
        );
        assert_eq!(world.read(SERVICE, IdentityFile::Cert), Some(installed));
        assert_eq!(
            world.report(SERVICE).0,
            Some(T0 + 8 * HOUR),
            "the report still names the installed certificate's end"
        );
    }
}

/// Catches the coordinator's refusals, and the answer of a coordinator without the call, changing
/// what is installed, and a refusal's token being reworded: the report is the token.
#[test]
fn a_refusal_keeps_what_is_installed_and_is_reported_as_its_token() {
    let cases = [
        (IssueAnswer::Unimplemented, "unimplemented"),
        (
            IssueAnswer::Refused(
                hostkeeper_enroll::Reason::new("service_not_assigned").expect("a token"),
            ),
            "service_not_assigned",
        ),
    ];
    for (answer, token) in cases {
        let mut world = World::new("refused");
        world.keeper.step(&[wanted(SERVICE)]);
        let installed = world.read(SERVICE, IdentityFile::Cert);
        world.issuer.answer_with(move |_, _| Ok(answer.clone()));
        world.set_wall(T0 + 5 * HOUR);
        world.keeper.step(&[wanted(SERVICE)]);
        assert_eq!(
            world.report(SERVICE),
            (Some(T0 + 8 * HOUR), Some(token.to_owned()))
        );
        assert_eq!(world.read(SERVICE, IdentityFile::Cert), installed);
    }
}
