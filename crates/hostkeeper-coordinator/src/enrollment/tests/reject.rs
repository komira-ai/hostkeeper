//! `Enrollment.Reject` as an operator call: a rejected request leaves the queue and its key is
//! refused, the first rejection is audited once, a request already decided or unknown is not
//! rejected, and a rejection makes room in a full queue.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{DEFAULT_MAX_PENDING, RequestState};
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::{CoordinatorRefusal, EnrollState, PollState};
use hostkeeper_proto::hostkeeper::operator::v1::{
    EnrollmentRefusal, ListPendingRequest, RejectRequest,
};

use super::support::{
    HostKey, Rig, approval, audit_after, audit_of, audited, coordinator_over, enroll_request,
    enrolled, signed_poll,
};
use crate::enrollment::{ACTION_REJECT, RpcError, os_random};
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, ci_token};

/// The rejection of `key`'s request, for `reason`.
fn rejection(key: &HostKey, reason: &str) -> RejectRequest {
    RejectRequest {
        request_id: key.key_hash().to_hex(),
        reason: reason.to_owned(),
    }
}

/// The request ids `Enrollment.List` lists, oldest first.
fn listed(coordinator: &Coordinator, operator: &Caller) -> Vec<String> {
    coordinator
        .enrollment_list(operator, &ListPendingRequest::default())
        .expect("listed")
        .requests
        .into_iter()
        .map(|pending| pending.request_id)
        .collect()
}

/// Proves: `Enrollment.Reject` of a pending request answers its `enroll_reject` entry (by the
/// operator's token, its reason the request id, the claims and the operator's reason), and
/// the audit log gains exactly that entry; the request leaves `Enrollment.List` while another
/// stays; the host's next handshake is refused: its repost is answered with no pending state,
/// its signed `Poll` is answered `DENIED`, and an approval of it is `not_pending`; a second
/// `Reject` succeeds with no entry, and the log holds one `enroll_reject` still.
///
/// Catches: a rejection that leaves the request listed or approvable, a key that can still be
/// issued a certificate, and a repeat that audits the rejection twice.
#[tokio::test]
async fn a_rejected_request_leaves_the_queue_its_key_is_refused_and_it_is_audited_once() {
    let enrollment = Rig::new("reject-flow");
    let rig = coordinator_over("reject-flow-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let e = &enrollment.enrollment;
    let operator = c.caller(Some(OPERATOR)).await;
    let (key, other) = (HostKey::new(), HostKey::new());
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    e.enroll(enroll_request(&other, "machine-2"))
        .expect("enrolled");
    let skip = audit_of(c).len();

    let answer = c
        .enrollment_reject(&operator, &rejection(&key, " not one of ours "))
        .expect("rejected");
    let reason = format!(
        "request {}; claims machine_id=machine-1; not one of ours",
        key.key_hash()
    );
    let entry = answer.entry.expect("the entry");
    assert_eq!(
        (entry.action.as_str(), entry.reason.as_deref()),
        (ACTION_REJECT, Some(reason.as_str()))
    );
    assert_eq!(entry.node_id, None, "a new request names no host");
    let rejected = audited("operator", ACTION_REJECT, None, &reason);
    assert_eq!(audit_after(c, skip), std::slice::from_ref(&rejected));
    assert_eq!(listed(c, &operator), [other.key_hash().to_hex()]);

    let reposted = e
        .enroll(enroll_request(&key, "machine-1"))
        .expect("answered");
    assert_eq!(
        reposted.state,
        EnrollState::Unspecified as i32,
        "not pending"
    );
    let polled = signed_poll(e, &key, &key).expect("polled");
    assert_eq!(polled.state, PollState::Denied as i32);
    assert!(polled.certificate_der.is_empty(), "no certificate");
    assert_eq!(
        c.enrollment_approve(&operator, approval(&key, "s1")),
        Err(EnrollmentRefusal::NotPending)
    );

    let again = c
        .enrollment_reject(&operator, &rejection(&key, ""))
        .expect("rejected again");
    assert_eq!(again.entry, None, "a repeat is not audited");
    let rejections: Vec<_> = audit_after(c, skip)
        .into_iter()
        .filter(|(_, action, _, _)| action == ACTION_REJECT)
        .collect();
    assert_eq!(rejections, [rejected]);
}

/// Proves: a rejected renewal is audited about the host it renews, with no operator's reason
/// when none is given; the host's request is denied, so the key cannot renew that way again.
///
/// Catches: a renewal's rejection audited about no host, or a reason invented for an empty one.
#[tokio::test]
async fn a_rejected_renewal_is_audited_about_the_host_it_renews() {
    let enrollment = Rig::new("reject-renewal");
    let rig = coordinator_over("reject-renewal-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let e = &enrollment.enrollment;
    let key = HostKey::new();
    enrolled(e, &key, "machine-1", "s1");
    let mut renewal = enroll_request(&key, "machine-1");
    renewal.renewal_of = "s1".to_owned();
    e.enroll(renewal).expect("a renewal waits");
    let skip = audit_of(c).len();

    let operator = c.caller(Some(OPERATOR)).await;
    let answer = c
        .enrollment_reject(&operator, &rejection(&key, ""))
        .expect("rejected");
    let reason = format!("request {}; claims machine_id=machine-1", key.key_hash());
    assert_eq!(
        audit_after(c, skip),
        [audited("operator", ACTION_REJECT, Some("s1"), &reason)]
    );
    assert_eq!(
        answer.entry.and_then(|entry| entry.node_id).as_deref(),
        Some("s1")
    );
    let view = e
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held");
    assert_eq!(view.state, RequestState::Denied);
}

/// Proves: `Reject` refuses, and audits and changes nothing for: a caller with no token
/// (`unauthorized`) and CI (`forbidden`); a request id that is no SHA-256 and a reason that
/// breaks its rule (`invalid_request`); a well-formed id of no request (`unknown_request`,
/// NOT_FOUND); a request already approved (`not_pending`); and a store whose write fails
/// (`store_failed`). A coordinator without enrollment answers `unknown_request`.
///
/// Catches: a rejection reachable without the operator token, an unknown id answered as
/// success, and an approved host revoked by `Reject` instead of `Hosts.Retire`.
#[tokio::test]
async fn reject_refuses_what_it_cannot_reject_and_changes_nothing() {
    let enrollment = Rig::new("reject-refusals");
    let rig = coordinator_over("reject-refusals-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let e = &enrollment.enrollment;
    let (key, approved) = (HostKey::new(), HostKey::new());
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    enrolled(e, &approved, "machine-2", "s2");
    let before = audit_of(c);

    let ci = ci_token();
    for (bearer, refusal) in [
        (None, EnrollmentRefusal::Unauthorized),
        (Some(ci.as_str()), EnrollmentRefusal::Forbidden),
    ] {
        let caller = c.caller(bearer).await;
        assert_eq!(
            c.enrollment_reject(&caller, &rejection(&key, "")),
            Err(refusal)
        );
    }
    let operator = c.caller(Some(OPERATOR)).await;
    let refused = |request: RejectRequest| c.enrollment_reject(&operator, &request);
    let mut bad_id = rejection(&key, "");
    bad_id.request_id = "not-a-hash".to_owned();
    assert_eq!(refused(bad_id), Err(EnrollmentRefusal::InvalidRequest));
    assert_eq!(
        refused(rejection(&key, "line\nbreak")),
        Err(EnrollmentRefusal::InvalidRequest)
    );
    assert_eq!(
        refused(rejection(&key, &"x".repeat(257))),
        Err(EnrollmentRefusal::InvalidRequest)
    );
    assert_eq!(
        refused(rejection(&HostKey::new(), "")),
        Err(EnrollmentRefusal::UnknownRequest)
    );
    assert_eq!(
        refused(rejection(&approved, "")),
        Err(EnrollmentRefusal::NotPending)
    );
    assert_eq!(audit_of(c), before, "nothing is audited");
    let view = e
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held");
    assert_eq!(view.state, RequestState::Pending, "nothing changed");

    let without = crate::testutil::Rig::new("reject-no-enrollment");
    let operator = without.coordinator.caller(Some(OPERATOR)).await;
    assert_eq!(
        without
            .coordinator
            .enrollment_reject(&operator, &rejection(&key, "")),
        Err(EnrollmentRefusal::UnknownRequest)
    );

    let fs = Arc::new(FaultFs::new());
    let faulty = Rig::over("reject-store-fault", fs.clone(), os_random());
    let rig = coordinator_over("reject-store-fault-coordinator", &faulty).await;
    faulty
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        rig.coordinator
            .enrollment_reject(&operator, &rejection(&key, "")),
        Err(EnrollmentRefusal::StoreFailed)
    );
}

/// Proves: with the default cap of 256 requests waiting, a new host's `Enroll` is refused
/// `pending_full`; once the operator rejects one of the waiting requests, the same host's
/// `Enroll` is taken, pending.
///
/// Catches: a rejected request still counted toward the cap, so that the queue only grows.
#[tokio::test]
async fn a_rejection_makes_room_in_a_full_pending_queue() {
    let enrollment = Rig::new("reject-full");
    let rig = coordinator_over("reject-full-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    assert_eq!(DEFAULT_MAX_PENDING, 256);
    let waiting: Vec<HostKey> = (0..DEFAULT_MAX_PENDING)
        .map(|n| {
            let key = HostKey::new();
            e.enroll(enroll_request(&key, &format!("machine-{n}")))
                .expect("waits");
            key
        })
        .collect();
    let late = HostKey::new();
    assert_eq!(
        e.enroll(enroll_request(&late, "machine-late")),
        Err(RpcError::Refused(CoordinatorRefusal::PendingFull))
    );

    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    rig.coordinator
        .enrollment_reject(&operator, &rejection(&waiting[0], "lost"))
        .expect("rejected");
    let taken = e
        .enroll(enroll_request(&late, "machine-late"))
        .expect("room");
    assert_eq!(taken.state, EnrollState::Pending as i32);
}
