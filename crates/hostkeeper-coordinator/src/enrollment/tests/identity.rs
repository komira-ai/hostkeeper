//! The service identity calls of the enrollment over a store whose file operations fail in turn:
//! what an issue and a read of the deny list answer, and that a certificate is never answered
//! unless its audit entry was written; and the two small mappings of the operator's approval.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    IdentityApproval, IdentityApproved, IdentityError, StoreError,
};
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_enrollment::names::{HostId, KeyHash};
use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::HostIdentityRefusal;

use super::support::{HostKey, Rig, enrolled};
use crate::enrollment::identity::{approval_of, identity_refusal};
use crate::enrollment::{RpcError, os_random};
use crate::ops::OpsStore;
use crate::service_identity_support::request_for;

const SERVICE: &str = "js-agent";

/// An enrollment whose files go through `fs`, auditing into a store of its own when `audited`, and
/// the certificate of the host `s1` an operator approved.
fn rig(name: &str, fs: &Arc<FaultFs>, audited: bool) -> (Rig, Arc<OpsStore>, Vec<u8>) {
    let rig = Rig::over(name, fs.clone(), os_random());
    let ops = Arc::new(OpsStore::volatile());
    if audited {
        rig.enrollment.audit_to(ops.clone());
    }
    let certificate = enrolled(&rig.enrollment, &HostKey::new(), "machine-1", "s1");
    (rig, ops, certificate)
}

/// How many entries `service_identity_issued` the audit holds.
fn issued(ops: &OpsStore) -> usize {
    ops.state()
        .audit
        .iter()
        .filter(|entry| entry.action == "service_identity_issued")
        .count()
}

/// Proves: with every file operation of the store failing in turn, an ask for a service
/// certificate is refused `forbidden` (the host's record cannot be read) or `store_failed` (its
/// deny row, its assurance or the serial cannot be), no certificate is answered and no entry
/// `service_identity_issued` is written, until the failing operation is past the last the ask
/// makes, and then it is issued with its entry.
///
/// Catches: a certificate answered after a failed serial or a record that could not be read, and
/// one issued with no audit entry.
#[test]
fn a_store_that_fails_while_a_certificate_is_issued_issues_none() {
    let fs = Arc::new(FaultFs::new());
    let (rig, ops, certificate) = rig("identity-issue-fault", &fs, true);
    let request = request_for(SERVICE, &HostKey::new());
    let mut refusals = Vec::new();
    loop {
        fs.fail_at(fs.count() + 1 + refusals.len() as u64);
        let answer = rig
            .enrollment
            .service_ask(Some(&certificate), &request)
            .and_then(|ask| rig.enrollment.issue_service(&ask));
        match answer {
            Err(RpcError::Refused(refusal)) => {
                assert_eq!(issued(&ops), 0, "operation {}", refusals.len() + 1);
                refusals.push(refusal);
            }
            Err(other) => panic!("{other:?}"),
            Ok(_) => break,
        }
        assert!(refusals.len() < 64, "the ask keeps failing");
    }
    assert_eq!(issued(&ops), 1);
    assert!(
        refusals.iter().all(|r| [
            CoordinatorRefusal::Forbidden,
            CoordinatorRefusal::StoreFailed
        ]
        .contains(r)),
        "{refusals:?}"
    );
    assert!(refusals.contains(&CoordinatorRefusal::StoreFailed));
}

/// Proves: an enrollment that audits nowhere issues nothing: the ask is `store_failed`, and so a
/// certificate is never answered that no audit log could show.
///
/// Catches: an issue that is skipped over when there is no audit store.
#[test]
fn an_enrollment_with_no_audit_store_issues_nothing() {
    let fs = Arc::new(FaultFs::new());
    let (rig, ops, certificate) = rig("identity-no-audit", &fs, false);
    let ask = rig
        .enrollment
        .service_ask(Some(&certificate), &request_for(SERVICE, &HostKey::new()))
        .expect("the ask is judged");
    assert_eq!(
        rig.enrollment.issue_service(&ask).map(|_| ()),
        Err(RpcError::Refused(CoordinatorRefusal::StoreFailed))
    );
    assert_eq!(issued(&ops), 0);
}

/// Proves: with every file operation of the store failing in turn, `ListDeny` is `forbidden` (the
/// caller's record cannot be read) or an internal failure (a deny row cannot be), and never a
/// list that is short; once past the last operation it answers the list.
///
/// Catches: a deny list that omits a host because its row could not be read.
#[test]
fn a_store_that_fails_while_the_deny_list_is_read_never_answers_a_short_list() {
    let fs = Arc::new(FaultFs::new());
    let (rig, _, certificate) = rig("identity-deny-fault", &fs, true);
    enrolled(&rig.enrollment, &HostKey::new(), "machine-2", "s2");
    let denied = HostId::parse("s2").expect("a host id");
    rig.enrollment.store().deny(&denied).expect("denied");
    let mut failures = Vec::new();
    loop {
        fs.fail_at(fs.count() + 1 + failures.len() as u64);
        match rig.enrollment.list_deny(Some(&certificate)) {
            Err(why) => failures.push(why),
            Ok(listed) => {
                assert_eq!(listed.host_ids, ["s2"]);
                break;
            }
        }
        assert!(failures.len() < 64, "the call keeps failing");
    }
    assert!(failures.contains(&RpcError::Internal));
    assert!(
        failures.iter().all(|why| *why == RpcError::Internal
            || *why == RpcError::Refused(CoordinatorRefusal::Forbidden)),
        "{failures:?}"
    );
}

/// Proves: an error of the store's approval for identity is the refusal of its name (`unknown_host`,
/// `key_mismatch`, `denied`) and a failure of the store is `store_failed`; an approval the store
/// made, or found made, is the approval in force, and a host that needs none has none.
///
/// Catches: a race between the judgement and the write that is told as another refusal.
#[test]
fn the_stores_answers_become_the_operators() {
    let host = HostId::parse("s1").expect("a host id");
    assert_eq!(
        identity_refusal(IdentityError::UnknownHost(host)),
        HostIdentityRefusal::UnknownHost
    );
    assert_eq!(
        identity_refusal(IdentityError::KeyMismatch),
        HostIdentityRefusal::KeyMismatch
    );
    assert_eq!(
        identity_refusal(IdentityError::Denied),
        HostIdentityRefusal::Denied
    );
    assert_eq!(
        identity_refusal(IdentityError::Store(StoreError::Unkeyed("host"))),
        HostIdentityRefusal::StoreFailed
    );
    let approval = IdentityApproval {
        key_hash: KeyHash::of_spki(b"key"),
        approver: "operator".to_owned(),
        approved_unix: 7,
    };
    assert_eq!(
        approval_of(IdentityApproved::Approved(approval.clone())),
        Some(approval.clone())
    );
    assert_eq!(
        approval_of(IdentityApproved::AlreadyApproved(approval.clone())),
        Some(approval)
    );
    assert_eq!(approval_of(IdentityApproved::OperatorAssured), None);
}

/// Proves: an `Approve` of an auto host by an enrollment that audits nowhere is `audit_unavailable`
/// and approves nothing.
///
/// Catches: an approval written with no audit entry because there was no store to audit in.
#[test]
fn an_approval_with_no_audit_store_is_refused_and_changes_nothing() {
    use hostkeeper_enrollment::enroll::{ApprovalRecord, Decision, INVENTORY_APPROVER};
    use hostkeeper_enrollment::names::Role;
    use hostkeeper_proto::hostkeeper::operator::v1::ApproveHostIdentityRequest;

    use super::support::{NOW, enroll_request, host_id};

    let rig = Rig::new("identity-approve-no-audit");
    let key = HostKey::new();
    rig.enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("posted");
    rig.enrollment
        .store()
        .approve_recorded(
            &key.key_hash(),
            &key.key_hash(),
            &Decision {
                host_id: host_id("s1"),
                role: Role::parse("storage").expect("a role"),
                services: Vec::new(),
                replace: false,
                host_group: None,
            },
            &ApprovalRecord {
                approver: INVENTORY_APPROVER.to_owned(),
                approved_unix: NOW,
                reason: String::new(),
                mac_nonce: None,
                mac_enrolled_since_unix: None,
            },
        )
        .expect("approved by the inventory");
    let request = ApproveHostIdentityRequest {
        node_id: "s1".to_owned(),
        key_sha256: key.key_hash().to_hex(),
        reason: String::new(),
    };
    assert_eq!(
        rig.enrollment.identity_approve("operator", &request),
        Err(HostIdentityRefusal::AuditUnavailable)
    );
    let held = rig
        .enrollment
        .store()
        .identity_state(&host_id("s1"))
        .expect("read")
        .expect("a host");
    assert_eq!(held.approval, None);
}
