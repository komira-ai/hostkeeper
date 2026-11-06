//! Expiry of the requests no operator approved within the request TTL, at the coordinator: the
//! reload tick's sweep at the time the enrollment's clock reads, the `request_expired` audit
//! entry (once, and again at a start only when it did not land), an approval refused after
//! expiry, the unenrolled list, and the host's key posted again. No test waits: the tick body
//! ([`reload_all`]) is called with the clock set.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::{FaultFs, Op};
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollState, PollState};
use hostkeeper_proto::hostkeeper::operator::v1::{
    EnrollmentRefusal, EnrollmentRequestState, InventoryMatch, ListPendingRequest,
    PendingEnrollment,
};

use super::support::{
    Audited, HostKey, NOW, Rig, approval, audit_after, audit_of, audited, coordinator_over,
    enroll_request, requested, signed_poll,
};
use crate::enrollment::{DEFAULT_REQUEST_TTL_SECS, os_random};
use crate::listen::reload_all;
use crate::operator::Caller;
use crate::ops::OpsState;
use crate::rollback_tests::unwritable;
use crate::testutil::{Log, OPERATOR};
use crate::tokens::OperatorTokens;

/// The request TTL of these tests: the coordinator's default.
const TTL: u64 = DEFAULT_REQUEST_TTL_SECS;

/// One look of the coordinator's reload tick over `enrollment`, with no TLS file.
async fn tick(enrollment: &Rig) {
    reload_all(
        Some(&enrollment.enrollment),
        &[],
        &OperatorTokens::single(OPERATOR),
    )
    .await;
}

/// The coordinator's `request_expired` entry of `key`'s request, round 0, under `ttl`.
fn expired(key: &HostKey, ttl: u64) -> Audited {
    audited(
        "coordinator",
        "request_expired",
        None,
        &format!(
            "request {}; round 0; no operator approved it within {ttl}s",
            key.key_hash()
        ),
    )
}

/// The `request_expired` entries of the audit log of `rig`.
fn expiries(rig: &crate::testutil::Rig) -> usize {
    audit_of(&rig.coordinator)
        .iter()
        .filter(|entry| entry.1 == "request_expired")
        .count()
}

/// `Enrollment.List` of `rig`'s coordinator, the pending requests or the unenrolled ones.
fn listed(
    rig: &crate::testutil::Rig,
    operator: &Caller,
    unenrolled: bool,
) -> Vec<PendingEnrollment> {
    rig.coordinator
        .enrollment_list(
            operator,
            &ListPendingRequest {
                unenrolled,
                ..ListPendingRequest::default()
            },
        )
        .expect("listed")
        .requests
}

/// Proves the brief's first two cases at the coordinator's periodic point: a request just under
/// the TTL, and exactly at it, stays pending; one second over it, the tick expires it, it is
/// listed unenrolled with its reason, and the audit log holds exactly one `request_expired`
/// entry for it however many ticks follow. The host's poll is still answered `pending`.
///
/// Catches: an off-by-one at the TTL, a sweep that is not run by the tick or reads another
/// clock, an expiry audited at every tick, an expired request still listed pending, and an
/// unenrolled request listed without why.
#[tokio::test]
async fn a_request_over_the_ttl_is_unenrolled_with_one_audit_entry() {
    let enrollment = Rig::new("expiry-tick");
    let rig = coordinator_over("expiry-tick-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    let before = audit_of(&rig.coordinator).len();

    for at in [NOW + TTL - 1, NOW + TTL] {
        enrollment.clock.set(at);
        tick(&enrollment).await;
        assert_eq!(audit_after(&rig.coordinator, before), [], "at {at}");
        let pending = listed(&rig, &operator, false);
        assert_eq!(pending.len(), 1, "at {at}");
        assert_eq!(pending[0].state, EnrollmentRequestState::Pending as i32);
        assert_eq!(listed(&rig, &operator, true), [], "at {at}");
    }

    enrollment.clock.set(NOW + TTL + 1);
    tick(&enrollment).await;
    enrollment.clock.set(NOW + TTL + 100);
    tick(&enrollment).await;
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [expired(&key, TTL)],
        "one entry, at the first tick past the TTL"
    );
    assert_eq!(listed(&rig, &operator, false), []);
    assert_eq!(
        listed(&rig, &operator, true),
        [PendingEnrollment {
            request_id: key.key_hash().to_hex(),
            key_sha256: key.key_hash().to_hex(),
            claims: enroll_request(&key, "machine-1").claims,
            image: "hostkeeper-image-1".to_owned(),
            first_seen_unix: NOW,
            inventory_match: InventoryMatch::Unmatched.into(),
            state: EnrollmentRequestState::Unenrolled.into(),
            unenrolled_reason: "request_expired".to_owned(),
            unenrolled_unix: NOW + TTL + 1,
            request_ttl_secs: TTL,
            ..PendingEnrollment::default()
        }]
    );
    assert_eq!(
        signed_poll(&enrollment.enrollment, &key, &key)
            .expect("polled")
            .state,
        PollState::Pending as i32
    );
}

/// Proves the brief's third case: an operator's approval of an expired request is refused
/// `not_pending`, audited as a refusal, and changes nothing.
///
/// Catches: an expired request approved, so a host whose request lapsed gets a certificate.
#[tokio::test]
async fn an_approval_after_expiry_is_refused() {
    let enrollment = Rig::new("expiry-approve");
    let rig = coordinator_over("expiry-approve-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    enrollment.clock.set(NOW + TTL + 1);
    assert_eq!(enrollment.enrollment.expire_requests(), [key.key_hash()]);
    let before = audit_of(&rig.coordinator).len();

    assert_eq!(
        rig.coordinator
            .enrollment_approve(&operator, approval(&key, "s1"))
            .map(|_| ()),
        Err(EnrollmentRefusal::NotPending)
    );
    let after = audit_after(&rig.coordinator, before);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].1, "enroll_approve_refused");
    assert!(
        after[0]
            .3
            .as_deref()
            .is_some_and(|r| r.contains("not_pending")),
        "{after:?}"
    );
    let view = enrollment
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held");
    assert_eq!((view.state, view.host_id), (RequestState::Expired, None));
    assert_eq!(
        signed_poll(&enrollment.enrollment, &key, &key)
            .expect("polled")
            .state,
        PollState::Pending as i32,
        "no certificate"
    );
}

/// Catches the flag's TTL not reaching the sweep (the default used whatever is configured), and
/// a default other than 72 hours.
#[test]
fn the_sweep_uses_the_configured_ttl() {
    let unconfigured = Rig::new("expiry-default");
    assert_eq!(unconfigured.enrollment.request_ttl_secs(), 72 * 3600);
    let enrollment = Rig::configured(
        "expiry-configured",
        Arc::new(hostkeeper_enrollment::fs::RealFs),
        os_random(),
        |enrollment| enrollment.with_request_ttl(90),
    );
    assert_eq!(enrollment.enrollment.request_ttl_secs(), 90);
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    enrollment.clock.set(NOW + 90);
    assert_eq!(enrollment.enrollment.expire_requests(), []);
    enrollment.clock.set(NOW + 91);
    assert_eq!(enrollment.enrollment.expire_requests(), [key.key_hash()]);
}

/// Proves: the host's key posted again after expiry is a new request (`enroll_requested`),
/// pending from that post, and its second expiry is audited too: one `request_expired` per
/// expiry, not one per request.
///
/// Catches: a key kept out for good once its request expired, a re-post answered as the expired
/// request, and the second expiry of one request id taken as already audited.
#[tokio::test]
async fn a_key_posted_again_after_expiry_is_a_new_request() {
    let enrollment = Rig::new("expiry-repost");
    let rig = coordinator_over("expiry-repost-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let key = HostKey::new();
    let e = &enrollment.enrollment;
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    let before = audit_of(&rig.coordinator).len();
    enrollment.clock.set(NOW + TTL + 1);
    tick(&enrollment).await;

    enrollment.clock.set(NOW + TTL + 5);
    let again = e
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested again");
    assert_eq!(again.state, EnrollState::Pending as i32);
    let pending = listed(&rig, &operator, false);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].first_seen_unix, NOW + TTL + 5);
    assert_eq!(listed(&rig, &operator, true), []);

    enrollment.clock.set(NOW + 2 * TTL + 6);
    tick(&enrollment).await;
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            expired(&key, TTL),
            requested(&key, None, "pending"),
            expired(&key, TTL)
        ]
    );
}

/// Proves: an expiry whose entry did not land (no coordinator took the enrollment yet, or the
/// audit write failed) is audited at the next start, once; an expiry whose entry is in the log is
/// not audited again at later starts; and a request posted again after its audited expiry is not
/// taken for an unaudited expiry.
///
/// Catches: an expiry lost from the audit by a stop or a failed write, and an expiry audited
/// again at every start.
#[tokio::test]
async fn an_expiry_whose_entry_did_not_land_is_audited_at_the_next_start() {
    let enrollment = Rig::new("expiry-restart");
    let key = HostKey::new();
    let e = &enrollment.enrollment;
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    // No coordinator holds the enrollment: the expiry has no entry.
    enrollment.clock.set(NOW + TTL + 1);
    assert_eq!(e.expire_requests(), [key.key_hash()]);

    let mut rig = coordinator_over("expiry-restart-coordinator", &enrollment).await;
    assert_eq!(expiries(&rig), 1, "audited when the coordinator took it");
    let start = |rig: &mut crate::testutil::Rig| {
        rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
            .with_enrollment(Arc::clone(&enrollment.enrollment));
    };
    start(&mut rig);
    assert_eq!(expiries(&rig), 1, "not again at a later start");

    enrollment.clock.set(NOW + TTL + 2);
    e.enroll(enroll_request(&key, "machine-1"))
        .expect("requested again");
    start(&mut rig);
    assert_eq!(
        expiries(&rig),
        1,
        "a request waiting again has no expiry to audit"
    );

    // The second expiry's audit write fails.
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_ops(unwritable(OpsState::default()))
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    enrollment.clock.set(NOW + 2 * TTL + 3);
    assert_eq!(e.expire_requests(), [key.key_hash()]);
    start(&mut rig);
    assert_eq!(expiries(&rig), 2, "the entry that did not land is written");
    start(&mut rig);
    assert_eq!(expiries(&rig), 2);
}

/// Catches an expiry audited again at every start once the audit file's retention deleted the
/// segment its entry was in, and one never audited when its entry is merely missing: with
/// segment 1 deleted, an expiry before the oldest line kept is not audited at the next start,
/// and one at or after it is.
#[tokio::test]
async fn an_expiry_older_than_the_audit_kept_is_not_audited_again() {
    let enrollment = Rig::new("expiry-pruned");
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    enrollment.clock.set(NOW + TTL + 1);
    assert_eq!(enrollment.enrollment.expire_requests(), [key.key_hash()]);
    let mut rig = crate::testutil::Rig::new("expiry-pruned-coordinator");
    let audit = crate::audit_file::path_of(&rig.dir.path().join("desired.json.ops"));
    let kept_from = |at_unix: u64| {
        let line = format!(
            r#"{{"seq":40,"at_unix":{at_unix},"by":"operator","action":"hold","detail":{{}}}}"#
        );
        std::fs::write(
            crate::audit_file::segment_path(&audit, 2),
            format!("{line}\n"),
        )
        .expect("a segment past the first");
    };
    let start = |rig: &mut crate::testutil::Rig| {
        rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
            .with_enrollment(Arc::clone(&enrollment.enrollment));
    };

    kept_from(NOW + TTL + 2);
    start(&mut rig);
    assert_eq!(expiries(&rig), 0, "its entry went with a deleted segment");

    kept_from(NOW + TTL + 1);
    start(&mut rig);
    assert_eq!(
        expiries(&rig),
        1,
        "expired at the oldest line kept: audited"
    );
}

/// Catches a sweep or a start that cannot read the store failing silently or panicking: each
/// logs at error level and expires or audits nothing, and the next sweep expires the request.
#[tokio::test]
async fn a_store_that_cannot_be_read_is_logged_and_the_next_sweep_goes_on() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("expiry-unread", fs.clone(), os_random());
    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("requested");
    enrollment.clock.set(NOW + TTL + 1);
    let mut probe = crate::testutil::Rig::new("expiry-unread-probe");
    let mut rig = crate::testutil::Rig::new("expiry-unread-coordinator");
    let log = Log::default();
    let guard = log.install();
    fs.fail_at(fs.count() + 1);
    assert_eq!(enrollment.enrollment.expire_requests(), []);
    assert_eq!(enrollment.enrollment.expire_requests(), [key.key_hash()]);

    // At a start, the expired requests are the second list of the requests (the recorded
    // approvals are the first): found in a start that fails nothing, then failed in the next.
    let start = |rig: &mut crate::testutil::Rig| {
        rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
            .with_enrollment(Arc::clone(&enrollment.enrollment));
    };
    let base = fs.count();
    start(&mut probe);
    assert_eq!(expiries(&probe), 1);
    let lists: Vec<u64> = (base + 1..)
        .zip(&fs.ops()[usize::try_from(base).expect("small")..])
        .filter(|(_, op)| matches!(op, Op::List(dir) if dir.ends_with("requests")))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(lists.len(), 2, "{lists:?}");
    fs.fail_at(fs.count() + (lists[1] - base));
    start(&mut rig);
    drop(guard);
    let text = log.text();
    assert!(
        text.contains("cannot expire the enrollment requests"),
        "{text}"
    );
    assert!(
        text.contains("cannot read the expired requests: none is audited again"),
        "{text}"
    );
    assert_eq!(expiries(&rig), 0);
}

/// Catches the unenrolled list out of the order of the pending one (first seen, then request
/// id), a page that repeats or skips an expired request, and a pending request listed with the
/// expired ones or an expired one with the pending.
#[tokio::test]
async fn the_unenrolled_list_pages_in_order_and_holds_only_expired_requests() {
    let enrollment = Rig::new("expiry-pages");
    let rig = coordinator_over("expiry-pages-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let e = &enrollment.enrollment;
    let mut expired: Vec<(u64, String)> = Vec::new();
    for (n, offset) in [1, 0, 1].into_iter().enumerate() {
        let key = HostKey::new();
        enrollment.clock.set(NOW + offset);
        e.enroll(enroll_request(&key, &format!("machine-{n}")))
            .expect("requested");
        expired.push((NOW + offset, key.key_hash().to_hex()));
    }
    expired.sort();
    enrollment.clock.set(NOW + TTL + 2);
    let waiting = HostKey::new();
    e.enroll(enroll_request(&waiting, "machine-9"))
        .expect("requested");
    tick(&enrollment).await;

    let mut seen = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = rig
            .coordinator
            .enrollment_list(
                &operator,
                &ListPendingRequest {
                    limit: 2,
                    page_token: page_token.clone(),
                    unenrolled: true,
                },
            )
            .expect("a page");
        assert!(page.requests.len() <= 2);
        seen.extend(
            page.requests
                .iter()
                .map(|r| (r.first_seen_unix, r.request_id.clone(), r.state)),
        );
        page_token = page.next_page_token;
        if page_token.is_empty() {
            break;
        }
    }
    let unenrolled = EnrollmentRequestState::Unenrolled as i32;
    let want: Vec<(u64, String, i32)> = expired
        .into_iter()
        .map(|(at, id)| (at, id, unenrolled))
        .collect();
    assert_eq!(seen, want);
    let pending = listed(&rig, &operator, false);
    assert_eq!(
        pending
            .iter()
            .map(|r| r.request_id.clone())
            .collect::<Vec<_>>(),
        [waiting.key_hash().to_hex()]
    );
}
