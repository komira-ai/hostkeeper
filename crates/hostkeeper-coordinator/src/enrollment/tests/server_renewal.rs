//! The server certificate renewed while the coordinator runs: one look of the reload tick
//! ([`crate::enrollment::Enrollment::renew_server`], through [`crate::listen::reload_all`])
//! makes it again at two thirds of its life, written whole and owner-only; a look that cannot
//! read or write it keeps the one in use and says why in the status; and the listener serves
//! the renewed one from the same look. Time is the test's clock; no look waits.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hostkeeper_enrollment::ca::SERVER_VALIDITY_CAP_SECS;
use hostkeeper_enrollment::fs::{FileLock, FileStat, Fs, RealFs};
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_tls::{Alpn, ClientAuth, TlsFiles, TlsSource};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::{ClientConfig, RootCertStore};
use serde_json::json;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use x509_parser::prelude::X509Certificate;

use super::support::{FixedClock, NOW, Rig, SERVER_NAME, coordinator_over};
use crate::enrollment::{ServerCertificate, ServerFile, os_random};
use crate::listen::reload_all;
use crate::testutil::{Log, OPERATOR};
use crate::tokens::OperatorTokens;

const DAY: u64 = 24 * 60 * 60;

/// Two thirds of a server certificate's life: the first second it is due.
const DUE: u64 = SERVER_VALIDITY_CAP_SECS - SERVER_VALIDITY_CAP_SECS / 3;

/// The certificate a server certificate file starts with: its DER, the start and end of its
/// validity in unix seconds, and its serial.
#[derive(Debug, PartialEq, Eq)]
struct Leaf {
    der: Vec<u8>,
    not_before: i64,
    not_after: i64,
    serial: Vec<u8>,
}

fn leaf_of_der(der: &[u8]) -> Leaf {
    let (_, certificate) = x509_parser::parse_x509_certificate(der).expect("a certificate");
    leaf(&certificate, der)
}

fn leaf(certificate: &X509Certificate<'_>, der: &[u8]) -> Leaf {
    let validity = certificate.validity();
    Leaf {
        der: der.to_vec(),
        not_before: validity.not_before.timestamp(),
        not_after: validity.not_after.timestamp(),
        serial: certificate.raw_serial().to_vec(),
    }
}

fn leaf_of(path: &Path) -> Leaf {
    let bytes = std::fs::read(path).expect("the server certificate file");
    let (_, block) = x509_parser::pem::parse_x509_pem(&bytes).expect("PEM");
    leaf_of_der(&block.contents)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// The names in the directory of `rig`, sorted.
fn names_in(rig: &Rig) -> Vec<String> {
    let dir = rig.files.server_cert.parent().expect("a directory");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("listed")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// What the directory of every rig here holds: the CA's two files, the store and the server
/// certificate file, and no temporary file.
const FILES: [&str; 4] = ["ca.key", "ca.key.cert", "enrollment", "server.pem"];

fn secs(n: u64) -> i64 {
    i64::try_from(n).expect("a time")
}

/// The heartbeat listener's TLS source over the server certificate file of `rig`, as the start
/// builds it with enrollment (no client certificate required).
fn heartbeat_source(rig: &Rig) -> TlsSource {
    let served = TlsFiles {
        cert: rig.files.server_cert.clone(),
        key: rig.files.server_cert.clone(),
        client_ca: None,
    };
    let client = ClientAuth {
        optional: true,
        extra_roots: vec![rig.enrollment.ca_bundle().certificate_der().to_vec()],
    };
    TlsSource::files_with(
        "heartbeat",
        served,
        Duration::from_secs(60),
        Alpn::H2AndHttp1,
        client,
    )
    .expect("the listener serves the file")
}

/// A TLS client's clock that reads the test's: certificates are checked at its time.
#[derive(Debug)]
struct TestTime(Arc<FixedClock>);

impl TimeProvider for TestTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(
            self.0.now_unix(),
        )))
    }
}

/// The certificate `source` serves to a client that trusts only the enrollment CA of `rig` and
/// dials [`SERVER_NAME`] at the rig's time: the handshake checks the chain, the name and the
/// validity, in memory.
async fn served_leaf(rig: &Rig, source: &TlsSource) -> Leaf {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            rig.enrollment.ca_bundle().certificate_der().to_vec(),
        ))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let time = Arc::new(TestTime(Arc::clone(&rig.clock)));
    let config = ClientConfig::builder_with_details(provider, time)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let accepted = TlsAcceptor::from(source.config()).accept(server);
    let connected = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(SERVER_NAME).expect("a name"), client);
    let (accepted, connected) = tokio::join!(accepted, connected);
    accepted.expect("the server's handshake");
    let tls = connected.expect("a handshake whose chain ends in the CA");
    let peer = tls.get_ref().1.peer_certificates().expect("a certificate")[0].clone();
    leaf_of_der(peer.as_ref())
}

/// The status of the server certificate as `/v1/status` says it, of a coordinator over `rig`.
async fn status_says(name: &str, rig: &Rig) -> serde_json::Value {
    let coordinator = coordinator_over(name, rig).await;
    let status = coordinator
        .coordinator
        .status(Some(OPERATOR))
        .expect("the operator reads the status");
    serde_json::to_value(&status).expect("JSON")["server_certificate"].clone()
}

/// Proves the tick renews the server certificate at two thirds of its life and not a second
/// sooner: the renewed one starts at the tick's time, runs for the cap, chains to the
/// enrollment CA for [`SERVER_NAME`], is written owner-only with no temporary file left, and
/// the status says its new end.
///
/// Catches: a coordinator that makes its server certificate only at the start, a tick that
/// renews before the threshold (every tick a new serial), a renewal that reads the system clock
/// instead of the enrollment's, and a renewed file open to group or others.
#[tokio::test]
async fn the_tick_renews_the_server_certificate_at_two_thirds_of_its_life() {
    let rig = Rig::new("renew-at-two-thirds");
    let first = leaf_of(&rig.files.server_cert);
    assert_eq!(
        (first.not_before, first.not_after),
        (secs(NOW), secs(NOW + SERVER_VALIDITY_CAP_SECS))
    );
    let started = ServerCertificate {
        not_after: NOW + SERVER_VALIDITY_CAP_SECS,
        renewal_error: None,
    };
    assert_eq!(rig.enrollment.server_certificate(), started);

    rig.clock.set(NOW + DUE - 1);
    rig.enrollment.renew_server();
    assert_eq!(
        leaf_of(&rig.files.server_cert),
        first,
        "a second before the threshold"
    );
    assert_eq!(rig.enrollment.server_certificate(), started);

    rig.clock.set(NOW + DUE);
    let log = Log::default();
    let guard = log.install();
    rig.enrollment.renew_server();
    drop(guard);
    let renewed = leaf_of(&rig.files.server_cert);
    assert_ne!(renewed.serial, first.serial);
    assert_eq!(
        (renewed.not_before, renewed.not_after),
        (secs(NOW + DUE), secs(NOW + DUE + SERVER_VALIDITY_CAP_SECS))
    );
    assert_eq!(mode(&rig.files.server_cert), 0o600);
    assert_eq!(names_in(&rig), FILES);
    assert_eq!(
        rig.enrollment.server_certificate(),
        ServerCertificate {
            not_after: NOW + DUE + SERVER_VALIDITY_CAP_SECS,
            renewal_error: None,
        }
    );
    let text = log.text();
    assert!(
        text.contains("renewed the coordinator's server certificate")
            && text.contains(&format!("path={}", rig.files.server_cert.display())),
        "{text}"
    );
    let source = heartbeat_source(&rig);
    assert_eq!(served_leaf(&rig, &source).await, renewed);

    // The renewed certificate is not due again until two thirds of its own life.
    rig.clock.set(NOW + DUE + DUE - 1);
    rig.enrollment.renew_server();
    assert_eq!(leaf_of(&rig.files.server_cert), renewed);
    assert_eq!(
        status_says("renew-at-two-thirds-status", &rig).await,
        json!({ "not_after": NOW + DUE + SERVER_VALIDITY_CAP_SECS, "renewal_error": null })
    );
}

/// [`RealFs`] whose renames onto the server certificate file fail while `fail` is set: the
/// temporary file is written and synced, and the step that would put it in place fails. The
/// store's own writes (the serial) go through.
#[derive(Debug, Default)]
struct RenameFails {
    fail: AtomicBool,
}

/// The name of the server certificate file of every rig.
const SERVER_FILE: &str = "server.pem";

/// The error a failing rename of [`RenameFails`] gives.
const NO_SPACE: &str = "injected: no space left on the device";

impl Fs for RenameFails {
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn read_private(&self, path: &Path) -> io::Result<Option<(Vec<u8>, FileStat)>> {
        RealFs.read_private(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        RealFs.write_new(path, bytes, mode)
    }
    fn sync_file(&self, path: &Path) -> io::Result<()> {
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.fail.load(Ordering::SeqCst) && to.ends_with(SERVER_FILE) {
            return Err(io::Error::other(NO_SPACE));
        }
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn create_dir(&self, dir: &Path) -> io::Result<()> {
        RealFs.create_dir(dir)
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        RealFs.list(dir)
    }
    fn lock(&self, path: &Path) -> io::Result<FileLock> {
        RealFs.lock(path)
    }
}

/// Proves a renewal whose write fails leaves the certificate in use whole on disk and served,
/// removes its temporary file, logs at error level and puts the exact reason in the status;
/// the next tick that can write renews and clears it.
///
/// Catches: a failed write that truncates or replaces the live file, a temporary file left
/// beside it (a key on disk nobody serves), a failure the operator cannot see, an error that
/// stays in the status after a renewal succeeded, and a tick that gives up after one failure.
#[tokio::test]
async fn a_renewal_that_cannot_be_written_keeps_the_certificate_in_use_and_says_why() {
    let fs = Arc::new(RenameFails::default());
    let rig = Rig::over("renew-write-fails", fs.clone(), os_random());
    let first = leaf_of(&rig.files.server_cert);
    let source = heartbeat_source(&rig);
    let tokens = OperatorTokens::single(OPERATOR);

    rig.clock.set(NOW + DUE + DAY);
    fs.fail.store(true, Ordering::SeqCst);
    let log = Log::default();
    let guard = log.install();
    rig.enrollment.renew_server();
    drop(guard);
    assert_eq!(
        leaf_of(&rig.files.server_cert),
        first,
        "the old file, whole"
    );
    assert_eq!(names_in(&rig), FILES, "no temporary file is left");
    let failed = ServerCertificate {
        not_after: NOW + SERVER_VALIDITY_CAP_SECS,
        renewal_error: Some(NO_SPACE.to_owned()),
    };
    assert_eq!(rig.enrollment.server_certificate(), failed);
    let text = log.text();
    assert!(
        text.contains("ERROR")
            && text.contains(
                "cannot renew the coordinator's server certificate; the one in use is still served"
            )
            && text.contains(NO_SPACE)
            && text.contains(&format!("path={}", rig.files.server_cert.display())),
        "{text}"
    );
    assert_eq!(
        status_says("renew-write-fails-status", &rig).await,
        json!({ "not_after": NOW + SERVER_VALIDITY_CAP_SECS, "renewal_error": NO_SPACE })
    );
    // A whole tick: the renewal fails again and the listener keeps the certificate in use.
    reload_all(
        Some(&rig.enrollment),
        std::slice::from_ref(&source),
        &tokens,
    )
    .await;
    assert_eq!(served_leaf(&rig, &source).await, first);
    assert_eq!(rig.enrollment.server_certificate(), failed);

    fs.fail.store(false, Ordering::SeqCst);
    reload_all(
        Some(&rig.enrollment),
        std::slice::from_ref(&source),
        &tokens,
    )
    .await;
    let renewed = leaf_of(&rig.files.server_cert);
    assert_ne!(renewed.serial, first.serial);
    assert_eq!(renewed.not_before, secs(NOW + DUE + DAY));
    assert_eq!(mode(&rig.files.server_cert), 0o600);
    assert_eq!(names_in(&rig), FILES);
    assert_eq!(
        rig.enrollment.server_certificate(),
        ServerCertificate {
            not_after: NOW + DUE + DAY + SERVER_VALIDITY_CAP_SECS,
            renewal_error: None,
        }
    );
    assert_eq!(served_leaf(&rig, &heartbeat_source(&rig)).await, renewed);
}

/// Proves a server certificate file the tick cannot read is a renewal error, not a reason to
/// write over it, and that a file removed while the coordinator runs is made again at the next
/// tick, whatever the time.
///
/// Catches: a read error taken for a missing file (the path would be written over blind), and
/// a missing file left missing until a restart.
#[tokio::test]
async fn a_file_the_tick_cannot_read_is_an_error_and_a_missing_one_is_made_again() {
    let rig = Rig::new("renew-unreadable");
    let file = ServerFile::new(
        Arc::new(RealFs),
        &rig.files.server_cert,
        &rig.files.names,
        &[],
    )
    .expect("a file");
    assert_eq!(file.path(), rig.files.server_cert.as_path());
    let first = leaf_of(&rig.files.server_cert);
    std::fs::remove_file(&rig.files.server_cert).expect("removed");
    std::fs::create_dir(&rig.files.server_cert).expect("a directory in its place");

    rig.enrollment.renew_server();
    assert_eq!(
        rig.enrollment.server_certificate(),
        ServerCertificate {
            not_after: NOW + SERVER_VALIDITY_CAP_SECS,
            renewal_error: Some("Is a directory (os error 21)".to_owned()),
        }
    );
    assert!(
        rig.files.server_cert.is_dir(),
        "nothing was written over it"
    );

    std::fs::remove_dir(&rig.files.server_cert).expect("removed");
    rig.enrollment.renew_server();
    let made = leaf_of(&rig.files.server_cert);
    assert_ne!(made.serial, first.serial);
    assert_eq!(made.not_before, secs(NOW));
    assert_eq!(mode(&rig.files.server_cert), 0o600);
    assert_eq!(
        rig.enrollment.server_certificate(),
        ServerCertificate {
            not_after: NOW + SERVER_VALIDITY_CAP_SECS,
            renewal_error: None,
        }
    );
}

/// Proves one look of the reload tick renews a due certificate before the TLS sources reload,
/// so the heartbeat listener serves the new serial from that same look; and that a look before
/// the threshold changes nothing the listener serves.
///
/// The rig's clock starts two thirds of a life before the system's time and is moved only by
/// the test: the renewal decides by it alone. The TLS source's reload refuses a leaf that is not
/// valid by the system's clock, so the renewed certificate's window (from the rig's time at the
/// tick, for 90 days) has to hold the system's time while the test runs.
///
/// Catches: a renewal that runs after the sources reload (a new certificate served one interval
/// late), and a tick that renews without the listener picking it up.
#[tokio::test]
async fn one_look_of_the_tick_renews_a_due_certificate_and_the_listener_serves_it() {
    let system = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let start = system - DUE;
    let rig = Rig::at("renew-reload", start);
    let first = leaf_of(&rig.files.server_cert);
    let source = heartbeat_source(&rig);
    let tokens = OperatorTokens::single(OPERATOR);
    assert_eq!(served_leaf(&rig, &source).await.serial, first.serial);

    rig.clock.set(start + DUE - 1);
    reload_all(
        Some(&rig.enrollment),
        std::slice::from_ref(&source),
        &tokens,
    )
    .await;
    assert_eq!(leaf_of(&rig.files.server_cert), first);
    assert_eq!(served_leaf(&rig, &source).await.serial, first.serial);

    rig.clock.set(start + DUE);
    reload_all(
        Some(&rig.enrollment),
        std::slice::from_ref(&source),
        &tokens,
    )
    .await;
    let renewed = leaf_of(&rig.files.server_cert);
    assert_ne!(renewed.serial, first.serial);
    assert_eq!(renewed.not_before, secs(start + DUE));
    assert_eq!(served_leaf(&rig, &source).await, renewed);
    assert_eq!(source.reloads().applied, 1, "one reload, the tick's");
}
