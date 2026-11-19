//! A certificate in the pki directory that is not what the coordinator's CA issues now: renewed
//! with the host's key at the first step of the daemon, not at the middle of its validity, when
//! it can authenticate a renewal; returned with `renewal_of` and the same key when it cannot (it
//! is of another CA, or not for client authentication). A coordinator that answers the same
//! again is asked once per generation.

use std::fs;
use std::os::unix::fs::symlink;

use hostkeeper_enroll::{Enroller, Enrollment, Error, HostId, Outcome, Reason, RenewalState};
use p256::ecdsa::SigningKey;
use x509_cert::Certificate;
use x509_cert::der::asn1::OctetString;
use x509_cert::der::oid::AssociatedOid;
use x509_cert::der::oid::db::rfc5280::{ID_KP_CLIENT_AUTH, ID_KP_SERVER_AUTH};
use x509_cert::der::{DecodePem, Encode};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::ext::pkix::name::GeneralName;

use crate::support::{
    Args, Ca, HOST, HOST_ROLE_URI, T0, WEEK, World, critical, csr_spki, eku, host_san, pkcs8, san,
    signing_key, spki_of,
};

/// The extensions of a host certificate before the CA named the host by a DNS name: the
/// host-role URI alone, and client authentication.
fn older_profile() -> Vec<Extension> {
    vec![san(&[HOST_ROLE_URI], &[]), eku(&[ID_KP_CLIENT_AUTH])]
}

/// A test case: its name, and the extensions of the certificate the host held.
type Case = (&'static str, fn() -> Vec<Extension>);

/// A coordinator's CA that issues `extensions` instead of its profile's, for the request's key.
fn issuing(world: &World, extensions: impl Fn() -> Vec<Extension> + Send + 'static) {
    world.coordinator().issue = Some(Box::new(move |ca: &Ca, args: &Args| {
        hostkeeper_enroll::Issued {
            host_id: args.host_id.clone(),
            cert_pem: ca.leaf(
                &args.spki,
                &format!("CN=host:{}", args.host_id),
                extensions(),
                (args.now, args.now + WEEK),
            ),
            ca_bundle_pem: ca.pem.clone(),
        }
    }));
}

/// The host enrolled by a coordinator that issued `extensions`, and the daemon started again
/// (a new enroller on the same pki directory) against one that issues its profile, one second
/// later.
fn restarted_after(
    name: &str,
    extensions: impl Fn() -> Vec<Extension> + Send + 'static,
) -> (World, Enroller) {
    let world = World::new(name);
    issuing(&world, extensions);
    drop(world.enrolled());
    world.coordinator().issue = None;
    world.set_now(T0 + 1);
    let enroller = world.open();
    (world, enroller)
}

/// The pki directory of a host enrolled by hand: generation 1 holds `cert_pem`, the bundle
/// `ca_pem` and `key`. The coordinator holds no request of that key yet, so a return waits for
/// an operator.
fn held_by_hand(world: &World, key: &SigningKey, cert_pem: &str, ca_pem: &str) {
    let pki = world.pki();
    let gen1 = pki.join("gen-1");
    fs::create_dir_all(&gen1).expect("gen-1");
    fs::write(pki.join("SCHEMA"), "1\n").expect("SCHEMA");
    fs::write(gen1.join("cert.pem"), cert_pem).expect("cert");
    fs::write(gen1.join("ca.pem"), ca_pem).expect("ca");
    crate::support::write_private(&gen1.join("key"), &pkcs8(key));
    fs::write(gen1.join("COMPLETE"), "").expect("COMPLETE");
    symlink("gen-1", pki.join("current")).expect("current");
}

/// A certificate of the host for `key`, signed by `ca`, valid for a week from `T0`.
fn host_leaf(ca: &Ca, key: &SigningKey, extensions: Vec<Extension>) -> String {
    ca.leaf(
        &spki_of(key),
        &format!("CN=host:{HOST}"),
        extensions,
        (T0, T0 + WEEK),
    )
}

/// The DNS names of the certificate of generation `generation`.
fn dns_names(world: &World, generation: u64) -> Vec<String> {
    let path = world.pki().join(format!("gen-{generation}/cert.pem"));
    let pem = fs::read_to_string(path).expect("the certificate");
    let cert = Certificate::from_pem(pem.as_bytes()).expect("PEM");
    let names = cert
        .tbs_certificate()
        .get_extension::<SubjectAltName>()
        .expect("one subject alternative name")
        .map_or_else(Vec::new, |(_, names)| names.0);
    names
        .into_iter()
        .filter_map(|name| match name {
            GeneralName::DnsName(dns) => Some(dns.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// The key of generation `generation`, PKCS#8 DER.
fn key_of(world: &World, generation: u64) -> Vec<u8> {
    fs::read(world.pki().join(format!("gen-{generation}/key"))).expect("the key")
}

/// The generation `outcome` renewed to.
fn renewed(outcome: Result<Outcome, Error>) -> u64 {
    match outcome {
        Ok(Outcome::Renewed(identity)) => identity.generation,
        other => panic!("not renewed: {other:?}"),
    }
}

/// Catches a certificate issued before the CA named the host by a DNS name kept until the
/// middle of its validity: the first step of the daemon started again, one second into the
/// certificate's week and before any sleep, renews it signed by the key the host holds; the new
/// generation names the host id as its DNS name and keeps the key; the step after it renews
/// nothing.
#[test]
fn a_certificate_without_the_host_dns_name_is_renewed_at_start() {
    let (world, mut enroller) = restarted_after("reissue-no-dns", older_profile);
    let mut lines = Vec::new();
    let mut steps = 0;
    enroller.run_loop(
        &mut |line: &str| lines.push(line.to_owned()),
        &mut |_, _| {},
        &RenewalState::default(),
        &mut || {
            steps += 1;
            steps == 1
        },
    );
    assert_eq!(
        lines,
        [format!(
            "renewed the certificate of host:{HOST} (generation 2)"
        )]
    );
    assert_eq!(world.sleeps().len(), 1, "one step, then its sleep");
    let renews = world.coordinator().renews.clone();
    assert_eq!(renews.len(), 1);
    assert_eq!(renews[0].host_id.as_str(), HOST);
    assert_eq!(
        csr_spki(&renews[0].csr_der),
        spki_of_der(&key_of(&world, 1))
    );
    assert_eq!(dns_names(&world, 1), Vec::<String>::new());
    assert_eq!(dns_names(&world, 2), [HOST]);
    assert_eq!(key_of(&world, 2), key_of(&world, 1), "the key is kept");

    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 1);
}

/// The DER SubjectPublicKeyInfo of the key in the PKCS#8 file `der`.
fn spki_of_der(der: &[u8]) -> Vec<u8> {
    use p256::pkcs8::{DecodePrivateKey, EncodePublicKey};
    SigningKey::from_pkcs8_der(der)
        .expect("a PKCS#8 P-256 key")
        .verifying_key()
        .to_public_key_der()
        .expect("SPKI")
        .as_bytes()
        .to_vec()
}

/// A subject alternative name extension whose value is not a `SubjectAltName`.
fn undecodable_san() -> Extension {
    Extension {
        extn_id: SubjectAltName::OID,
        critical: false,
        extn_value: OctetString::new(vec![0x05, 0x00]).expect("an octet string"),
    }
}

/// Catches DNS names compared loosely: a certificate with no subject alternative name, one
/// whose DNS name is another host's, one that adds a second DNS name to the host id, and one
/// whose subject alternative name does not decode are each renewed at the first step, and the
/// new generation names the host id alone.
#[test]
fn dns_names_other_than_the_host_id_alone_are_renewed_at_start() {
    let cases: [Case; 4] = [
        ("reissue-no-san", || vec![eku(&[ID_KP_CLIENT_AUTH])]),
        ("reissue-other-dns", || {
            vec![san(&[HOST_ROLE_URI], &["web-2"]), eku(&[ID_KP_CLIENT_AUTH])]
        }),
        ("reissue-extra-dns", || {
            vec![
                san(&[HOST_ROLE_URI], &[HOST, "web-1.example"]),
                eku(&[ID_KP_CLIENT_AUTH]),
            ]
        }),
        ("reissue-bad-san", || {
            vec![undecodable_san(), eku(&[ID_KP_CLIENT_AUTH])]
        }),
    ];
    for (name, extensions) in cases {
        let (world, mut enroller) = restarted_after(name, extensions);
        assert_eq!(renewed(enroller.run_once()), 2, "{name}");
        assert_eq!(dns_names(&world, 2), [HOST], "{name}");
        assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle, "{name}");
        assert_eq!(world.coordinator().renews.len(), 1, "{name}");
    }
}

/// Catches an extended key usage checked only for client authentication: a certificate that
/// also allows server authentication is renewed at the first step, with the host's key.
#[test]
fn an_extended_key_usage_beyond_client_authentication_is_renewed_at_start() {
    let (world, mut enroller) = restarted_after("reissue-eku", || {
        vec![host_san(HOST), eku(&[ID_KP_CLIENT_AUTH, ID_KP_SERVER_AUTH])]
    });
    assert_eq!(renewed(enroller.run_once()), 2);
    assert_eq!(key_of(&world, 2), key_of(&world, 1));
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 1);
}

/// Catches a certificate renewed early although it is as the CA issues it, and a host-role
/// URI taken for part of the profile (its prefix is the coordinator's setting): after a
/// restart, a certificate of the profile, and one with the DNS name and no URI, are not renewed
/// at the first step nor one second before the middle of their validity, and are renewed at
/// the middle as before.
#[test]
fn a_certificate_as_the_ca_issues_it_is_not_renewed_before_its_middle() {
    let cases: [Case; 2] = [
        ("reissue-matching", || {
            vec![host_san(HOST), eku(&[ID_KP_CLIENT_AUTH])]
        }),
        ("reissue-no-uri", || {
            vec![san(&[], &[HOST]), eku(&[ID_KP_CLIENT_AUTH])]
        }),
    ];
    for (name, extensions) in cases {
        let (world, mut enroller) = restarted_after(name, extensions);
        assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle, "{name}");
        world.set_now(T0 + WEEK / 2 - 1);
        assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle, "{name}");
        assert!(world.coordinator().renews.is_empty(), "{name}");
        world.set_now(T0 + WEEK / 2);
        assert_eq!(renewed(enroller.run_once()), 2, "{name}");
    }
}

/// The certificates of `HOST` for `key` that cannot authenticate a renewal, by name, each with
/// the CA bundle stored beside it: one of another CA with that CA as its bundle (the pin
/// changed), one of another CA beside the pinned CA's bundle, one that names the pinned CA as
/// its issuer but is signed by another, and one of the pinned CA without client authentication.
fn unusable(pinned: &Ca, key: &SigningKey) -> Vec<(&'static str, String, String)> {
    let other = Ca::new(0x5b);
    let profile = || vec![host_san(HOST), eku(&[ID_KP_CLIENT_AUTH])];
    let issuer = format!("CN=hostkeeper test CA {}", 0x70);
    vec![
        (
            "reissue-other-ca",
            host_leaf(&other, key, profile()),
            other.pem.clone(),
        ),
        (
            "reissue-other-issuer",
            host_leaf(&other, key, profile()),
            pinned.pem.clone(),
        ),
        (
            "reissue-not-signed",
            other.leaf_with_issuer(
                &spki_of(key),
                &format!("CN=host:{HOST}"),
                &issuer,
                profile(),
                (T0, T0 + WEEK),
            ),
            pinned.pem.clone(),
        ),
        (
            "reissue-server-only",
            host_leaf(pinned, key, vec![host_san(HOST), eku(&[ID_KP_SERVER_AUTH])]),
            pinned.pem.clone(),
        ),
    ]
}

/// Catches a certificate that cannot authenticate a renewal stopping every step, or renewed
/// over a connection the coordinator refuses: a certificate of another CA (with that CA's
/// bundle, or beside the pinned one, or naming the pinned CA but signed by another) and one
/// without client authentication each return at the first step with the same key, `renewal_of`
/// the host and no `Renew`; the enrollment still reads the generation as not the host's; once
/// approved the new generation is of the pinned CA, keeps the key, and is used.
#[test]
fn a_certificate_of_another_ca_returns_with_its_key_at_start() {
    let key = signing_key(0x44);
    let pinned = Ca::new(0x70);
    for (name, cert_pem, ca_pem) in unusable(&pinned, &key) {
        let world = World::new(name);
        assert_eq!(world.coordinator().ca.der, pinned.der, "the pinned CA");
        held_by_hand(&world, &key, &cert_pem, &ca_pem);
        let mut enroller = world.open();
        assert_eq!(
            enroller.run_once().expect("post"),
            Outcome::Pending,
            "{name}"
        );
        {
            let c = world.coordinator();
            assert!(c.renews.is_empty(), "{name}");
            assert_eq!(c.enrolls.len(), 1, "{name}");
            assert_eq!(c.enrolls[0].renewal_of, HostId::new(HOST), "{name}");
            assert_eq!(
                csr_spki(&c.enrolls[0].csr_der),
                spki_of(&key).to_der().expect("DER"),
                "{name}"
            );
        }
        assert_eq!(
            enroller.enrollment(),
            Enrollment::Unreadable(
                "the pki directory's generation 1 is not a certificate and key of this host"
                    .to_owned()
            ),
            "{name}"
        );
        world.coordinator().approve(HOST);
        let Outcome::Enrolled(identity) = enroller.run_once().expect("returned") else {
            panic!("{name}: not returned");
        };
        assert_eq!(identity.generation, 2, "{name}");
        assert_eq!(key_of(&world, 2), pkcs8(&key), "{name}: the key is kept");
        assert_eq!(dns_names(&world, 2), [HOST], "{name}");
        assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle, "{name}");
        assert!(
            matches!(enroller.enrollment(), Enrollment::Enrolled { .. }),
            "{name}"
        );
    }
}

/// Catches an enroller that renews at every step against a coordinator that still issues the
/// older profile: the first step renews once, the steps after it renew nothing, the new
/// generation renews at its own middle as before (and not again after it), and a daemon started
/// again asks once more.
#[test]
fn a_coordinator_that_still_issues_the_older_profile_is_asked_once_per_generation() {
    let (world, mut enroller) = restarted_after("reissue-old-coordinator", older_profile);
    issuing(&world, older_profile);
    assert_eq!(renewed(enroller.run_once()), 2);
    assert_eq!(dns_names(&world, 2), Vec::<String>::new());
    for _ in 0..3 {
        assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    }
    assert_eq!(world.coordinator().renews.len(), 1);
    world.set_now(T0 + 1 + WEEK / 2);
    assert_eq!(renewed(enroller.run_once()), 3);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 2);
    drop(enroller);
    let mut enroller = world.open();
    assert_eq!(renewed(enroller.run_once()), 4);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 3);
}

/// Catches a refused early renewal asked again at every step: after the coordinator refuses
/// it, the steps before the middle renew nothing, and at the middle the renewal is asked as it
/// always was.
#[test]
fn a_refused_early_renewal_is_not_asked_again_before_the_middle() {
    let (world, mut enroller) = restarted_after("reissue-refused", older_profile);
    world.coordinator().renew_refusal = Reason::new("denied");
    let refused = enroller.run_once().expect_err("refused");
    assert!(matches!(&refused, Error::RenewRefused(reason) if reason.as_str() == "denied"));
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 1);
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(enroller.run_once(), Err(Error::RenewRefused(_))));
    assert_eq!(world.coordinator().renews.len(), 2);
}

/// Catches an early renewal given up when the coordinator cannot be reached (as while it is
/// updated in the same roll): the step fails, and the next step, once the coordinator is back,
/// renews.
#[test]
fn an_early_renewal_that_cannot_reach_the_coordinator_is_tried_at_the_next_step() {
    let (world, mut enroller) = restarted_after("reissue-down", older_profile);
    world.coordinator().down = Some("connection refused".to_owned());
    assert!(matches!(enroller.run_once(), Err(Error::Transport(_))));
    world.coordinator().down = None;
    assert_eq!(renewed(enroller.run_once()), 2);
    assert_eq!(dns_names(&world, 2), [HOST]);
}

/// Catches an expired certificate of the older profile sent to `Renew`, which it can no longer
/// authenticate: it returns with `renewal_of` the host, as any expired certificate does.
#[test]
fn an_expired_certificate_of_the_older_profile_returns_rather_than_renews() {
    let (world, mut enroller) = restarted_after("reissue-expired", older_profile);
    world.set_now(T0 + WEEK + 1);
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    let c = world.coordinator();
    assert!(c.renews.is_empty());
    assert_eq!(
        c.enrolls.last().map(|r| r.renewal_of.clone()),
        Some(HostId::new(HOST))
    );
}

/// Catches every refusal taken for a return: a certificate of the pinned CA for the host's key
/// that is refused for anything but its CA or client authentication (here a subject alternative
/// name marked critical, which the daemon does not read) is not the host's, and the step stops
/// with nothing sent.
#[test]
fn a_certificate_refused_for_another_reason_stops_the_step() {
    let world = World::new("reissue-critical-san");
    let key = signing_key(0x44);
    let pinned = Ca::new(0x70);
    let cert_pem = host_leaf(
        &pinned,
        &key,
        vec![critical(host_san(HOST)), eku(&[ID_KP_CLIENT_AUTH])],
    );
    held_by_hand(&world, &key, &cert_pem, &pinned.pem);
    let mut enroller = world.open();
    assert!(matches!(enroller.run_once(), Err(Error::Corrupt(1))));
    let c = world.coordinator();
    assert!(c.enrolls.is_empty());
    assert!(c.renews.is_empty());
}
