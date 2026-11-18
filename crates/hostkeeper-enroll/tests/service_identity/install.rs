//! What a service with an identity gets, and when it is asked again.

use std::os::unix::fs::MetadataExt;

use hostkeeper_enroll::{HostId, IdentityFile, Trust};
use x509_cert::der::Encode;

use crate::support::{
    HOST, HOUR, SERVICE, T0, World, certificate_of, key_of, me, wanted, window_of,
};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).expect("stat").mode() & 0o7777
}

/// Catches a certificate installed with another owner or mode than the account's `0600`, a
/// service directory that is not `0750` with the account's group, or an identity root that is
/// not `0755`.
#[test]
fn a_service_with_an_identity_gets_files_owned_by_its_account_mode_0600() {
    let mut world = World::new("owned-0600");
    world.keeper.step(&[wanted(SERVICE)]);
    for file in [IdentityFile::Key, IdentityFile::Cert, IdentityFile::Ca] {
        let path = world.path(SERVICE, file);
        let meta = std::fs::metadata(&path).expect("the file is installed");
        assert_eq!(
            (meta.mode() & 0o7777, meta.uid(), meta.gid(), meta.is_file()),
            (0o600, me().uid, me().gid, true),
            "{}",
            file.name()
        );
    }
    let service_dir = world.root().join(SERVICE);
    assert_eq!(mode(&service_dir), 0o750);
    assert_eq!(
        std::fs::metadata(&service_dir).expect("stat").gid(),
        me().gid
    );
    assert_eq!(mode(&world.root()), 0o755);
    let leftovers: Vec<_> = std::fs::read_dir(&service_dir)
        .expect("the directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(leftovers.len(), 3, "no temporary file stays: {leftovers:?}");
}

/// Catches the keeper writing something other than the certificate it checked: the installed
/// certificate is the coordinator's, for the key on disk, naming the service and the host, and
/// the CA file is the bundle the host trusts.
#[test]
fn the_installed_certificate_is_for_the_key_on_disk_and_names_the_service_and_host() {
    let mut world = World::new("names");
    world.keeper.step(&[wanted(SERVICE)]);
    let cert = certificate_of(&world.read(SERVICE, IdentityFile::Cert).expect("cert.pem"));
    let key = key_of(&world.read(SERVICE, IdentityFile::Key).expect("key.pem"));
    let spki = x509_cert::spki::SubjectPublicKeyInfoOwned::from_key(key.verifying_key())
        .expect("SPKI")
        .to_der()
        .expect("DER");
    assert_eq!(
        cert.tbs_certificate()
            .subject_public_key_info()
            .to_der()
            .expect("DER"),
        spki
    );
    assert_eq!(window_of(&cert), (T0, T0 + 8 * HOUR));
    assert_eq!(
        world.read(SERVICE, IdentityFile::Ca),
        Some(world.ca.pem.clone().into_bytes())
    );
    let calls = world.issuer.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, SERVICE);
    assert_eq!(
        world.report(SERVICE),
        (Some(T0 + 8 * HOUR), None),
        "the report names the end of the certificate and no error"
    );
    assert_eq!(
        world
            .wall_sites()
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["service_valid_now"]),
        "the wall clock is read for the validity window only, there being no certificate to renew"
    );
}

/// Catches a service that did not ask for an identity being given one: nothing is asked of the
/// coordinator, nothing is written, nothing is reported.
#[test]
fn a_service_without_an_identity_is_not_asked_for_and_gets_nothing() {
    let mut world = World::new("none");
    world.keeper.step(&[]);
    assert_eq!(world.issuer.calls().len(), 0);
    assert!(!world.root().exists(), "no directory is made");
    assert_eq!(world.state.get(), []);
}

/// Catches a certificate renewed early, late, or with a new key: the same key is asked for at
/// half the validity, and not a second before.
#[test]
fn a_certificate_is_renewed_at_half_life_with_the_same_key() {
    let mut world = World::new("half-life");
    world.keeper.step(&[wanted(SERVICE)]);
    let key = world.read(SERVICE, IdentityFile::Key).expect("key.pem");
    let first = world.read(SERVICE, IdentityFile::Cert).expect("cert.pem");
    world.set_wall(T0 + 4 * HOUR - 1);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.issuer.calls().len(), 1, "not asked before half life");
    world.set_wall(T0 + 4 * HOUR);
    world.keeper.step(&[wanted(SERVICE)]);
    let calls = world.issuer.calls();
    assert_eq!(calls.len(), 2, "asked at half life");
    assert_eq!(calls[0].1, calls[1].1, "the same key signed both requests");
    assert_eq!(world.read(SERVICE, IdentityFile::Key), Some(key));
    let second = world.read(SERVICE, IdentityFile::Cert).expect("cert.pem");
    assert_ne!(first, second);
    assert_eq!(
        window_of(&certificate_of(&second)),
        (T0 + 4 * HOUR, T0 + 12 * HOUR)
    );
    assert_eq!(world.report(SERVICE), (Some(T0 + 12 * HOUR), None));
}

/// Catches an expired certificate not being asked for again, and the old one being kept after
/// the new one is installed.
#[test]
fn an_expired_certificate_is_replaced() {
    let mut world = World::new("expired");
    world.keeper.step(&[wanted(SERVICE)]);
    world.set_wall(T0 + 20 * HOUR);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.issuer.calls().len(), 2);
    let cert = certificate_of(&world.read(SERVICE, IdentityFile::Cert).expect("cert.pem"));
    assert_eq!(window_of(&cert), (T0 + 20 * HOUR, T0 + 28 * HOUR));
}

/// Catches a restart between the key's write and the certificate's arrival losing the key: the
/// key is on disk and no certificate is; the next daemon asks for a certificate for the same key
/// and installs one.
#[test]
fn a_restart_after_the_key_and_before_the_certificate_asks_again_for_the_same_key() {
    let mut world = World::new("restart");
    world.issuer.answer_with(|_, _| {
        Err(hostkeeper_enroll::TransportError(
            "the coordinator is away".to_owned(),
        ))
    });
    world.keeper.step(&[wanted(SERVICE)]);
    let key = world
        .read(SERVICE, IdentityFile::Key)
        .expect("the key was written first");
    assert_eq!(world.read(SERVICE, IdentityFile::Cert), None);
    assert_eq!(
        world.report(SERVICE),
        (None, Some("the coordinator is away".to_owned()))
    );

    let mut restarted = world.new_keeper();
    let ca = std::sync::Arc::clone(&world.ca);
    world.issuer.answer_with(move |service, csr| {
        let leaf = crate::support::Leaf::good(service, HOST, T0);
        Ok(hostkeeper_enroll::IssueAnswer::Issued {
            cert_der: leaf.sign(&ca, csr),
            ca_bundle_pem: ca.pem.clone(),
        })
    });
    // The first try failed less than five minutes ago, and the restarted daemon has no memory of
    // it: it asks at its first pass.
    restarted.step(&[wanted(SERVICE)]);
    let calls = world.issuer.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, calls[1].1, "the same key signed both requests");
    assert_eq!(world.read(SERVICE, IdentityFile::Key), Some(key));
    assert!(world.read(SERVICE, IdentityFile::Cert).is_some());
    assert_eq!(world.report(SERVICE), (Some(T0 + 8 * HOUR), None));
    assert_eq!(world.draws_made(), 1, "one key was made in all");
}

/// Catches an account the host does not have being given files: the account's lookup error is
/// the service's reported error, nothing is written and the coordinator is not asked.
#[test]
fn an_unknown_account_is_reported_and_nothing_is_written() {
    let mut world = World::new("no-account");
    let mut unknown = wanted(SERVICE);
    unknown.account = Err("no user named \"svc\"".to_owned());
    world.keeper.step(&[unknown]);
    assert_eq!(
        world.report(SERVICE),
        (
            None,
            Some("the account of the service: no user named \"svc\"".to_owned())
        )
    );
    assert_eq!(world.issuer.calls().len(), 0);
    assert!(!world.root().exists());
}

/// Catches a host with no certificate yet (or an unreadable pki directory) asking anyway: the
/// reason is the service's error and nothing is written or asked.
#[test]
fn a_host_that_has_no_trust_yet_is_reported_and_nothing_is_asked() {
    let mut world = World::new("no-trust");
    world.host.set(Err("cannot read cert.pem".to_owned()));
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        world.report(SERVICE),
        (
            None,
            Some("the host's certificate and CA: cannot read cert.pem".to_owned())
        )
    );
    assert_eq!(world.issuer.calls().len(), 0);
    assert!(!world.root().exists());
}

/// Catches a service that stops wanting an identity staying in the report: it is dropped, and
/// its files stay until its uninstall removes them.
#[test]
fn a_service_no_longer_wanted_leaves_the_report_and_keeps_its_files() {
    let mut world = World::new("dropped");
    world.keeper.step(&[wanted("alpha"), wanted("beta")]);
    let names: Vec<String> = world.state.get().into_iter().map(|r| r.service).collect();
    assert_eq!(names, ["alpha", "beta"]);
    world.keeper.step(&[wanted("beta")]);
    let names: Vec<String> = world.state.get().into_iter().map(|r| r.service).collect();
    assert_eq!(names, ["beta"]);
    assert!(world.path("alpha", IdentityFile::Cert).exists());
    assert_eq!(world.issuer.calls().len(), 2);
}

/// Catches an installed certificate that is not a certificate (a half-written file from another
/// tool) blocking the service: it is replaced.
#[test]
fn an_installed_certificate_that_does_not_parse_is_asked_for_again() {
    let mut world = World::new("garbage-cert");
    world.keeper.step(&[wanted(SERVICE)]);
    let path = world.path(SERVICE, IdentityFile::Cert);
    std::fs::write(&path, b"not a certificate").expect("write");
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.issuer.calls().len(), 2);
    assert!(
        certificate_of(&std::fs::read(&path).expect("read"))
            .tbs_certificate()
            .validity()
            .not_before
            .to_unix_duration()
            .as_secs()
            == T0
    );
}

/// Catches a key file that is not a key being replaced silently: the problem is reported, the
/// coordinator is not asked and the file is left for the operator.
#[test]
fn a_key_file_that_is_not_a_key_is_reported_and_left_alone() {
    let mut world = World::new("bad-key");
    world.keeper.step(&[wanted(SERVICE)]);
    let key_path = world.path(SERVICE, IdentityFile::Key);
    std::fs::remove_file(world.path(SERVICE, IdentityFile::Cert)).expect("remove");
    for (bytes, message) in [
        (
            b"not pem".to_vec(),
            "the key file is not PEM: PEM error: PEM preamble contains invalid data (NUL byte)",
        ),
        (
            b"-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n".to_vec(),
            "the key file is a \"CERTIFICATE\", not a PRIVATE KEY",
        ),
        (
            b"-----BEGIN PRIVATE KEY-----\nZm9v\n-----END PRIVATE KEY-----\n".to_vec(),
            "the key on disk is not a P-256 PKCS#8 key: ",
        ),
    ] {
        std::fs::write(&key_path, &bytes).expect("write");
        world.keeper.step(&[wanted(SERVICE)]);
        let error = world.last_error(SERVICE).expect("an error");
        assert!(
            error.starts_with(message.split(':').next().expect("a prefix")),
            "{error}"
        );
        world.advance_mono(300);
    }
    assert_eq!(world.issuer.calls().len(), 1, "only the first pass asked");
    assert_eq!(
        std::fs::read(&key_path).expect("read"),
        b"-----BEGIN PRIVATE KEY-----\nZm9v\n-----END PRIVATE KEY-----\n"
    );
}

/// Catches a failing disk being hidden: a certificate that cannot be written is the service's
/// error and nothing stale remains as the new certificate; a key that cannot be written stops
/// the request before it is sent.
#[test]
fn a_file_that_cannot_be_written_is_reported() {
    let mut world = World::new("disk");
    world.fail("write", IdentityFile::Key);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        world.last_error(SERVICE).as_deref(),
        Some("key.pem: write key.pem failed")
    );
    assert_eq!(
        world.issuer.calls().len(),
        0,
        "no request before the key is on disk"
    );

    world.fail("write", IdentityFile::Cert);
    world.advance_mono(300);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        world.last_error(SERVICE).as_deref(),
        Some("cert.pem: write cert.pem failed")
    );
    assert_eq!(world.read(SERVICE, IdentityFile::Cert), None);
    assert_eq!(world.report(SERVICE).0, None);
}

/// Catches a host whose id the certificate must name being read from somewhere else than the
/// trust it was given: a certificate for another host id is refused after the host's id changes.
#[test]
fn the_host_id_of_the_trust_is_the_dns_name_the_certificate_must_carry() {
    let mut world = World::new("other-host-id");
    world.host.set(Ok(Trust {
        host_id: HostId::new("db-7").expect("a host id"),
        ca_bundle_pem: world.ca.pem.clone(),
    }));
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        world.last_error(SERVICE).as_deref(),
        Some(
            "the certificate's subject alternative name is not exactly urn:hostkeeper:service:js-agent and db-7"
        )
    );
}

/// Catches a host with no random bytes asking with a key it never made: the failure is the
/// service's error and the coordinator is not asked.
#[test]
fn a_key_that_cannot_be_made_is_reported_and_nothing_is_asked() {
    let mut world = World::new("no-entropy");
    let mut keeper = world.keeper_without_entropy();
    keeper.step(&[wanted(SERVICE)]);
    assert_eq!(
        world.last_error(SERVICE).as_deref(),
        Some("the operating system gave no random bytes: no bytes")
    );
    assert_eq!(world.issuer.calls().len(), 0);
    world.keeper.step(&[]);
}
