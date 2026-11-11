//! The alerts over a coordinator: `Alerts.List` and its token, the enrollment store's alerts
//! read from the store, a store that cannot be read, the observation loop, and the flags that
//! set the thresholds.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{Kind, Subject, Thresholds, observe_every};
use crate::operator::Caller;
use crate::testutil::{LEGACY, Log, beat, secs};

/// What a [`Subject`] of `kind` says of `host`, with nothing else.
fn subject(kind: Kind, host: &str) -> Subject {
    Subject {
        kind,
        service: String::new(),
        host: host.to_owned(),
        version: String::new(),
        request: String::new(),
        other_host: String::new(),
    }
}

// ---- over a coordinator -------------------------------------------------------------------

/// Proves: `Alerts.List` takes the operator token only: CI's is `forbidden`, none is
/// `unauthorized`; the operator's answers the alerts, with the coordinator's own time as when
/// each was first observed, kept from one call to the next.
///
/// Catches: alerts shown to a CI token, and a first observation time read from the caller.
#[tokio::test]
async fn alerts_list_takes_the_operator_token_and_answers_the_coordinators_time() {
    use crate::marks_support::{WEB, rig, set};
    let rig = rig("alerts-list", "");
    set(&rig, WEB, "2.0", 1).await;
    rig.coordinator
        .halt(
            &crate::marks_support::operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
    assert_eq!(
        rig.coordinator
            .list_alerts(&Caller::Ci, proto::ListAlertsRequest {}),
        Err(proto::AlertsRefusal::Forbidden)
    );
    assert_eq!(
        rig.coordinator
            .list_alerts(&Caller::Anonymous, proto::ListAlertsRequest {}),
        Err(proto::AlertsRefusal::Unauthorized)
    );
    let unix = rig.coordinator.ops.unix_now();
    let listed = rig
        .coordinator
        .list_alerts(
            &crate::marks_support::operator(),
            proto::ListAlertsRequest {},
        )
        .expect("listed");
    let [halt] = listed.alerts.as_slice() else {
        panic!("one alert: {listed:?}");
    };
    assert_eq!(halt.kind(), proto::AlertKind::Halt);
    assert_eq!(halt.severity(), proto::AlertSeverity::Critical);
    assert_eq!((halt.service.as_str(), halt.host_id.as_str()), ("web", ""));
    assert!(halt.first_observed_unix >= unix, "{halt:?}");
    let again = rig
        .coordinator
        .list_alerts(
            &crate::marks_support::operator(),
            proto::ListAlertsRequest {},
        )
        .expect("listed");
    assert_eq!(again.alerts, listed.alerts);
}

/// Proves: over an enrollment store, a pending request is `waiting_for_operator`, a second
/// machine claiming an enrolled host's MAC is `duplicate_claim_pending` naming that host, a host
/// two live daemons report is `duplicate`, and the coordinator's certificate is
/// `cert_near_expiry` once the window reaches it.
///
/// Catches: the enrollment-store alerts read from somewhere else than the store, or a duplicate
/// claim that does not name the host whose identity it claims.
#[tokio::test]
async fn the_enrollment_store_alerts_read_the_store() {
    use crate::enrollment::tests::support::{
        HostKey, Rig as EnrollmentRig, approval, coordinator_over, enroll_request, identity,
        signed_poll,
    };
    let enrollment = EnrollmentRig::new("alerts-enrollment");
    let mut rig = coordinator_over("alerts-enrollment-coordinator", &enrollment).await;
    // Approved as an operator approves, so the approval is recorded with its approver.
    let one = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&one, "machine-1"))
        .expect("posted");
    rig.coordinator
        .enrollment_approve(&crate::marks_support::operator(), approval(&one, "s1"))
        .expect("approved");
    let first = signed_poll(&enrollment.enrollment, &one, &one)
        .expect("polled")
        .certificate_der;
    rig.coordinator = std::mem::replace(
        &mut rig.coordinator,
        crate::testutil::Rig::start(&rig.dir, &rig.clock),
    )
    .with_alert_thresholds(Thresholds {
        silent_after: secs(3_600),
        cert_window_secs: u64::MAX / 2,
    });
    let s1 = identity(&rig.coordinator, &first);
    rig.coordinator
        .heartbeat_from(s1.clone(), beat("1.0"), Some("machine-1"))
        .expect("a heartbeat");
    rig.coordinator
        .heartbeat_from(s1, beat("1.0"), Some("machine-9"))
        .expect("a heartbeat");
    let waiter = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&waiter, "machine-2"))
        .expect("posted");
    let twin = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&twin, "machine-1"))
        .expect("posted");

    let observed: Vec<Subject> = rig
        .coordinator
        .observe_alerts()
        .expect("observed")
        .into_iter()
        .map(|one| one.alert.subject)
        .collect();
    let mut want = vec![
        Subject {
            request: waiter.key_hash().to_hex(),
            ..subject(Kind::WaitingForOperator, "")
        },
        subject(Kind::Duplicate, "s1"),
        Subject {
            request: twin.key_hash().to_hex(),
            other_host: "s1".to_owned(),
            ..subject(Kind::DuplicateClaimPending, "")
        },
        subject(Kind::CertNearExpiry, ""),
    ];
    want.sort();
    assert_eq!(observed, want);
}

/// Proves: an enrollment store that cannot be read is `store_failed` for `Alerts.List`, not an
/// answer without its alerts.
///
/// Catches: hosts waiting for an operator dropped from the list in silence when the store
/// fails.
#[tokio::test]
async fn an_unreadable_enrollment_store_is_store_failed() {
    use crate::enrollment::os_random;
    use crate::enrollment::tests::support::{Rig as EnrollmentRig, coordinator_over};
    let fs = Arc::new(hostkeeper_enrollment::fs::FaultFs::new());
    let enrollment = EnrollmentRig::over("alerts-failing", fs.clone(), os_random());
    let rig = coordinator_over("alerts-failing-coordinator", &enrollment).await;
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        rig.coordinator.list_alerts(
            &crate::marks_support::operator(),
            proto::ListAlertsRequest {}
        ),
        Err(proto::AlertsRefusal::StoreFailed)
    );
}

/// Proves: the running coordinator observes its alerts every interval, by the runtime's clock
/// (paused here and moved by hand): a halt is logged at the first tick after it stands, and not
/// before; a tick whose observation cannot read the enrollment store logs that.
///
/// Catches: alerts that are only ever logged when someone lists them, and an observation that
/// fails in silence.
#[tokio::test(start_paused = true)]
async fn the_coordinator_observes_its_alerts_every_interval() {
    use crate::enrollment::os_random;
    use crate::enrollment::tests::support::{Rig as EnrollmentRig, coordinator_over};
    let fs = Arc::new(hostkeeper_enrollment::fs::FaultFs::new());
    let enrollment = EnrollmentRig::over("alerts-every", fs.clone(), os_random());
    let mut rig = coordinator_over("alerts-every-coordinator", &enrollment).await;
    let coordinator = Arc::new(std::mem::replace(
        &mut rig.coordinator,
        crate::testutil::Rig::start(&rig.dir, &rig.clock),
    ));
    coordinator
        .halt(
            &crate::marks_support::operator(),
            proto::HaltRequest {
                service: LEGACY.to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
    let log = Log::default();
    let _guard = log.install();
    let every = Duration::from_secs(15);
    let ticking = tokio::spawn(observe_every(Arc::clone(&coordinator), every));
    // The loop starts, and its interval with it, at the first poll.
    tokio::task::yield_now().await;
    let tick = |by: Duration| async move {
        tokio::time::advance(by).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    };
    tick(every / 2).await;
    assert!(!log.text().contains(r#"alert="halt""#), "{}", log.text());
    tick(every / 2).await;
    assert_eq!(
        log.text().matches(r#"alert="halt""#).count(),
        1,
        "{}",
        log.text()
    );
    fs.fail_at(fs.count() + 1);
    tick(every).await;
    assert!(
        log.text().contains("the alerts could not be observed"),
        "{}",
        log.text()
    );
    assert_eq!(log.text().matches(r#"alert="halt""#).count(), 1);
    ticking.abort();
}

// ---- the flags ----------------------------------------------------------------------------

/// The flags of a start with `extra` added; the files named need not exist to be parsed.
fn serve_with(extra: &[&str]) -> Result<crate::config::Serve, clap::Error> {
    use clap::Parser as _;
    let mut args = vec![
        "hostkeeper-coordinator",
        "serve",
        "--heartbeat-listen",
        "192.0.2.1:1",
        "--private-listen",
        "192.0.2.1:2",
        "--tls-cert",
        "tls.pem",
        "--tls-key",
        "tls.key",
        "--client-ca",
        "ca.pem",
        "--state-file",
        "desired.json",
        "--oidc-config",
        "oidc.toml",
        "--operator-token-file",
        "operator.token",
    ];
    args.extend_from_slice(extra);
    let crate::config::Command::Serve(flags) = crate::config::Cli::try_parse_from(args)?.command
    else {
        panic!("serve")
    };
    Ok(flags)
}

/// Proves: without the flags the alerts are derived with 300 s of silence and a 14-day
/// certificate window; `--alert-silent-host-secs` and `--alert-cert-window-secs` set each
/// threshold, and only its own; a silence of 0 s is refused at parsing, and a window of 0 s
/// (alert only once expired) is taken.
///
/// Catches: a default that is not the documented one, the two values crossed on their way to
/// the derivation, and a threshold of no silence at all that would call every host silent.
#[test]
fn the_alert_flags_set_the_thresholds_and_default_to_300_s_and_14_days() {
    use crate::config::alert_thresholds;
    let default = serve_with(&[]).expect("parsed");
    assert_eq!(
        alert_thresholds(&default),
        Thresholds {
            silent_after: secs(300),
            cert_window_secs: 1_209_600,
        }
    );
    assert_eq!(alert_thresholds(&default), Thresholds::default());
    let set = serve_with(&[
        "--alert-silent-host-secs",
        "60",
        "--alert-cert-window-secs",
        "7200",
    ])
    .expect("parsed");
    assert_eq!(
        alert_thresholds(&set),
        Thresholds {
            silent_after: secs(60),
            cert_window_secs: 7_200,
        }
    );
    let zero = serve_with(&["--alert-silent-host-secs", "0"]).expect_err("refused");
    assert_eq!(zero.kind(), clap::error::ErrorKind::ValueValidation);
    let expired_only = serve_with(&["--alert-cert-window-secs", "0"]).expect("parsed");
    assert_eq!(alert_thresholds(&expired_only).cert_window_secs, 0);
}

/// Proves: the alert is derived from the setup's push certificate in use and the setting
/// `push_alert_days`, on a coordinator: none with no certificate, one when the certificate expires
/// inside a threshold, none again when the setting asks for fewer days, and a certificate that
/// was replaced is judged by its newest version.
///
/// Catches: an expiry read off a replaced version, and a setting that does not change when the alert
/// comes.
#[tokio::test]
async fn a_coordinator_alerts_on_the_push_certificate_it_holds() {
    use crate::operator_mdm_push_tests::{DAY, TOPIC, alice, rig};
    use hostkeeper_mdm_push::request::pkcs1_pem;
    use hostkeeper_mdm_push::testkit::{fixture_key, push_certificate_pem};

    let rig = rig("alerts-push-certificate");
    let alerts = |rig: &crate::operator_mdm_push_tests::Rig| {
        rig.world
            .coordinator
            .list_alerts(&alice(), proto::ListAlertsRequest::default())
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind == proto::AlertKind::PushCertNearExpiry as i32)
            .collect::<Vec<_>>()
    };
    assert!(alerts(&rig).is_empty(), "no certificate, no alert");
    // A secret of another kind in the setup is not a certificate and raises nothing.
    rig.world
        .coordinator
        .mdm_put_secret(
            &alice(),
            proto::PutSecretRequest {
                kind: "push_key".to_owned(),
                material: b"a key that is not a certificate".to_vec(),
                not_after_unix: None,
                with_certificate: Vec::new(),
            },
        )
        .expect("stored");
    assert!(alerts(&rig).is_empty(), "a key is not a certificate");
    let key = fixture_key(0);
    let now = rig.now();
    let nearly = push_certificate_pem(key, TOPIC, now - DAY, now + 20 * DAY);
    rig.certificate(&nearly, pkcs1_pem(key).expect("pem").expose())
        .expect("stored");
    // A secret of another kind is not the push certificate, whatever its expiry.
    rig.world
        .coordinator
        .mdm_setup
        .put_secret(
            crate::mdm_secrets::SecretKind::TlsCert,
            &crate::mdm_secrets::Secret::new(b"not read".to_vec()),
            Some(now + DAY),
            1,
        )
        .expect("stored");
    let found = alerts(&rig);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].version, "60");
    assert_eq!(found[0].severity, proto::AlertSeverity::Warning as i32);
    rig.world
        .coordinator
        .mdm_put_setting(
            &alice(),
            &proto::PutSettingRequest {
                name: "push_alert_days".to_owned(),
                value: "10".to_owned(),
            },
        )
        .expect("set");
    assert!(
        alerts(&rig).is_empty(),
        "20 days left is outside a threshold of 10"
    );
    let renewed = push_certificate_pem(key, TOPIC, now - DAY, now + 5 * DAY);
    rig.certificate(&renewed, pkcs1_pem(key).expect("pem").expose())
        .expect("stored");
    assert_eq!(
        alerts(&rig)[0].version,
        "10",
        "the newest certificate's expiry is read"
    );
}

/// Proves: a setup that cannot be read raises no alert of its own and does not fail the list.
///
/// Catches: an alert list that fails because the MDM setup store is down.
#[tokio::test]
async fn a_setup_that_cannot_be_read_does_not_fail_the_alerts() {
    use crate::operator_mdm_push_tests::{alice, flaky_rig};
    use std::sync::atomic::Ordering;

    let (rig, store) = flaky_rig("alerts-push-unreadable");
    store.reads_fail.store(true, Ordering::SeqCst);
    let listed = rig
        .world
        .coordinator
        .list_alerts(&alice(), proto::ListAlertsRequest::default())
        .expect("listed");
    assert!(listed.alerts.is_empty());
}
