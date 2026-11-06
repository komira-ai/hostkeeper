#![cfg(all(target_os = "linux", feature = "privileged"))]
//! A host's first boot with the real programs: the coordinator (`hostkeeper-coordinator serve`,
//! making its CA) and the daemon (`hostkeeper daemon`), each started by the test as a child
//! process, on loopback, as root against the host's systemd. The test drives them only as an
//! operator and a host do: the flags and files of each, the operator API through `hostkeeper
//! ctl`, and the certificate the daemon keeps in its pki directory.
//!
//! What it asserts: the daemon's enrollment request reaches the coordinator's pending list (the
//! CA takes its subject, `CN=host:pending`), the operator's approval gives the daemon a
//! certificate of subject `CN=host:<approved id>` for the key it asked with, and the coordinator
//! hears its heartbeat over mutual TLS with that identity; a coordinator killed and started
//! again on the same files hears the same host again with no new request and no new
//! certificate, and so does a coordinator that a killed and restarted daemon (its boot pass)
//! comes back to.
//!
//! The daemon fixes its pki directory (`/etc/updater/pki`) and installs its self-update units
//! under `/etc/systemd/system`; the test refuses to run on a host that has either and removes
//! them when it ends ([`support::pair::HostPaths`]). The tests of this file run one at a time
//! (`--test-threads=1` in privileged.yml's job first-boot): they share those paths.
//!
//! Nothing here reads a clock. Each wait looks at a state (a ctl answer, a file) a bounded number
//! of times ([`support::spawn::poll`]); the coordinator's port is the one the system gave its
//! first start.

mod support;

use serde_json::Value;
use support::Bench;
use support::pair::{BEACON, Coordinator, Daemon, HostCertificate, HostPaths, ROLE, rollout};
use support::spawn::poll;

/// A release channel nothing fetches from: the daemon's config needs one, and no test of this
/// file orders a package.
const NO_CHANNEL: &str = "https://127.0.0.1:9";

/// The issuer every host certificate names: the subject of the coordinator's CA.
fn ca_subject(coordinator: &Coordinator) -> String {
    let der = std::fs::read(coordinator.ca_cert_path()).expect("the CA certificate");
    let (_, cert) = x509_parser::parse_x509_certificate(&der).expect("a certificate");
    cert.subject().to_string()
}

/// Waits for the daemon's certificate once its request is approved.
fn wait_certificate(coordinator: &mut Coordinator, daemon: &mut Daemon) -> HostCertificate {
    poll("the daemon's host certificate", || {
        coordinator.assert_running();
        daemon.assert_running();
        HostCertificate::current()
    })
}

/// No enrollment request is pending.
fn assert_none_pending(coordinator: &Coordinator) {
    let pending = coordinator.pending().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(pending, Vec::<Value>::new(), "no request is pending");
}

/// Catches a daemon whose request the CA refuses (a CSR subject other than `CN=host:pending`), a
/// daemon that sends no heartbeat over its certificate, and a coordinator that does not know an
/// approved host after a restart (each never lists the host), and a daemon that enrolls again
/// after its own restart or the coordinator's.
#[test]
fn a_daemon_enrolls_beats_and_keeps_its_identity_across_both_restarts() {
    let bench = Bench::new("first_boot");
    let _host = HostPaths::claim(&[], &[]);
    let scratch = bench.scratch();
    let node = "ci-host-1";

    // 1. The coordinator makes its CA, on ports of loopback the system picks.
    let mut coordinator = Coordinator::start(scratch, &rollout(""), &[]);
    // 2. The daemon, pinning that CA, asks to enroll: its request is pending.
    let mut daemon = Daemon::start(scratch, &coordinator, NO_CHANNEL);
    // 3. The operator approves it; the daemon's next poll gets its certificate.
    let request = coordinator.approve_first(node, &mut daemon);
    assert!(
        request["image"]
            .as_str()
            .is_some_and(|image| image.starts_with("hostkeeper-")),
        "the request names the daemon's image: {request}"
    );
    assert_eq!(
        request["renewalOf"],
        Value::Null,
        "a first request: {request}"
    );
    let first = wait_certificate(&mut coordinator, &mut daemon);
    assert_eq!(first.subject, format!("CN=host:{node}"));
    assert_eq!(first.issuer, ca_subject(&coordinator));
    assert_eq!(
        Some(first.key_sha256.as_str()),
        request["keySha256"].as_str(),
        "the certificate is for the key the daemon asked with"
    );
    poll("the approved request leaves the pending list", || {
        let pending = coordinator.pending()?;
        if pending.is_empty() {
            Ok(())
        } else {
            Err(format!("{pending:?}"))
        }
    });

    // 4. The coordinator hears the host's heartbeat, by the identity of its certificate.
    let heard = coordinator.wait_heard(node, &mut daemon);
    assert_eq!(heard["role"], ROLE, "{heard}");

    // 5. The coordinator is killed and started again on the same files and ports. It knows
    // nothing of the fleet at a start, so the host listed again is a heartbeat it heard since.
    coordinator.restart();
    let heard = coordinator.wait_heard(node, &mut daemon);
    assert_eq!(heard["role"], ROLE, "{heard}");
    assert_eq!(
        HostCertificate::current().as_ref(),
        Ok(&first),
        "the same certificate: no new enrollment"
    );
    assert_none_pending(&coordinator);

    // 7. The daemon is killed. A coordinator started again while it is down has heard nothing
    // of it; the daemon, started again (its boot pass), comes back with the same identity.
    daemon.kill();
    coordinator.restart();
    assert_eq!(
        coordinator.assigned(BEACON, node),
        Ok(None),
        "no heartbeat while the daemon is down"
    );
    daemon.start_again();
    let heard = coordinator.wait_heard(node, &mut daemon);
    assert_eq!(heard["role"], ROLE, "{heard}");
    assert_eq!(
        HostCertificate::current().as_ref(),
        Ok(&first),
        "the same certificate after the daemon's restart"
    );
    assert_none_pending(&coordinator);
    assert_eq!(
        daemon.log().matches("enrolled as host:").count(),
        1,
        "the daemon enrolled once, over its two starts and the coordinator's three:\n{}",
        daemon.log()
    );
}

/// Catches a renewal that does not happen at half life, a renewed certificate of another subject
/// or key, and a daemon that stops beating over its renewed certificate.
#[test]
#[ignore = "no flag of the coordinator sets the validity of the host certificates it issues: https://github.com/komira-ai/hostkeeper/issues/100"]
fn a_short_lived_certificate_is_renewed_and_the_heartbeat_goes_on() {
    let bench = Bench::new("first_boot_renewal");
    let _host = HostPaths::claim(&[], &[]);
    let scratch = bench.scratch();
    let node = "ci-host-2";

    // 6. The coordinator issues certificates valid for a minute: the daemon renews at half life.
    let mut coordinator =
        Coordinator::start(scratch, &rollout(""), &["--host-cert-validity-secs", "60"]);
    let mut daemon = Daemon::start(scratch, &coordinator, NO_CHANNEL);
    coordinator.approve_first(node, &mut daemon);
    let first = wait_certificate(&mut coordinator, &mut daemon);
    coordinator.wait_heard(node, &mut daemon);

    let renewed = poll("a renewed certificate", || {
        coordinator.assert_running();
        daemon.assert_running();
        let now = HostCertificate::current()?;
        if now.serial == first.serial {
            Err(format!("still serial {}", now.serial))
        } else {
            Ok(now)
        }
    });
    assert_ne!(renewed.generation, first.generation);
    assert_eq!(renewed.subject, first.subject);
    assert_eq!(
        renewed.key_sha256, first.key_sha256,
        "renewed for the same key"
    );
    assert_none_pending(&coordinator);

    // A coordinator started again hears the host over its renewed certificate.
    coordinator.restart();
    coordinator.wait_heard(node, &mut daemon);
    assert_eq!(
        HostCertificate::current().map(|c| c.serial),
        Ok(renewed.serial)
    );
}
