#![cfg(all(target_os = "linux", feature = "privileged"))]
//! A host's first boot with the real programs: the coordinator (`hostkeeper-coordinator serve`,
//! making its CA) and the daemon (`hostkeeper daemon`), each started by the test as a child
//! process, on loopback, as root against the host's systemd. The test drives them only as an
//! operator and a host do: the flags and files of each, the operator API through its
//! gRPC clients, and the certificate the daemon keeps in its pki directory.
//!
//! What it asserts: the daemon's enrollment request reaches the coordinator's pending list (the
//! CA takes its subject, `CN=host:pending`), the operator's approval gives the daemon a
//! certificate of subject `CN=host:<approved id>` for the key it asked with, and the coordinator
//! hears its heartbeat over mutual TLS with that identity (`hosts list`: the host, its role, its
//! enrollment and its heartbeat); a coordinator killed and started
//! again on the same files hears the same host again with no new request, no new certificate and
//! the same enrollment, and so does a coordinator that a killed and restarted daemon (its boot pass)
//! comes back to; and a daemon whose certificate the coordinator issues for a minute
//! (`--host-cert-validity-secs 60`) renews it, for the same subject and key, and is heard over
//! the renewed one.
//!
//! The daemon fixes its pki directory (`/etc/updater/pki`), keeps the repository entries the
//! coordinator pushes it in `/etc/hostkeeper/repositories`, and installs its self-update units
//! under `/etc/systemd/system`; the test refuses to run on a host that has any of them and removes
//! them when it ends ([`support::pair::HostPaths`]). The tests of this file run one at a time
//! (`--test-threads=1` in privileged.yml's job first-boot): they share those paths.
//!
//! Nothing here reads a clock. Each wait looks at a state (an operator API answer, a file) a bounded number
//! of times ([`support::spawn::poll`]); the coordinator's port is the one the system gave its
//! first start.

mod support;

use serde_json::Value;
use support::Bench;
use support::pair::{BEACON, Coordinator, Daemon, HostCertificate, HostPaths, ROLE};
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

/// `hosts list` lists `heard` as the approved host heard since the coordinator started: of
/// [`ROLE`], live, enrolled at `enrolled` (the unix second of its approval), its heartbeat the
/// daemon's.
fn assert_heard(heard: &Value, enrolled: &Value) {
    assert_eq!(heard["role"], ROLE, "{heard}");
    assert_eq!(heard["live"], true, "{heard}");
    assert_eq!(
        &heard["enrolledUnix"], enrolled,
        "the same enrollment: {heard}"
    );
    assert_eq!(heard["heartbeat"]["agent"]["name"], "hostkeeper", "{heard}");
}

/// No enrollment request is pending.
fn assert_none_pending(coordinator: &Coordinator) {
    let pending = coordinator.pending().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(pending, Vec::<Value>::new(), "no request is pending");
}

/// Catches a daemon whose request the CA refuses (a CSR subject other than `CN=host:pending`), a
/// daemon that sends no heartbeat over its certificate, and a coordinator that does not know an
/// approved host after a restart (each never lists the host as heard), a restarted coordinator
/// that does not list an enrolled host it has not heard yet, or lists it as live, and a daemon
/// that enrolls again after its own restart or the coordinator's.
#[test]
fn a_daemon_enrolls_beats_and_keeps_its_identity_across_both_restarts() {
    let bench = Bench::new("first_boot");
    let _host = HostPaths::claim(&[], &[]);
    let scratch = bench.scratch();
    let node = "ci-host-1";

    // 1. The coordinator makes its CA, on ports of loopback the system picks.
    let mut coordinator = Coordinator::start(scratch, &[]);
    coordinator.put_group(ROLE, &[BEACON]);
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
    let enrolled = heard["enrolledUnix"].clone();
    assert!(enrolled.is_string(), "listed as enrolled: {heard}");
    assert_heard(&heard, &enrolled);

    // 5. The coordinator is killed and started again on the same files and ports. It knows
    // nothing of the fleet at a start: it lists the host from its enrollment, not live, until it
    // hears the host's next heartbeat.
    coordinator.restart();
    assert_heard(&coordinator.wait_heard(node, &mut daemon), &enrolled);
    assert_eq!(
        HostCertificate::current().as_ref(),
        Ok(&first),
        "the same certificate: no new enrollment"
    );
    assert_none_pending(&coordinator);

    // 7. The daemon is killed. A coordinator started again while it is down has heard nothing
    // of it and lists it from its enrollment alone: not live, no heartbeat time, the daemon its
    // enrollment kept from the last heartbeat. The daemon, started again (its boot pass), comes
    // back with the same identity.
    daemon.kill();
    coordinator.restart();
    let unheard = coordinator.assert_unheard(node);
    assert_eq!(unheard["role"], ROLE, "{unheard}");
    assert_eq!(&unheard["enrolledUnix"], &enrolled, "{unheard}");
    assert_eq!(
        unheard["heartbeat"]["agent"]["name"], "hostkeeper",
        "the daemon its enrollment kept: {unheard}"
    );
    daemon.start_again();
    assert_heard(&coordinator.wait_heard(node, &mut daemon), &enrolled);
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

/// Catches a coordinator whose `--host-cert-validity-secs` does not reach the certificates it
/// issues and renews, a renewal that does not happen, a renewed certificate of another subject or
/// key, and a coordinator that does not hear the daemon over its renewed certificate. The wait is
/// for the certificate's serial to change, not for a time.
#[test]
fn a_short_lived_certificate_is_renewed_and_the_heartbeat_goes_on() {
    let bench = Bench::new("first_boot_renewal");
    let _host = HostPaths::claim(&[], &[]);
    let scratch = bench.scratch();
    let node = "ci-host-2";

    // 6. The coordinator issues certificates valid for a minute: the daemon renews at half life.
    let mut coordinator = Coordinator::start(scratch, &["--host-cert-validity-secs", "60"]);
    coordinator.put_group(ROLE, &[BEACON]);
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

    // A coordinator started again hears the host over a renewed certificate: this one, or a
    // later renewal of it, since the daemon renews again every half minute whatever the
    // restart takes.
    coordinator.restart();
    coordinator.wait_heard(node, &mut daemon);
    let now = HostCertificate::current().unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(now.serial, first.serial, "a renewed certificate");
    assert_eq!(
        (&now.subject, &now.key_sha256),
        (&renewed.subject, &renewed.key_sha256)
    );
}
