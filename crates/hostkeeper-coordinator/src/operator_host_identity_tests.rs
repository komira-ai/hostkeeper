//! `HostIdentity.Get` and `HostIdentity.Approve` as plain functions: what `Get` says of a host of
//! each assurance, what `Approve` writes and audits, each refusal and that it changes nothing, and
//! a store or an audit that fails. In process, with the in-memory store; the failing store is the
//! enrollment's files over a file system that fails.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{ApprovalRecord, Decision, INVENTORY_APPROVER};
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostIdentityRefusal as Refusal;

use crate::enrollment::os_random;
use crate::enrollment::tests::support::{
    HostKey, NOW, Rig as EnrollmentRig, coordinator_over, enroll_request, host_id,
};
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::service_identity_support::{Fix, Held};

fn get(node: &str) -> proto::GetHostIdentityRequest {
    proto::GetHostIdentityRequest {
        node_id: node.to_owned(),
    }
}

fn approve_of(node: &str, key: &str, reason: &str) -> proto::ApproveHostIdentityRequest {
    proto::ApproveHostIdentityRequest {
        node_id: node.to_owned(),
        key_sha256: key.to_owned(),
        reason: reason.to_owned(),
    }
}

/// The `Approve` of the key `held` holds, with the reason `reason`.
fn approve_held(held: &Held, reason: &str) -> proto::ApproveHostIdentityRequest {
    approve_of(&held.name, &held.key.key_hash().to_hex(), reason)
}

fn info(approver: &str, at: u64) -> Option<proto::IdentityApprovalInfo> {
    Some(proto::IdentityApprovalInfo {
        approver: approver.to_owned(),
        approved_unix: at,
    })
}

/// The unix time the enrollment's clock reads, which an approval is stamped with.
const WHEN: u64 = NOW;

/// Proves: `Get` of a host approved by an operator says its assurance is `operator`, names the SHA-256
/// of its key, shows no approval, and says the coordinator signs for it; `Get` of a host approved
/// by an inventory record says `auto` and not allowed until its key is approved, and then shows
/// who approved it and when; `Get` of a retired host says it is denied and not allowed.
///
/// Catches: an auto host reported as allowed, an approval shown for a key that is not the host's,
/// and a retired host reported as allowed.
#[tokio::test]
async fn get_says_the_assurance_the_key_the_approval_and_whether_the_coordinator_signs() {
    let fix = Fix::new("hid-get").await;
    let by_hand = fix.operator_host("node-01");
    let auto = fix.auto_host("node-02");
    let c = fix.coordinator();
    let read = |node: &str| c.host_identity_get(&operator(), &get(node)).expect("read");

    assert_eq!(
        read("node-01"),
        proto::GetHostIdentityResponse {
            node_id: "node-01".to_owned(),
            assurance: proto::HostAssurance::Operator as i32,
            key_sha256: by_hand.key.key_hash().to_hex(),
            allowed: true,
            approval: None,
            denied: false,
        }
    );
    let before = read("node-02");
    assert_eq!(
        before,
        proto::GetHostIdentityResponse {
            node_id: "node-02".to_owned(),
            assurance: proto::HostAssurance::Auto as i32,
            key_sha256: auto.key.key_hash().to_hex(),
            allowed: false,
            approval: None,
            denied: false,
        }
    );
    c.host_identity_approve(&operator(), &approve_held(&auto, ""))
        .expect("approved");
    assert_eq!(
        read("node-02"),
        proto::GetHostIdentityResponse {
            allowed: true,
            approval: info("operator", WHEN),
            ..before
        }
    );
    c.retire_host(
        &operator(),
        &proto::RetireRequest {
            node_id: "node-01".to_owned(),
            reason: String::new(),
        },
    )
    .expect("retired");
    let retired = read("node-01");
    assert_eq!((retired.denied, retired.allowed), (true, false));
}

/// Proves: `Approve` of an auto host's current key writes one audit entry `identity_approve` under
/// the operator's name with the host as its node and the key and the reason as its reason, answers
/// the approval in force, and makes the host allowed; asking again, with the same key, answers the
/// same approval and writes no second entry; a host an operator approved by hand answers no
/// approval and writes no entry.
///
/// Catches: an approval with no audit entry, a repeat that audits again or restamps the approval,
/// and an approval asked of a host that needs none that is audited as a change.
#[tokio::test]
async fn approve_audits_once_and_a_repeat_or_an_operator_host_changes_nothing() {
    let fix = Fix::new("hid-approve").await;
    let auto = fix.auto_host("node-02");
    let by_hand = fix.operator_host("node-01");
    let c = fix.coordinator();

    let first = c
        .host_identity_approve(&operator(), &approve_held(&auto, "racked by us"))
        .expect("approved");
    assert_eq!(
        first,
        proto::ApproveHostIdentityResponse {
            node_id: "node-02".to_owned(),
            approval: info("operator", WHEN),
        }
    );
    let entries = fix.audited("identity_approve");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (
            entries[0].by.as_str(),
            entries[0].node_id.as_deref(),
            entries[0].reason.as_deref()
        ),
        (
            "operator",
            Some("node-02"),
            Some(format!("key {}; racked by us", auto.key.key_hash().to_hex()).as_str())
        )
    );

    fix.farm.world.enrollment.clock.set(WHEN + 60);
    let again = c
        .host_identity_approve(&operator(), &approve_held(&auto, "again"))
        .expect("approved");
    assert_eq!(again, first, "the approval in force is not restamped");
    assert_eq!(fix.audited("identity_approve").len(), 1);

    let hand = c
        .host_identity_approve(&operator(), &approve_held(&by_hand, ""))
        .expect("answered");
    assert_eq!(
        hand,
        proto::ApproveHostIdentityResponse {
            node_id: "node-01".to_owned(),
            approval: None,
        }
    );
    assert_eq!(fix.audited("identity_approve").len(), 1);
}

/// Proves: `Approve` with no reason audits the key alone as the entry's reason.
///
/// Catches: a reason that is made up, or an entry with none.
#[tokio::test]
async fn an_approve_with_no_reason_audits_the_key() {
    let fix = Fix::new("hid-no-reason").await;
    let auto = fix.auto_host("node-02");
    fix.coordinator()
        .host_identity_approve(&operator(), &approve_held(&auto, "  "))
        .expect("approved");
    assert_eq!(
        fix.audited("identity_approve")[0].reason,
        Some(format!("key {}", auto.key.key_hash().to_hex()))
    );
}

/// Proves: an `Approve` that names another key than the host's current one is refused
/// `key_mismatch` and changes nothing: no approval, no audit entry, the host still not allowed.
/// So is one that names the key the host held before a replace.
///
/// Catches: a key hash that is not compared, which would let an approval of the wrong machine
/// stand, and one refused after its audit entry was written.
#[tokio::test]
async fn a_wrong_key_hash_is_refused_and_changes_nothing() {
    let fix = Fix::new("hid-mismatch").await;
    let auto = fix.auto_host("node-02");
    let c = fix.coordinator();
    let other = HostKey::new().key_hash().to_hex();
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_of("node-02", &other, "x")),
        Err(Refusal::KeyMismatch)
    );
    let renewed = fix.auto_host_replaced("node-02");
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_held(&auto, "stale")),
        Err(Refusal::KeyMismatch)
    );
    assert_eq!(fix.audited("identity_approve"), []);
    let read = c
        .host_identity_get(&operator(), &get("node-02"))
        .expect("read");
    assert_eq!(
        (read.allowed, read.approval, read.key_sha256),
        (false, None, renewed.key.key_hash().to_hex())
    );
}

/// Proves: each field that breaks its rule is `invalid_request` (a node id that is no host id, a
/// key hash that is not 64 lower-case hex characters, a reason of 257 bytes or with a control
/// character), a host never enrolled is `unknown_host` for both calls, and a retired host is
/// `denied` for `Approve`, which changes nothing.
///
/// Catches: a refusal that names the wrong token, and an approval of a host an operator retired.
#[tokio::test]
async fn each_refusal_names_its_token_and_changes_nothing() {
    let fix = Fix::new("hid-refusals").await;
    let auto = fix.auto_host("node-02");
    let c = fix.coordinator();
    let key = auto.key.key_hash().to_hex();
    assert_eq!(
        c.host_identity_get(&operator(), &get("Not A Host")),
        Err(Refusal::InvalidRequest)
    );
    for (node, hash, reason) in [
        ("Not A Host", key.as_str(), ""),
        ("", key.as_str(), ""),
        ("node-02", "abc", ""),
        ("node-02", key.to_uppercase().as_str(), ""),
        ("node-02", &key[..63], ""),
        ("node-02", key.as_str(), &"r".repeat(257)),
        ("node-02", key.as_str(), "bad\u{7}reason"),
    ] {
        assert_eq!(
            c.host_identity_approve(&operator(), &approve_of(node, hash, reason)),
            Err(Refusal::InvalidRequest),
            "{node:?} {hash:?} {reason:?}"
        );
    }
    assert_eq!(
        c.host_identity_get(&operator(), &get("node-77")),
        Err(Refusal::UnknownHost)
    );
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_of("node-77", &key, "")),
        Err(Refusal::UnknownHost)
    );
    c.retire_host(
        &operator(),
        &proto::RetireRequest {
            node_id: "node-02".to_owned(),
            reason: String::new(),
        },
    )
    .expect("retired");
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_held(&auto, "")),
        Err(Refusal::Denied)
    );
    assert_eq!(fix.audited("identity_approve"), []);
}

/// Proves: no credential is `unauthorized` and a CI token is `forbidden` for both calls, whatever
/// the request, before it is looked at.
///
/// Catches: a CI token that can approve a host for identity.
#[tokio::test]
async fn only_an_operator_may_call() {
    let fix = Fix::new("hid-callers").await;
    let auto = fix.auto_host("node-02");
    let c = fix.coordinator();
    for (caller, refusal) in [
        (Caller::Anonymous, Refusal::Unauthorized),
        (Caller::Ci, Refusal::Forbidden),
    ] {
        assert_eq!(c.host_identity_get(&caller, &get("node-02")), Err(refusal));
        assert_eq!(
            c.host_identity_approve(&caller, &approve_held(&auto, "")),
            Err(refusal)
        );
    }
    assert_eq!(fix.audited("identity_approve"), []);
}

/// Proves: an `Approve` whose audit entry cannot be written is `audit_unavailable` and changes
/// nothing (the host is still not allowed), and once the store takes writes the same call
/// succeeds.
///
/// Catches: an approval that stands with no audit entry.
#[tokio::test]
async fn an_audit_entry_that_cannot_be_written_changes_nothing() {
    let fix = Fix::new("hid-audit-fails").await;
    let auto = fix.auto_host("node-02");
    let c = fix.coordinator();
    fix.farm
        .world
        .fake
        .refuse(|_| Some(crate::store::audit_failure()));
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_held(&auto, "")),
        Err(Refusal::AuditUnavailable)
    );
    let read = c
        .host_identity_get(&operator(), &get("node-02"))
        .expect("read");
    assert_eq!((read.allowed, read.approval), (false, None));
    fix.farm.world.fake.heal();
    assert!(
        c.host_identity_approve(&operator(), &approve_held(&auto, ""))
            .is_ok()
    );
}

/// Proves: with every file operation of the enrollment's store failing in turn, `Approve` of an
/// auto host is `store_failed`, with no approval recorded until the one operation that makes it
/// stand has run; asking again then answers the approval in force; `Get` is `store_failed` for a
/// store that cannot be read.
///
/// Catches: an approval half written, and a failure of the store told as another refusal.
#[tokio::test]
async fn a_store_that_cannot_be_read_or_written_is_store_failed_and_changes_nothing() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = EnrollmentRig::over("hid-store-fault", fs.clone(), os_random());
    let key = HostKey::new();
    let store = enrollment.enrollment.store();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("posted");
    store
        .approve_recorded(
            &key.key_hash(),
            &key.key_hash(),
            &Decision {
                host_id: host_id("node-02"),
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
    let rig = coordinator_over("hid-store-fault-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let request = approve_of("node-02", &key.key_hash().to_hex(), "");

    fs.fail_at(fs.count() + 1);
    assert_eq!(
        c.host_identity_get(&operator(), &get("node-02")),
        Err(Refusal::StoreFailed)
    );
    let mut failures = 0;
    loop {
        fs.fail_at(fs.count() + 1 + failures);
        let Err(refusal) = c.host_identity_approve(&operator(), &request) else {
            panic!("the call succeeded with operation {} failing", failures + 1);
        };
        assert_eq!(refusal, Refusal::StoreFailed, "operation {}", failures + 1);
        fs.fail_at(0);
        failures += 1;
        let read = c
            .host_identity_get(&operator(), &get("node-02"))
            .expect("read");
        if read.approval.is_some() {
            // The write landed and a later operation (the sync of its directory) failed: the
            // approval stands, and asking again answers it and writes nothing more.
            break;
        }
        assert!(failures < 64, "the call keeps failing before it writes");
    }
    assert!(failures >= 3, "the call reads the store before it writes");
    let again = c
        .host_identity_approve(&operator(), &request)
        .expect("approved");
    assert_eq!(again.approval, info("operator", WHEN));
}

/// Proves: a coordinator with no enrollment has no host: both calls are `unknown_host` for an
/// operator.
///
/// Catches: a call that panics, or answers a host out of nothing, without enrollment.
#[test]
fn a_coordinator_without_enrollment_knows_no_host() {
    let rig = crate::testutil::Rig::new("hid-no-enrollment");
    let c = &rig.coordinator;
    assert_eq!(
        c.host_identity_get(&operator(), &get("node-01")),
        Err(Refusal::UnknownHost)
    );
    assert_eq!(
        c.host_identity_approve(&operator(), &approve_of("node-01", &"a".repeat(64), "")),
        Err(Refusal::UnknownHost)
    );
}
