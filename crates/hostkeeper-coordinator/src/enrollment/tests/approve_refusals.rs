//! The refusals of `Enrollment.Approve`: each field's rule with its own token, every refusal
//! of the store, a store that fails, and a start that cannot read what it would audit again.
//! Each refusal is audited `enroll_approve_refused` and changes nothing else.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollRequest;
use hostkeeper_proto::hostkeeper::operator::v1::{ApproveRequest, EnrollmentRefusal};

use super::support::{
    HostKey, Rig, approval, audit_after, audit_of, audited, coordinator_over, enroll_request,
    enrolled, host_id,
};
use crate::enrollment::{os_random, token_of};
use crate::testutil::{Log, OPERATOR};

/// Catches a field's rule left to the store, or refused with another field's token: every
/// edge of the host id, role, service, request id, key hash and reason is refused with its
/// own token, audited, and changes nothing; the longest valid values approve.
#[tokio::test]
async fn invalid_host_id_role_and_service_are_refused_with_their_own_tokens() {
    let enrollment = Rig::new("refuse-fields");
    let rig = coordinator_over("refuse-fields-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let base = approval(&key, "s1");
    let id = key.key_hash().to_hex();
    let host = |host_id: String| ApproveRequest {
        host_id,
        ..base.clone()
    };
    let role = |role: String| ApproveRequest {
        role,
        ..base.clone()
    };
    let services = |services: &[&str]| ApproveRequest {
        services: services.iter().map(|s| (*s).to_owned()).collect(),
        ..base.clone()
    };
    let cases = [
        (host("a".repeat(64)), EnrollmentRefusal::InvalidHostId),
        (host("-s1".to_owned()), EnrollmentRefusal::InvalidHostId),
        (host("s1-".to_owned()), EnrollmentRefusal::InvalidHostId),
        (host("S1".to_owned()), EnrollmentRefusal::InvalidHostId),
        (host("s_1".to_owned()), EnrollmentRefusal::InvalidHostId),
        (host(String::new()), EnrollmentRefusal::InvalidHostId),
        (role(String::new()), EnrollmentRefusal::InvalidRole),
        (role("1storage".to_owned()), EnrollmentRefusal::InvalidRole),
        (role("Storage".to_owned()), EnrollmentRefusal::InvalidRole),
        (role("storage-".to_owned()), EnrollmentRefusal::InvalidRole),
        (role("a".repeat(33)), EnrollmentRefusal::InvalidRole),
        (services(&["kbf/x"]), EnrollmentRefusal::InvalidService),
        (services(&["-kbf"]), EnrollmentRefusal::InvalidService),
        (services(&["unmanaged"]), EnrollmentRefusal::InvalidService),
        (services(&["kbf", ""]), EnrollmentRefusal::InvalidService),
        (
            ApproveRequest {
                reason: String::new(),
                ..base.clone()
            },
            EnrollmentRefusal::InvalidRequest,
        ),
        (
            ApproveRequest {
                reason: "x".repeat(257),
                ..base.clone()
            },
            EnrollmentRefusal::InvalidRequest,
        ),
        (
            ApproveRequest {
                key_sha256: id.to_ascii_uppercase(),
                ..base.clone()
            },
            EnrollmentRefusal::InvalidRequest,
        ),
    ];
    let before = audit_of(&rig.coordinator).len();
    for (request, refusal) in cases.iter().cloned() {
        let shown = format!("{request:?}");
        assert_eq!(
            rig.coordinator.enrollment_approve(&operator, request),
            Err(refusal),
            "{shown}"
        );
    }
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, refusal)| {
            audited(
                "operator",
                "enroll_approve_refused",
                None,
                &format!("{}; request {id}", token_of(*refusal)),
            )
        })
        .collect();
    assert_eq!(audit_after(&rig.coordinator, before), expected);

    // A request id that is no id is refused, and audited without one.
    let before = audit_of(&rig.coordinator).len();
    for request_id in ["not-hex".to_owned(), id.to_ascii_uppercase(), String::new()] {
        let refused = rig.coordinator.enrollment_approve(
            &operator,
            ApproveRequest {
                request_id,
                ..base.clone()
            },
        );
        assert_eq!(refused, Err(EnrollmentRefusal::InvalidRequest));
    }
    let invalid = audited(
        "operator",
        "enroll_approve_refused",
        None,
        "invalid_request",
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [invalid.clone(), invalid.clone(), invalid]
    );

    let store = enrollment.enrollment.store();
    assert_eq!(
        store
            .request(&key.key_hash())
            .expect("read")
            .expect("held")
            .state,
        RequestState::Pending
    );
    assert!(store.recorded_approvals().expect("read").is_empty());

    // The longest host id, the longest role and reason, and a service named twice approve.
    let longest = ApproveRequest {
        host_id: "a".repeat(63),
        role: format!("a{}", "b".repeat(31)),
        services: vec!["kbf".to_owned(), "kbf".to_owned()],
        reason: "x".repeat(256),
        ..base
    };
    rig.coordinator
        .enrollment_approve(&operator, longest)
        .expect("the longest values approve");
    let recorded = store.recorded_approvals().expect("read");
    assert_eq!(recorded[0].services, ["kbf"]);
    assert_eq!(token_of(EnrollmentRefusal::PendingFull), "pending_full");
}

/// Catches a refusal of the store mapped to another token, or a refused approval that
/// changes anything but the audit: an unknown request, one no longer pending, a denied host
/// id, and a renewal named under another host id.
#[tokio::test]
async fn every_refusal_of_the_store_is_audited_and_changes_nothing() {
    let enrollment = Rig::new("refuse-store");
    let rig = coordinator_over("refuse-store-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let e = &enrollment.enrollment;
    let approve = |request: ApproveRequest| rig.coordinator.enrollment_approve(&operator, request);

    let never = HostKey::new();
    assert_eq!(
        approve(approval(&never, "s9")),
        Err(EnrollmentRefusal::UnknownRequest)
    );

    let twice = HostKey::new();
    e.enroll(enroll_request(&twice, "machine-2"))
        .expect("enrolled");
    approve(approval(&twice, "s2")).expect("approved");
    assert_eq!(
        approve(approval(&twice, "s3")),
        Err(EnrollmentRefusal::NotPending)
    );

    let (old, new) = (HostKey::new(), HostKey::new());
    enrolled(e, &old, "machine-4", "s4");
    e.store().deny(&host_id("s4")).expect("denied");
    e.enroll(enroll_request(&new, "machine-5"))
        .expect("enrolled");
    assert_eq!(
        approve(approval(&new, "s4")),
        Err(EnrollmentRefusal::Denied)
    );

    let renewing = HostKey::new();
    enrolled(e, &renewing, "machine-6", "s6");
    e.enroll(EnrollRequest {
        renewal_of: "s6".to_owned(),
        ..enroll_request(&renewing, "machine-6")
    })
    .expect("a renewal");
    let before = audit_of(&rig.coordinator).len();
    assert_eq!(
        approve(approval(&renewing, "s7")),
        Err(EnrollmentRefusal::InvalidHostId)
    );
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [audited(
            "operator",
            "enroll_approve_refused",
            Some("s7"),
            &format!("invalid_host_id; request {}", renewing.key_hash())
        )]
    );
    let actions: Vec<String> = audit_of(&rig.coordinator)
        .into_iter()
        .map(|(_, action, _, _)| action)
        .filter(|action| action.starts_with("enroll_"))
        .collect();
    assert_eq!(
        actions,
        [
            "enroll_approve_refused",
            "enroll_approve",
            "enroll_approve_refused",
            "enroll_approve_refused",
            "enroll_approve_refused"
        ]
    );
    for host in ["s3", "s7", "s9"] {
        assert_eq!(e.store().host_role(&host_id(host)).expect("read"), None);
    }
    assert!(e.store().is_denied(&host_id("s4")).expect("read"));
}

/// Catches a failure of the store answered as anything but `store_failed`, or leaving an
/// approval behind: the read of the pending request fails, then the store's lock does.
#[tokio::test]
async fn a_store_that_fails_refuses_store_failed_and_approves_nothing() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("refuse-failing", fs.clone(), os_random());
    let rig = coordinator_over("refuse-failing-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let log = Log::default();
    let guard = log.install();
    // The read of the pending request (1), then the store's lock (2).
    for n in [1, 2] {
        fs.fail_at(fs.count() + n);
        assert_eq!(
            rig.coordinator
                .enrollment_approve(&operator, approval(&key, "s1")),
            Err(EnrollmentRefusal::StoreFailed),
            "operation {n}"
        );
    }
    drop(guard);
    assert!(
        log.text().contains("cannot read a pending request"),
        "{}",
        log.text()
    );
    assert!(
        log.text().contains("an approval could not be written"),
        "{}",
        log.text()
    );
    let restarted = enrollment.restart();
    assert!(
        restarted
            .store()
            .recorded_approvals()
            .expect("read")
            .is_empty()
    );
    assert_eq!(
        restarted
            .store()
            .request(&key.key_hash())
            .expect("read")
            .expect("held")
            .state,
        RequestState::Pending
    );
}

/// Catches a start that cannot read the approvals or the audit log auditing every approval
/// again, or not auditing what comes after; and an enrollment no coordinator took audits
/// nothing and still approves.
#[tokio::test]
async fn a_start_that_cannot_read_what_it_would_audit_again_audits_nothing_again() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("audit-unread", fs.clone(), os_random());
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    // No coordinator yet: the approval stands, and has no entry.
    let alone = enrollment
        .enrollment
        .approve("operator", approval(&key, "s1"), &|_| true)
        .expect("approved");
    assert_eq!(alone.entry, None);

    let mut rig = coordinator_over("audit-unread-coordinator", &enrollment).await;
    let log = Log::default();
    let guard = log.install();
    // The list of the requests fails: nothing is audited again.
    fs.fail_at(fs.count() + 1);
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    let audit = rig.dir.path().join("desired.json.ops.audit");
    let before = audit_of(&rig.coordinator);
    // The audit file cannot be read once the ops store is open: nothing is audited again
    // either.
    let blind = crate::testutil::Rig::start(&rig.dir, &rig.clock);
    std::fs::remove_file(&audit).expect("remove");
    std::fs::create_dir(&audit).expect("a directory in its place");
    let blind = blind.with_enrollment(Arc::clone(&enrollment.enrollment));
    drop(guard);
    let text = log.text();
    assert!(
        text.contains("cannot read the recorded approvals"),
        "{text}"
    );
    assert!(text.contains("cannot read the audit log"), "{text}");
    drop(blind);
    std::fs::remove_dir(&audit).expect("remove");
    let approvals = before
        .iter()
        .filter(|(_, action, _, _)| action == "enroll_approve")
        .count();
    assert_eq!(
        approvals, 1,
        "the first start audited the approval once: {before:?}"
    );
}

/// Catches a refusal of the store's own checks mapped to another token when the store, under
/// its lock, finds what the coordinator's read did not (a request gone, another key).
#[test]
fn the_stores_own_refusals_keep_their_tokens() {
    use hostkeeper_enrollment::enroll::{ApproveError, MacNonceRefusal};

    use crate::enrollment::approve::refusal_of;
    for (error, refusal) in [
        (
            ApproveError::UnknownRequest,
            EnrollmentRefusal::UnknownRequest,
        ),
        (ApproveError::KeyMismatch, EnrollmentRefusal::KeyMismatch),
        (
            ApproveError::MacNonce(MacNonceRefusal::Used),
            EnrollmentRefusal::StoreFailed,
        ),
    ] {
        assert_eq!(refusal_of(&error), refusal, "{error}");
    }
}

/// Catches a host's claims put in the audit as they came: every claim made is named, the
/// addresses joined, a character that is not printable ASCII written `?`, and each claim cut
/// at 64 bytes.
#[tokio::test]
async fn an_approvals_audit_quotes_the_claims() {
    use hostkeeper_proto::hostkeeper::agent::v1::Claims;

    let enrollment = Rig::new("audit-claims");
    let rig = coordinator_over("audit-claims-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(EnrollRequest {
            claims: Some(Claims {
                machine_id: "m 1\u{e9}".to_owned(),
                dmi_serial: "D".repeat(70),
                addresses: vec!["192.0.2.10".to_owned(), "2001:db8::10".to_owned()],
                ..Claims::default()
            }),
            ..enroll_request(&key, "unused")
        })
        .expect("enrolled");
    let before = audit_of(&rig.coordinator).len();
    rig.coordinator
        .enrollment_approve(&operator, approval(&key, "s1"))
        .expect("approved");
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            audited(
                "operator",
                "enroll_approve",
                Some("s1"),
                &format!(
                    "request {}; role storage; services kbf; claims machine_id=m?1? \
                     dmi_serial={} addresses=192.0.2.10,2001:db8::10; new rack",
                    key.key_hash(),
                    "D".repeat(64)
                )
            ),
            audited("operator", "assign", Some("s1"), "host s1"),
        ]
    );
}
