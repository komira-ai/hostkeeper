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

/// Proves: a coordinator with an MDM server alerts on the identities of its live Macs and on its
/// CA, from the tables and the setup: none while every identity is far from expiry, one listing the
/// serial whose identity is inside the window of `renew_alert_days`, and none for an identity of a
/// Mac that left or was never entered.
///
/// Catches: an alert fed by an identity nothing uses, or one that never sees the live Macs.
#[tokio::test]
async fn a_coordinator_alerts_on_the_identities_of_its_live_macs() {
    use crate::mdm_secrets::{Secret, SecretKind};
    use crate::mdm_server::support::Rig;
    use crate::store::mdm::{Challenge, Enrollment, Issuance, MdmStore as _};

    let rig = Rig::new("alerts-identities");
    let alice: crate::operator::Caller =
        crate::operator::Caller::Operator("alice".parse().expect("a name"));
    let unix = i64::try_from(crate::host_groups_support::UNIX).expect("a time");
    let day = 86_400;
    let alerts = |rig: &Rig| {
        rig.coordinator
            .list_alerts(&alice, proto::ListAlertsRequest::default())
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind == proto::AlertKind::MdmIdentityNearExpiry as i32)
            .collect::<Vec<_>>()
    };
    let enter = |n: u32, serial: &str, not_after: i64, enter: bool| {
        let hash = format!("h{n}");
        let cert = format!("c{n}");
        rig.store
            .issue_challenge(&Challenge {
                challenge_sha256: hash.clone(),
                serial: serial.to_owned(),
                udid: "u".to_owned(),
                expires_unix: 1,
                confirmed: false,
            })
            .expect("issued");
        rig.store
            .consume_challenge(
                &hash,
                "k",
                &Issuance {
                    cert_sha256: cert.clone(),
                    cert_der: vec![1],
                    not_after_unix: not_after,
                },
                0,
            )
            .expect("consumed");
        if enter {
            let mut live = Enrollment::new(&format!("e{n}"), serial, "u", 0);
            live.cert_sha256 = Some(cert);
            rig.store.replace_live(&live).expect("entered");
        }
    };
    assert!(alerts(&rig).is_empty(), "no Mac, no CA");
    enter(1, "FAR1", unix + 365 * day, true);
    assert!(alerts(&rig).is_empty());
    enter(2, "NEAR2", unix + 10 * day, true);
    enter(3, "NEVER3", unix + 5 * day, false);
    let found = alerts(&rig);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].version, "90");
    assert!(found[0].message.contains("NEAR2"));
    assert!(!found[0].message.contains("FAR1") && !found[0].message.contains("NEVER3"));
    rig.setup.set_setting("renew_alert_days", "5").expect("set");
    assert!(
        alerts(&rig).is_empty(),
        "10 days left is outside a window of 5"
    );
    rig.setup
        .put_secret(
            SecretKind::MdmCaKey,
            &Secret::new(b"ca".to_vec()),
            Some(unix + 3 * day),
            unix,
        )
        .expect("a CA is held");
    let found = alerts(&rig);
    assert_eq!(found.len(), 1);
    assert!(found[0].message.starts_with("the CA that issues"));
    rig.store.fail(Some("identities_expiring"));
    let found = alerts(&rig);
    assert_eq!(
        found.len(),
        1,
        "the CA is still told when the tables cannot be read"
    );
}
