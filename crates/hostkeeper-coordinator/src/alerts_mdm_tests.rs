//! The alert `mdm_identity_near_expiry`: its derivation, and its facts read from a coordinator.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::mdm_identity_near_expiry;
use crate::alerts::{Alert, EnrollmentFacts, Kind};

const UNIX: u64 = 1_800_000_000;

/// The alerts of the identities `identities`, the CA expiring at `ca` and the window `days`.
fn identity_alerts(identities: &[(&str, u64)], ca: Option<u64>, days: Option<u32>) -> Vec<Alert> {
    let facts = EnrollmentFacts {
        mdm_identities: identities
            .iter()
            .map(|(serial, at)| ((*serial).to_owned(), *at))
            .collect(),
        mdm_ca_not_after: ca,
        mdm_renew_alert_days: days,
        ..EnrollmentFacts::default()
    };
    mdm_identity_near_expiry(&facts, UNIX)
}

/// Proves: the alert lists the serials whose identity expires inside the window, names the CA when
/// it does, has the window in days as its version and `expired` once any has, and is none when
/// nothing is inside the window or the setting was not read.
///
/// Catches: a renewal missed because a Mac's identity ran out unannounced, and a CA that expires
/// without a word.
#[test]
fn the_identity_alert_lists_the_serials_and_the_ca_inside_the_window() {
    let day = 86_400;
    assert_eq!(identity_alerts(&[], None, Some(90)), Vec::new());
    assert_eq!(
        identity_alerts(&[("A1", UNIX + day)], None, None),
        Vec::new()
    );
    assert_eq!(
        identity_alerts(&[], Some(UNIX + 91 * day), Some(90)),
        Vec::new()
    );
    let alerts = identity_alerts(
        &[("A1", UNIX + day), ("B2", UNIX + 5 * day)],
        None,
        Some(90),
    );
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].subject.kind, Kind::MdmIdentityNearExpiry);
    assert_eq!(alerts[0].subject.version, "90");
    assert_eq!(
        alerts[0].message,
        "the identity certificates of A1, B2 expire within 90 days (or have): enroll each Mac \
         again before they do"
    );
    let both = identity_alerts(&[("A1", UNIX + day)], Some(UNIX + 10 * day), Some(30));
    assert_eq!(both[0].subject.version, "30");
    assert!(both[0].message.contains("A1") && both[0].message.contains("the CA that issues"));
    let ca_only = identity_alerts(&[], Some(UNIX + 10 * day), Some(90));
    assert!(ca_only[0].message.starts_with("the CA that issues"));
    assert_eq!(
        identity_alerts(&[("A1", UNIX)], None, Some(90))[0]
            .subject
            .version,
        "expired"
    );
    assert_eq!(
        identity_alerts(&[], Some(UNIX - day), Some(90))[0]
            .subject
            .version,
        "expired"
    );
    assert_eq!(
        Kind::MdmIdentityNearExpiry.log_token(),
        "mdm_identity_near_expiry"
    );
    assert_eq!(
        Kind::MdmIdentityNearExpiry.severity(),
        proto::AlertSeverity::Warning
    );
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
    let alice = crate::operator::Caller::Operator("alice".parse().expect("a name"));
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
    assert_eq!(
        alerts(&rig).len(),
        1,
        "the CA is still told when the tables cannot be read"
    );
}

/// Proves: a setup that cannot be read raises no identity alert of its own and does not fail the
/// list of alerts.
///
/// Catches: an alert list that fails whenever the MDM setup does.
#[tokio::test]
async fn an_unreadable_setup_raises_no_identity_alert() {
    use std::sync::atomic::Ordering;

    use crate::mdm_secrets::MasterKey;
    use crate::mdm_server::support::Rig;
    use crate::mdm_setup::MdmSetup;
    use crate::operator_mdm_push_tests::Flaky;

    let store = Flaky::new();
    let shared: std::sync::Arc<dyn crate::store::mdm_setup::SetupStore> = store.clone();
    let setup = MdmSetup::new(
        shared,
        Some(MasterKey::from_bytes(&[3; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    let rig = Rig::with_apple_over("alerts-identities-unreadable", Vec::new(), setup);
    store.reads_fail.store(true, Ordering::SeqCst);
    let alice = crate::operator::Caller::Operator("alice".parse().expect("a name"));
    let listed = rig
        .coordinator
        .list_alerts(&alice, proto::ListAlertsRequest::default())
        .expect("listed");
    assert!(listed.alerts.is_empty());
}
