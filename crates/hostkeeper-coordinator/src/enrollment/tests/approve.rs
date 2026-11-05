//! `Enrollment.Approve` through the coordinator's operator call: the key hash an approval
//! names, what one approval records and audits, racing approvals for one host id, `replace`,
//! duplicate claims, and an audit entry that did not land before a stop.

use std::sync::{Arc, Barrier};

use hostkeeper_enrollment::enroll::{RecordedApproval, RequestState};
use hostkeeper_enrollment::names::{KeyHash, Role};
use hostkeeper_proto::hostkeeper::agent::v1::PollState;
use hostkeeper_proto::hostkeeper::operator::v1::{
    ApproveRequest, EnrollmentRefusal, ListPendingRequest,
};

use super::support::{
    HostKey, NOW, Rig, approval, audit_after, audit_of, audited, coordinator_over, enroll_request,
    enrolled, host_id, identity, signed_poll,
};
use crate::assignment::Entry;
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsState, Write};
use crate::rollback_tests::unwritable;
use crate::testutil::OPERATOR;

/// An enrollment, a coordinator over it, and the operator's caller.
async fn rig(name: &str) -> (Rig, crate::testutil::Rig, Caller) {
    let enrollment = Rig::new(name);
    let rig = coordinator_over(&format!("{name}-coordinator"), &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    (enrollment, rig, operator)
}

/// The reason of the audit entry of an approval of `key` by the operator, from `machine`.
fn approved_reason(key: &HostKey, machine: &str) -> String {
    format!(
        "request {}; role storage; services kbf; claims machine_id={machine}; new rack",
        key.key_hash()
    )
}

/// Catches an approval whose key hash is not compared, compared by a prefix, or compared after
/// the approval is written: it is refused `key_mismatch`, no approval and no assignment are
/// written, and one `enroll_approve_refused` entry names the operator's token.
#[tokio::test]
async fn rpc_approve_with_a_different_key_is_refused_and_audited() {
    let (enrollment, rig, operator) = rig("approve-other-key").await;
    let (one, two) = (HostKey::new(), HostKey::new());
    enrollment
        .enrollment
        .enroll(enroll_request(&one, "machine-1"))
        .expect("enrolled");
    let before = audit_of(&rig.coordinator).len();
    let mut near = *one.key_hash().as_bytes();
    near[31] ^= 0x01;
    let near = KeyHash::from_bytes(&near).expect("32 bytes").to_hex();
    for key_sha256 in [two.key_hash().to_hex(), near] {
        let refused = rig.coordinator.enrollment_approve(
            &operator,
            ApproveRequest {
                key_sha256,
                ..approval(&one, "s1")
            },
        );
        assert_eq!(refused, Err(EnrollmentRefusal::KeyMismatch));
    }
    let store = enrollment.enrollment.store();
    let view = store.request(&one.key_hash()).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Pending);
    assert_eq!(store.host_role(&host_id("s1")).expect("read"), None);
    assert!(store.recorded_approvals().expect("read").is_empty());
    let refused = audited(
        "operator",
        "enroll_approve_refused",
        Some("s1"),
        &format!("key_mismatch; request {}", one.key_hash()),
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [refused.clone(), refused]
    );
}

/// Catches an approval looked up by the key hash alone, or by the request id alone: the id of
/// one pending request with the key hash of another is refused, both stay pending, and the
/// right pair approves.
#[tokio::test]
async fn approval_with_another_requests_key_hash_is_refused() {
    let (enrollment, rig, operator) = rig("approve-crossed").await;
    let (one, two) = (HostKey::new(), HostKey::new());
    for (key, machine) in [(&one, "machine-1"), (&two, "machine-2")] {
        enrollment
            .enrollment
            .enroll(enroll_request(key, machine))
            .expect("enrolled");
    }
    for (id_of, hash_of) in [(&one, &two), (&two, &one)] {
        let crossed = ApproveRequest {
            key_sha256: hash_of.key_hash().to_hex(),
            ..approval(id_of, "s1")
        };
        assert_eq!(
            rig.coordinator.enrollment_approve(&operator, crossed),
            Err(EnrollmentRefusal::KeyMismatch)
        );
    }
    let pending = enrollment.enrollment.store().pending().expect("read");
    assert_eq!(pending.len(), 2);
    rig.coordinator
        .enrollment_approve(&operator, approval(&two, "s1"))
        .expect("the request's own key hash approves");
}

/// Catches an approval that does not feed issuance, records the host id, role, services or
/// approver wrongly, audits without the operator's token name, or leaves the services it names
/// unassigned.
#[tokio::test]
async fn approval_with_the_right_key_records_host_role_services_and_approver() {
    let (enrollment, rig, operator) = rig("approve-records").await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let before = audit_of(&rig.coordinator).len();
    let answer = rig
        .coordinator
        .enrollment_approve(&operator, approval(&key, "s1"))
        .expect("approved");
    assert_eq!(answer.host_id, "s1");
    assert!(answer.certificate_issued_at_poll);
    let entry = answer.entry.expect("the audit entry");
    assert_eq!(
        (entry.action.as_str(), entry.node_id.as_deref()),
        ("enroll_approve", Some("s1"))
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            audited(
                "operator",
                "enroll_approve",
                Some("s1"),
                &approved_reason(&key, "machine-1")
            ),
            audited("operator", "assign", Some("s1"), "host s1"),
        ]
    );
    let store = enrollment.enrollment.store();
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(
        recorded,
        [RecordedApproval {
            request_id: key.key_hash(),
            host_id: "s1".to_owned(),
            role: "storage".to_owned(),
            services: vec!["kbf".to_owned()],
            replace: false,
            approver: "operator".to_owned(),
            approved_unix: NOW,
            reason: "new rack".to_owned(),
            claims: recorded[0].claims.clone(),
            mac_nonce: false,
        }]
    );
    assert_eq!(recorded[0].claims.machine_id, "machine-1");
    assert_eq!(
        store.host_role(&host_id("s1")).expect("read"),
        Some(Role::parse("storage").expect("a role"))
    );
    // The services it names are assigned to the host, as `Services.Assign` of the host does.
    let assigned = rig.coordinator.ops().state().assignments;
    assert_eq!(assigned["kbf"].hosts.get("s1"), Some(&Entry::Include));
    // Nothing is listed as waiting any more, and the next poll issues for the host id.
    let listed = rig
        .coordinator
        .enrollment_list(&operator, &ListPendingRequest::default())
        .expect("listed");
    assert!(listed.requests.is_empty());
    let issued = signed_poll(&enrollment.enrollment, &key, &key).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    assert_eq!(issued.host_id, "s1");
    let who = identity(&rig.coordinator, &issued.certificate_der);
    assert_eq!((who.host_id.as_str(), who.role.as_str()), ("s1", "storage"));
}

/// Catches the host id's uniqueness checked outside the lock that records the approval: two
/// approvals racing for one host id, released together, admit exactly one.
#[tokio::test]
async fn racing_approve_rpcs_for_one_host_id_admit_one() {
    let (enrollment, rig, operator) = rig("approve-race").await;
    let keys = [HostKey::new(), HostKey::new()];
    for (n, key) in keys.iter().enumerate() {
        enrollment
            .enrollment
            .enroll(enroll_request(key, &format!("machine-{n}")))
            .expect("enrolled");
    }
    let barrier = Barrier::new(2);
    let coordinator = &rig.coordinator;
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = keys
            .iter()
            .map(|key| {
                let (barrier, operator) = (&barrier, &operator);
                scope.spawn(move || {
                    barrier.wait();
                    coordinator.enrollment_approve(operator, approval(key, "s1"))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("no panic"))
            .collect()
    });
    let admitted = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(admitted, 1, "{results:?}");
    assert!(
        results.contains(&Err(EnrollmentRefusal::HostIdTaken)),
        "{results:?}"
    );
    let recorded = enrollment
        .enrollment
        .store()
        .recorded_approvals()
        .expect("read");
    assert_eq!(
        recorded
            .iter()
            .filter(|approval| approval.host_id == "s1")
            .count(),
        1
    );
}

/// Catches `replace` leaving the old host's certificate accepted, issuing for the old key, or
/// going unaudited, and a host id taken without `replace`.
#[tokio::test]
async fn replace_revokes_the_old_host_and_is_audited() {
    let (enrollment, rig, operator) = rig("approve-replace").await;
    let (first, second) = (HostKey::new(), HostKey::new());
    let old = enrolled(&enrollment.enrollment, &first, "machine-1", "s1");
    assert!(rig.coordinator.identify(&old).is_ok());
    enrollment
        .enrollment
        .enroll(enroll_request(&second, "machine-2"))
        .expect("enrolled");
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator, approval(&second, "s1")),
        Err(EnrollmentRefusal::HostIdTaken)
    );
    let before = audit_of(&rig.coordinator).len();
    let answer = rig
        .coordinator
        .enrollment_approve(
            &operator,
            ApproveRequest {
                replace: true,
                ..approval(&second, "s1")
            },
        )
        .expect("replaced");
    assert_eq!(answer.host_id, "s1");
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            audited(
                "operator",
                "enroll_replace",
                Some("s1"),
                &approved_reason(&second, "machine-2")
            ),
            audited("operator", "assign", Some("s1"), "host s1"),
        ]
    );
    // The certificate of the first key names the host no longer; the second key's does.
    assert!(rig.coordinator.identify(&old).is_err());
    let issued = signed_poll(&enrollment.enrollment, &second, &second).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    assert_ne!(issued.certificate_der, old);
    assert_eq!(
        identity(&rig.coordinator, &issued.certificate_der)
            .host_id
            .as_str(),
        "s1"
    );
}

/// Catches the `duplicate_claim` flag being cosmetic: a second host presenting a machine id
/// another request has is listed as a duplicate claim, a plain approval of it is refused, and
/// only an explicit `replace` approves it.
#[tokio::test]
async fn duplicate_claim_request_needs_an_explicit_replace() {
    let (enrollment, rig, operator) = rig("approve-duplicate").await;
    let (first, clone) = (HostKey::new(), HostKey::new());
    for key in [&first, &clone] {
        enrollment
            .enrollment
            .enroll(enroll_request(key, "machine-1"))
            .expect("enrolled");
    }
    let listed = rig
        .coordinator
        .enrollment_list(&operator, &ListPendingRequest::default())
        .expect("listed");
    let flagged: Vec<(String, bool)> = listed
        .requests
        .iter()
        .map(|request| (request.request_id.clone(), request.duplicate_claim))
        .collect();
    assert!(
        flagged.contains(&(first.key_hash().to_hex(), false)),
        "{flagged:?}"
    );
    assert!(
        flagged.contains(&(clone.key_hash().to_hex(), true)),
        "{flagged:?}"
    );
    let before = audit_of(&rig.coordinator).len();
    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator, approval(&clone, "s2")),
        Err(EnrollmentRefusal::DuplicateClaim)
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [audited(
            "operator",
            "enroll_approve_refused",
            Some("s2"),
            &format!("duplicate_claim; request {}", clone.key_hash())
        )]
    );
    assert_eq!(
        enrollment
            .enrollment
            .store()
            .host_role(&host_id("s2"))
            .expect("read"),
        None
    );
    rig.coordinator
        .enrollment_approve(
            &operator,
            ApproveRequest {
                replace: true,
                ..approval(&clone, "s2")
            },
        )
        .expect("an explicit replace approves it");
}

/// Catches an approval whose audit entry did not land staying unaudited, or being audited
/// twice: the approval and its assignment stand, the entry is written at the next start, and
/// once only.
#[tokio::test]
async fn an_approval_whose_audit_did_not_land_is_audited_at_the_next_start() {
    let enrollment = Rig::new("approve-unaudited");
    let mut rig = coordinator_over("approve-unaudited-coordinator", &enrollment).await;
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_ops(unwritable(OpsState::default()))
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let answer = rig
        .coordinator
        .enrollment_approve(&operator, approval(&key, "s1"))
        .expect("the approval stands");
    assert_eq!(answer.entry, None);
    assert_eq!(
        enrollment
            .enrollment
            .store()
            .host_role(&host_id("s1"))
            .expect("read"),
        Some(Role::parse("storage").expect("a role"))
    );

    let entry = audited(
        "operator",
        "enroll_approve",
        Some("s1"),
        &approved_reason(&key, "machine-1"),
    );
    let count = |rig: &crate::testutil::Rig| {
        audit_of(&rig.coordinator)
            .iter()
            .filter(|audited| **audited == entry)
            .count()
    };
    // Entries a start must not take for this approval's: another action, and approval entries
    // with no reason, a short one, or one that names no request.
    let ops = crate::testutil::ops_of(&rig.dir);
    for (action, reason) in [
        ("hold", Some(format!("request {}; held", key.key_hash()))),
        ("enroll_approve", None),
        ("enroll_approve", Some("request short".to_owned())),
        ("enroll_replace", Some(format!("for {}", key.key_hash()))),
    ] {
        let mut entry = AuditEntry::new("operator", action);
        entry.reason = reason;
        ops.change(Write::BeforeMemory, entry, |_| {})
            .expect("written");
    }
    drop(ops);
    for _ in 0..2 {
        rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
            .with_enrollment(Arc::clone(&enrollment.enrollment));
        assert_eq!(count(&rig), 1);
    }
    // Another ops store taken later is audited into as well.
    let restarted = enrollment.restart();
    let coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&restarted))
        .with_ops(crate::testutil::ops_of(&rig.dir));
    rig.coordinator = coordinator;
    assert_eq!(count(&rig), 1);
}
