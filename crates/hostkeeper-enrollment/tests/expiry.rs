//! Expiry of a request no operator approved in time ([`EnrollmentStore::expire`]): which
//! requests expire and when, what an expired request still allows, and what its file looks like
//! to a release that does not know expiry. Times are the `now` each call is given; nothing here
//! reads a clock.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, EnrollmentStore, Expiry, IssueError, PostError, RequestState,
    RequestView,
};
use hostkeeper_enrollment::fs::{FaultFs, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use support::{HostKey, NOW, TestCa, decision, renewal, request, scratch};

/// The request TTL of these tests: the coordinator's default, 72 hours.
const TTL: u64 = 72 * 3600;

fn open(dir: &hostkeeper_testkit::Scratch) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open")
}

fn post(store: &EnrollmentStore, key: &HostKey, machine_id: &str, at: u64) -> KeyHash {
    store
        .post(request(key, machine_id), at)
        .expect("post")
        .request_id
}

fn ids(views: &[RequestView]) -> Vec<KeyHash> {
    views.iter().map(|view| view.request_id).collect()
}

/// Catches an off-by-one at the TTL in either direction, a sweep that expires a request twice
/// (and so would be audited twice), and an expired request still listed as waiting or still
/// counted as pending.
#[test]
fn a_request_expires_once_when_it_has_waited_longer_than_the_ttl() {
    let dir = scratch("expiry-ttl");
    let store = open(&dir);
    let id = post(&store, &HostKey::new(), "m-1", NOW);

    for at in [NOW, NOW + TTL - 1, NOW + TTL] {
        assert_eq!(store.expire(at, TTL).expect("sweep"), [], "at {at}");
        assert_eq!(ids(&store.pending().expect("pending")), [id], "at {at}");
    }
    assert_eq!(store.expired().expect("expired"), []);

    let swept = store.expire(NOW + TTL + 1, TTL).expect("sweep");
    let expiry = Some(Expiry {
        expired_unix: NOW + TTL + 1,
        ttl_secs: TTL,
    });
    assert_eq!(ids(&swept), [id]);
    assert_eq!(
        (swept[0].state, swept[0].expiry),
        (RequestState::Expired, expiry)
    );
    assert_eq!(swept[0].posted_unix, NOW, "the time it was posted is kept");
    assert_eq!(store.pending().expect("pending"), []);
    assert_eq!(store.expired().expect("expired"), swept);
    assert_eq!(store.request(&id).expect("read"), Some(swept[0].clone()));

    // A later sweep, even under a shorter TTL, finds nothing waiting: it expires nothing again.
    assert_eq!(store.expire(NOW + 10 * TTL, 1).expect("sweep"), []);
    // The store read again from its files holds the same.
    let reopened = open(&dir);
    assert_eq!(reopened.expired().expect("expired"), swept);
}

/// Catches a sweep that skips a duplicate claim (it waits for an operator like any request),
/// expires what an operator already decided, or expires a request posted after the cut.
#[test]
fn only_waiting_requests_older_than_the_ttl_expire() {
    let ca = TestCa::new("expiry-states-ca");
    let dir = scratch("expiry-states");
    let store = open(&dir);
    let waiting = post(&store, &HostKey::new(), "m-1", NOW);
    let duplicate = post(&store, &HostKey::new(), "m-1", NOW);
    assert_eq!(
        store
            .request(&duplicate)
            .expect("read")
            .expect("held")
            .state,
        RequestState::DuplicateClaim
    );
    let approved = post(&store, &HostKey::new(), "m-2", NOW);
    store
        .approve(&approved, &approved, &decision("web-2", false))
        .expect("approve");
    let issued = post(&store, &HostKey::new(), "m-3", NOW);
    store
        .approve(&issued, &issued, &decision("web-3", false))
        .expect("approve");
    store.issue(&ca.ca, &issued).expect("issue");
    let denied = post(&store, &HostKey::new(), "m-4", NOW);
    store
        .approve(&denied, &denied, &decision("web-4", false))
        .expect("approve");
    store.deny(&support::host_id("web-4")).expect("deny");
    let young = post(&store, &HostKey::new(), "m-5", NOW + 2);

    let mut expected = vec![waiting, duplicate];
    expected.sort();
    let swept = store.expire(NOW + TTL + 1, TTL).expect("sweep");
    assert_eq!(ids(&swept), expected);
    assert!(swept.iter().all(|view| view.state == RequestState::Expired));
    assert_eq!(ids(&store.pending().expect("pending")), [young]);
    let states: Vec<RequestState> = [approved, issued, denied]
        .iter()
        .map(|id| store.request(id).expect("read").expect("held").state)
        .collect();
    assert_eq!(
        states,
        [
            RequestState::Approved,
            RequestState::Issued,
            RequestState::Denied
        ]
    );
}

/// Catches an approval of an expired request, by an operator or with a record, and a poll that
/// issues it or fails on it instead of answering that it is not approved.
#[test]
fn an_expired_request_is_neither_approved_nor_issued() {
    let ca = TestCa::new("expiry-approve-ca");
    let dir = scratch("expiry-approve");
    let store = open(&dir);
    let id = post(&store, &HostKey::new(), "m-1", NOW);
    store.expire(NOW + TTL + 1, TTL).expect("sweep");
    let before = store.request(&id).expect("read");

    let plain = store
        .approve(&id, &id, &decision("web-1", false))
        .expect_err("refused");
    assert!(
        matches!(plain, ApproveError::NotPending(RequestState::Expired)),
        "{plain}"
    );
    assert_eq!(plain.token(), Some("not_pending"));
    let record = ApprovalRecord {
        approver: "operator".to_owned(),
        approved_unix: NOW + TTL + 2,
        reason: "late".to_owned(),
        mac_nonce: None,
    };
    let recorded = store
        .approve_recorded(&id, &id, &decision("web-1", true), &record)
        .expect_err("refused");
    assert!(
        matches!(recorded, ApproveError::NotPending(RequestState::Expired)),
        "{recorded}"
    );
    assert_eq!(store.request(&id).expect("read"), before, "nothing written");
    assert_eq!(store.recorded_approvals().expect("approvals"), []);

    let polled = store
        .issue(&ca.ca, &id)
        .map(|_| ())
        .expect_err("not issued");
    assert!(
        matches!(polled, IssueError::NotApproved(RequestState::Expired)),
        "{polled}"
    );
    assert_eq!(
        polled.token(),
        None,
        "the poll answers pending, not a refusal"
    );
}

/// Catches an expired request that still holds its place in the pending queue, a key whose
/// request expired kept out for good (or answered as the old request), and the expired
/// request's machine id flagging the host's next request as a duplicate claim.
#[test]
fn a_key_posted_again_after_expiry_waits_again_from_that_post() {
    let dir = scratch("expiry-repost");
    let store = open(&dir).with_max_pending(1);
    let key = HostKey::new();
    let id = post(&store, &key, "m-1", NOW);
    store.expire(NOW + TTL + 1, TTL).expect("sweep");

    // Its slot is free: another host's request is taken, with the expired one's machine id.
    let other = HostKey::new();
    let posted = store
        .post(request(&other, "m-1"), NOW + TTL + 2)
        .expect("a slot is free");
    assert_eq!(posted.state, RequestState::Pending, "not a duplicate claim");
    // That one now holds the one slot.
    let full = store
        .post(request(&key, "m-1"), NOW + TTL + 3)
        .expect_err("full");
    assert!(matches!(full, PostError::PendingFull), "{full}");
    assert_eq!(
        store.request(&id).expect("read").expect("held").state,
        RequestState::Expired,
        "a refused post changes nothing"
    );

    let store = store.with_max_pending(2);
    let again = store
        .post(request(&key, "m-1"), NOW + TTL + 4)
        .expect("posted again");
    assert_eq!(
        (again.request_id, again.state, again.round, again.reposted),
        (id, RequestState::DuplicateClaim, 0, false),
        "a new request of the same round, checked as one: the other host waits with m-1"
    );
    let view = store.request(&id).expect("read").expect("held");
    assert_eq!((view.posted_unix, view.expiry), (NOW + TTL + 4, None));
    // It waits a whole TTL from that post; the other host's request, posted earlier, goes first.
    assert_eq!(
        ids(&store.expire(NOW + 2 * TTL + 3, TTL).expect("sweep")),
        [posted.request_id]
    );
    assert_eq!(store.expire(NOW + 2 * TTL + 4, TTL).expect("sweep"), []);
    assert_eq!(
        ids(&store.expire(NOW + 2 * TTL + 5, TTL).expect("sweep")),
        [id]
    );
}

/// Catches a renewal that expires into a broken host (its host row names the request's key)
/// or comes back as a new round.
#[test]
fn an_expired_renewal_keeps_its_host_and_round() {
    let ca = TestCa::new("expiry-renewal-ca");
    let dir = scratch("expiry-renewal");
    let store = open(&dir);
    let key = HostKey::new();
    let id = post(&store, &key, "m-1", NOW);
    store
        .approve(&id, &id, &decision("web-1", false))
        .expect("approve");
    store.issue(&ca.ca, &id).expect("issue");
    let web = support::host_id("web-1");
    let renewing = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 10)
        .expect("renewal");
    assert_eq!((renewing.request_id, renewing.round), (id, 1));

    let swept = store.expire(NOW + 10 + TTL + 1, TTL).expect("sweep");
    assert_eq!(ids(&swept), [id]);
    assert_eq!(
        (swept[0].round, swept[0].renewal_of.clone()),
        (1, Some(web.clone()))
    );
    assert!(store.renew_allowed(&web, &id).expect("host"));
    assert_eq!(store.bound_macs().expect("macs").len(), 0);

    let again = store
        .post(renewal(&key, "m-1", "web-1"), NOW + 10 + TTL + 2)
        .expect("renewal again");
    assert_eq!(
        (again.state, again.round, again.reposted),
        (RequestState::Pending, 1, false)
    );
}

/// The value of field `number` of the protobuf message `bytes`, when it is a varint: what a
/// reader that knows only that field sees. Lengths and fixed-width fields are skipped.
fn varint_field(bytes: &[u8], number: u64) -> Option<u64> {
    let mut at = 0;
    let varint = |at: &mut usize| {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = bytes[*at];
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        value
    };
    let mut found = None;
    while at < bytes.len() {
        let tag = varint(&mut at);
        match tag & 7 {
            0 => {
                let value = varint(&mut at);
                if tag >> 3 == number {
                    found = Some(value);
                }
            }
            2 => {
                let length = usize::try_from(varint(&mut at)).expect("small");
                at += length;
            }
            wire => panic!("wire type {wire} is not in a request file"),
        }
    }
    found
}

/// Catches an expired request written in a state of its own, which a release before expiry
/// refuses (`UnknownState`), making the whole store unreadable after a downgrade: its file
/// keeps the waiting state, and only adds the two new fields.
#[test]
fn an_expired_request_file_reads_as_waiting_to_a_release_before_expiry() {
    let dir = scratch("expiry-file");
    let store = open(&dir);
    let waiting = post(&store, &HostKey::new(), "m-1", NOW);
    let duplicate = post(&store, &HostKey::new(), "m-1", NOW);
    store.expire(NOW + TTL + 1, TTL).expect("sweep");
    for (id, state) in [(waiting, 1), (duplicate, 2)] {
        let path = dir.path().join("requests").join(format!("{id}.pb"));
        let bytes = std::fs::read(&path).expect("the request file");
        assert_eq!(varint_field(&bytes, 8), Some(state), "the stored state");
        assert_eq!(
            varint_field(&bytes, 14),
            Some(NOW + TTL + 1),
            "expired_unix"
        );
        assert_eq!(varint_field(&bytes, 15), Some(TTL), "expired_after_secs");
    }
}

/// Catches a sweep that fails silently or leaves a request half expired: a sweep stopped at any
/// operation of the file system reports the failure, and the request is then either waiting or
/// expired, and expired exactly once by the sweeps after.
#[test]
fn a_sweep_that_fails_is_an_error_and_the_next_one_finishes_it() {
    let probe_dir = scratch("expiry-fail-probe");
    let probe_fs = Arc::new(FaultFs::new());
    let probe = EnrollmentStore::open(probe_fs.clone(), probe_dir.path()).expect("open");
    post(&probe, &HostKey::new(), "m-1", NOW);
    let base = probe_fs.count();
    assert_eq!(probe.expire(NOW + TTL + 1, TTL).expect("sweep").len(), 1);
    let total = probe_fs.count() - base;
    assert!(total > 0);
    for n in 1..=total {
        let dir = scratch("expiry-fail-run");
        let fs = Arc::new(FaultFs::new());
        let store = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
        let id = post(&store, &HostKey::new(), "m-1", NOW);
        fs.fail_at(fs.count() + n);
        let failed = store.expire(NOW + TTL + 1, TTL).expect_err("fails");
        assert!(
            failed.to_string().contains("injected failure at operation"),
            "operation {n}: {failed}"
        );
        let state = open(&dir).request(&id).expect("read").expect("held").state;
        assert!(
            matches!(state, RequestState::Pending | RequestState::Expired),
            "operation {n}: {state:?}"
        );
        let finished = open(&dir).expire(NOW + TTL + 2, TTL).expect("sweep");
        let expected: usize = (state == RequestState::Pending).into();
        assert_eq!(finished.len(), expected, "operation {n}");
        assert_eq!(ids(&open(&dir).expired().expect("expired")), [id]);
    }
}
