//! `Mdm.ListEnrollmentRequests` and `Mdm.ConfirmEnrollment`.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use super::testkit::{SERIAL, UDID, forged_for, post, rig};
use crate::audit_actions;
use crate::mdm_server::support::{NOW, Rig};
use crate::operator::Caller;
use crate::store::mdm::{AbmBatch, MdmStore as _};

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn confirm(rig: &Rig, serial: &str) -> Result<proto::ConfirmEnrollmentResponse, MdmRefusal> {
    rig.coordinator.mdm_confirm_enrollment(
        &alice(),
        &proto::ConfirmEnrollmentRequest {
            serial: serial.to_owned(),
        },
    )
}

fn remove_from_abm(rig: &Rig) {
    rig.store
        .abm_apply(&AbmBatch {
            removed: vec![SERIAL.to_owned()],
            removed_unix: 5,
            ..AbmBatch::default()
        })
        .expect("removed");
}

/// Proves: the list shows each waiting request with what the blob claimed and why it was not
/// verified, whether the ABM table lists the serial now, and the confirmation once there is one;
/// it is empty with no request, and a store that cannot be read is `store_failed`.
///
/// Catches: a list that shows a serial as ours that ABM no longer lists.
#[tokio::test]
async fn the_list_shows_what_waits_and_whether_abm_lists_it() {
    let rig = rig("operator-list");
    let none = rig
        .coordinator
        .mdm_list_enrollment_requests(&alice())
        .expect("listed");
    assert!(none.requests.is_empty());
    post(&rig, &forged_for(SERIAL, UDID)).await;
    let listed = rig
        .coordinator
        .mdm_list_enrollment_requests(&alice())
        .expect("listed");
    let [request] = listed.requests.as_slice() else {
        panic!("{listed:?}");
    };
    assert_eq!(
        (request.serial.as_str(), request.udid.as_str()),
        (SERIAL, UDID)
    );
    assert_eq!(request.product.as_deref(), Some("Mac14,13"));
    assert!(request.reason.contains("does not verify"));
    assert_eq!(
        (request.count, request.in_abm, request.confirmed_until_unix),
        (1, true, None)
    );
    let at = i64::try_from(NOW).expect("a time");
    assert_eq!((request.first_unix, request.last_unix), (at, at));
    confirm(&rig, SERIAL).expect("confirmed");
    remove_from_abm(&rig);
    let listed = rig
        .coordinator
        .mdm_list_enrollment_requests(&alice())
        .expect("listed");
    assert_eq!(
        (
            listed.requests[0].in_abm,
            listed.requests[0].confirmed_until_unix
        ),
        (false, Some(at + 3600))
    );
    rig.store.fail(Some("abm_device"));
    assert_eq!(
        rig.coordinator.mdm_list_enrollment_requests(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
}

/// Proves: a confirmation reads the ABM table at that moment: a serial that left it since the
/// request came is refused `serial_not_in_abm` and audited, a serial that does not wait is
/// `no_pending_enrollment`, and a serial that is no serial is `invalid_request`; none of these
/// confirms anything.
///
/// Catches: a click that admits a Mac that is not ours any more.
#[tokio::test]
async fn a_confirmation_reads_the_abm_table_again() {
    let rig = rig("operator-confirm-abm");
    assert_eq!(confirm(&rig, SERIAL), Err(MdmRefusal::NoPendingEnrollment));
    assert_eq!(confirm(&rig, "a b"), Err(MdmRefusal::InvalidRequest));
    assert_eq!(confirm(&rig, ""), Err(MdmRefusal::InvalidRequest));
    post(&rig, &forged_for(SERIAL, UDID)).await;
    remove_from_abm(&rig);
    assert_eq!(confirm(&rig, SERIAL), Err(MdmRefusal::SerialNotInAbm));
    assert_eq!(rig.server.enroll.pending.list()[0].1.confirmed_until, None);
    let rows = rig
        .coordinator
        .ops()
        .state()
        .audit
        .iter()
        .map(|e| (e.by.clone(), e.action.clone(), e.reason.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        rows.last().cloned(),
        Some((
            "alice".to_owned(),
            audit_actions::MDM_ENROLL_CONFIRMED.to_owned(),
            Some("refused: MDM_REFUSAL_SERIAL_NOT_IN_ABM".to_owned())
        ))
    );
    rig.store.fail(Some("abm_device"));
    assert_eq!(confirm(&rig, SERIAL), Err(MdmRefusal::StoreFailed));
}

/// Proves: a confirmation of a waiting serial that ABM lists answers when it stands until, audits
/// it under the user's name, and when the audit cannot be written confirms nothing.
///
/// Catches: a confirmation with no audit entry.
#[tokio::test]
async fn a_confirmation_is_audited_before_it_is_made() {
    let refusing = rig("operator-confirm");
    post(&refusing, &forged_for(SERIAL, UDID)).await;
    refusing
        .fake
        .refuse(|_| Some(crate::store::audit_failure()));
    assert_eq!(
        confirm(&refusing, SERIAL),
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        refusing.server.enroll.pending.list()[0].1.confirmed_until,
        None
    );
    let rig = rig("operator-confirm-ok");
    post(&rig, &forged_for(SERIAL, UDID)).await;
    let done = confirm(&rig, SERIAL).expect("confirmed");
    let at = i64::try_from(NOW).expect("a time");
    assert_eq!(
        (done.serial.as_str(), done.expires_unix),
        (SERIAL, at + 3600)
    );
    assert_eq!(
        rig.audited(),
        [
            audit_actions::MDM_ENROLL_HELD,
            audit_actions::MDM_ENROLL_CONFIRMED
        ]
    );
    rig.setup
        .set_setting("challenge_ttl_s", "120")
        .expect("set");
    assert_eq!(confirm(&rig, SERIAL).expect("again").expires_unix, at + 120);
}

/// Proves: both calls are the operator's: an anonymous caller is `unauthorized` and CI's token is
/// `forbidden`; a coordinator with no MDM listener lists nothing and confirms nothing
/// (`enrollment_unavailable`).
///
/// Catches: the list or the click reachable by a token that is not an operator's.
#[tokio::test]
async fn only_an_operator_lists_or_confirms_and_a_coordinator_without_a_listener_has_nothing() {
    let rig = rig("operator-callers");
    for (caller, refusal) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        assert_eq!(
            rig.coordinator.mdm_list_enrollment_requests(&caller),
            Err(refusal)
        );
        assert_eq!(
            rig.coordinator.mdm_confirm_enrollment(
                &caller,
                &proto::ConfirmEnrollmentRequest {
                    serial: SERIAL.to_owned()
                }
            ),
            Err(refusal)
        );
    }
    let bare = crate::host_groups_support::coordinator_listed(
        &rig.fake,
        &std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000)),
        None,
        "",
    );
    assert!(
        bare.mdm_list_enrollment_requests(&alice())
            .expect("listed")
            .requests
            .is_empty()
    );
    assert_eq!(
        bare.mdm_confirm_enrollment(
            &alice(),
            &proto::ConfirmEnrollmentRequest {
                serial: SERIAL.to_owned()
            }
        ),
        Err(MdmRefusal::EnrollmentUnavailable)
    );
}
