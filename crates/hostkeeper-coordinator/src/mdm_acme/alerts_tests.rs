use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::derive;
use crate::alerts::Kind;
use crate::mdm_acme::status::{Failing, Snapshot};
use crate::operator_mdm_push_tests::{alice, rig};

/// Proves: a status with nothing wrong raises nothing; a failing renewal raises
/// `mdm_tls_renewal_failed` naming the attempts, the first time and the message; a refused
/// credential raises `mdm_dns_credential_rejected` with the provider's words; both can stand
/// together.
///
/// Catches: an alert with no reason in it, which sends the operator to the log.
#[test]
fn the_status_raises_the_two_alerts() {
    assert!(derive(&Snapshot::default()).is_empty());
    let failing = Failing {
        since_unix: 1_700_000_000,
        message: "the CA ended the order invalid".to_owned(),
        attempts: 3,
    };
    let alerts = derive(&Snapshot {
        failing: Some(failing.clone()),
        dns_rejected: None,
    });
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].subject.kind, Kind::MdmTlsRenewalFailed);
    assert_eq!(
        alerts[0].message,
        "the certificate for the MDM name could not be renewed (3 attempts since unix \
         1700000000): the CA ended the order invalid. The Macs keep the certificate they have \
         until it expires; upload one by hand from the MDM setup or fix the cause"
    );
    let alerts = derive(&Snapshot {
        failing: Some(failing),
        dns_rejected: Some("the DNS provider refused the credential (status 403)".to_owned()),
    });
    let kinds: Vec<Kind> = alerts.iter().map(|alert| alert.subject.kind).collect();
    assert_eq!(
        kinds,
        [Kind::MdmTlsRenewalFailed, Kind::MdmDnsCredentialRejected]
    );
    assert_eq!(
        alerts[1].message,
        "the DNS provider refused the credential (status 403): upload another DNS credential \
         from the MDM setup, or give this one the right to the zone"
    );
}

/// Proves: on a coordinator, the alerts list shows the two alerts while the setup's status holds
/// them, with the warning severity and the log tokens, and shows none once an attempt succeeded.
#[test]
fn a_coordinator_lists_the_alerts_of_the_renewal_it_runs() {
    let rig = rig("alerts-tls-renewal");
    let status = rig.world.coordinator.mdm_setup().tls_status();
    let listed = || {
        rig.world
            .coordinator
            .list_alerts(&alice(), proto::ListAlertsRequest::default())
            .expect("listed")
            .alerts
            .into_iter()
            .map(|alert| (alert.kind, alert.severity))
            .collect::<Vec<_>>()
    };
    assert!(listed().is_empty());
    status.failed(10, 3_610, "the provider refused", true, 1);
    assert_eq!(
        listed(),
        [
            (
                proto::AlertKind::MdmTlsRenewalFailed as i32,
                proto::AlertSeverity::Warning as i32
            ),
            (
                proto::AlertKind::MdmDnsCredentialRejected as i32,
                proto::AlertSeverity::Warning as i32
            ),
        ]
    );
    status.succeeded();
    assert!(listed().is_empty());
}
