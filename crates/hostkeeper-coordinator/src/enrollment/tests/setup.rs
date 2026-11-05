//! The start of enrollment: the CA made only with `--ca-init` and kept owner-only, its
//! fingerprint logged, the server certificate made when missing and again in the last third of
//! its life, and every file that stops the start.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enrollment::ca::{CaError, SERVER_VALIDITY_CAP_SECS};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_tls::{Alpn, ClientAuth, TlsFiles, TlsSource};
use x509_parser::prelude::X509Certificate;

use super::support::{Files, FixedClock, NOW, SERVER_NAME};
use crate::enrollment::{SetupError, ca_certificate_path, os_random, pem, server_due, setup};
use crate::testutil::{Log, scratch};

const DAY: u64 = 24 * 60 * 60;

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// Catches a CA made without `--ca-init`, one made again over the CA it already has (its
/// fingerprint would change under every daemon config), a key file open to others accepted, and
/// the fingerprint not logged.
#[test]
fn the_ca_is_made_once_with_ca_init_and_its_key_file_must_be_owner_only() {
    let dir = scratch("setup-ca");
    let files = Files::in_dir(&dir);
    let clock = FixedClock::at(NOW);
    let start = |init: bool| {
        setup(
            Arc::new(RealFs),
            &files.setup(init),
            clock.clone(),
            os_random(),
        )
    };

    let refused = start(false).expect_err("no CA and no --ca-init");
    assert!(
        matches!(&refused, SetupError::NoCa(path) if *path == files.ca_key),
        "{refused}"
    );
    assert!(refused.to_string().contains("--ca-init"), "{refused}");

    let log = Log::default();
    let guard = log.install();
    let made = start(true).expect("made");
    drop(guard);
    let fingerprint = made.ca_bundle().sha256_hex();
    let debug = format!("{made:?}");
    assert!(
        debug.starts_with("Enrollment { ca: Ca(") && debug.contains(&fingerprint),
        "{debug}"
    );
    assert!(
        log.text().contains(&format!("ca_sha256={fingerprint}")),
        "{}",
        log.text()
    );
    assert_eq!(mode(&files.ca_key), 0o600);
    assert_eq!(
        std::fs::read(ca_certificate_path(&files.ca_key)).expect("the CA certificate"),
        made.ca_bundle().certificate_der()
    );
    for init in [false, true] {
        let again = start(init).expect("opened");
        assert_eq!(again.ca_bundle().sha256_hex(), fingerprint, "init {init}");
    }

    std::fs::set_permissions(&files.ca_key, std::fs::Permissions::from_mode(0o644))
        .expect("a mode");
    let refused = start(true).expect_err("a key open to others");
    assert!(
        matches!(refused, SetupError::Ca(CaError::KeyNotPrivate { .. })),
        "{refused}"
    );
}

/// The certificate the server certificate file starts with, and its validity.
fn leaf_of(path: &std::path::Path) -> (Vec<u8>, i64, i64, bool, bool) {
    let bytes = std::fs::read(path).expect("the server certificate file");
    let (_, block) = x509_parser::pem::parse_x509_pem(&bytes).expect("PEM");
    let certificate: X509Certificate<'_> = block.parse_x509().expect("a certificate");
    let validity = certificate.validity();
    let usage = certificate
        .extended_key_usage()
        .expect("an extension")
        .expect("an extended key usage")
        .value;
    (
        block.contents.clone(),
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
        usage.server_auth,
        usage.client_auth,
    )
}

/// Catches a server certificate not made at the first start, one made again at every start,
/// one kept into the last third of its life, a file that is not a certificate kept, a file
/// readable by others, and a file whose chain and key the listener cannot serve.
#[test]
fn the_server_certificate_is_made_when_missing_and_again_in_the_last_third_of_its_life() {
    let dir = scratch("setup-server");
    let files = Files::in_dir(&dir);
    let clock = FixedClock::at(NOW);
    let start = || {
        files
            .start(Arc::new(RealFs), clock.clone(), os_random())
            .expect("started")
    };
    let enrollment = start();
    assert_eq!(mode(&files.server_cert), 0o600);
    let (first, not_before, not_after, server_auth, client_auth) = leaf_of(&files.server_cert);
    let now = i64::try_from(NOW).expect("a time");
    let cap = i64::try_from(SERVER_VALIDITY_CAP_SECS).expect("a validity");
    assert_eq!((not_before, not_after), (now, now + cap));
    assert_eq!((server_auth, client_auth), (true, false));
    let text = std::fs::read_to_string(&files.server_cert).expect("text");
    let ca = pem("CERTIFICATE", enrollment.ca_bundle().certificate_der());
    assert!(text.contains(&ca), "the chain ends in the CA");
    assert!(text.contains("PRIVATE KEY"));
    let served = TlsFiles {
        cert: files.server_cert.clone(),
        key: files.server_cert.clone(),
        client_ca: None,
    };
    TlsSource::files_with(
        "test",
        served,
        Duration::from_secs(60),
        Alpn::H2,
        ClientAuth::default(),
    )
    .expect("the listener serves the file");

    clock.set(NOW + 59 * DAY);
    start();
    assert_eq!(
        leaf_of(&files.server_cert).0,
        first,
        "two thirds of its life are left"
    );
    clock.set(NOW + 60 * DAY);
    start();
    let (renewed, start_of_renewed, ..) = leaf_of(&files.server_cert);
    assert_ne!(renewed, first, "a third of its life is left");
    assert_eq!(
        start_of_renewed,
        now + i64::try_from(60 * DAY).expect("a time")
    );

    std::fs::write(&files.server_cert, "not a certificate").expect("written");
    start();
    assert_ne!(leaf_of(&files.server_cert).0, renewed);
    assert!(server_due(None, NOW));
    assert!(server_due(Some(b"not a certificate".as_slice()), NOW));
    let not_der = pem("CERTIFICATE", b"not DER");
    assert!(server_due(Some(not_der.as_bytes()), NOW));
    let file = std::fs::read(&files.server_cert).expect("the file");
    assert!(!server_due(Some(file.as_slice()), NOW + 60 * DAY));
    assert!(server_due(Some(file.as_slice()), u64::MAX));
}

/// Catches a start that goes on with a store directory it cannot make, a server certificate
/// path it cannot read or that names no file, or a server name the CA does not sign.
#[test]
fn a_store_or_server_certificate_that_cannot_be_used_stops_the_start() {
    let dir = scratch("setup-refusals");
    let clock = FixedClock::at(NOW);
    let start = |files: &Files| files.start(Arc::new(RealFs), clock.clone(), os_random());
    let refusal = |files: &Files| match start(files) {
        Ok(_) => panic!("the start was not refused"),
        Err(why) => why,
    };

    let mut files = Files::in_dir(&dir);
    std::fs::write(&files.store, "a file").expect("written");
    assert!(matches!(refusal(&files), SetupError::Store(_)));
    files.store = dir.path().join("enrollment-2");

    std::fs::create_dir(&files.server_cert).expect("a directory");
    let unreadable = refusal(&files);
    assert!(
        matches!(&unreadable, SetupError::Server { path, .. } if *path == files.server_cert),
        "{unreadable}"
    );
    assert!(
        unreadable.to_string().starts_with("--server-cert-file "),
        "{unreadable}"
    );

    files.server_cert = dir.path().join("..");
    assert!(
        refusal(&files)
            .to_string()
            .ends_with("does not name a file")
    );

    files.server_cert = dir.path().join("server-2.pem");
    files.names = vec!["Not_A_Name".to_owned()];
    assert!(matches!(refusal(&files), SetupError::Server { .. }));
    files.names = vec![SERVER_NAME.to_owned()];
    assert!(start(&files).is_ok());
}
