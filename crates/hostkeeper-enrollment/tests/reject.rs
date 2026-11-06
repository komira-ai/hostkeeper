//! The operator's rejection of a pending request, over a scratch directory: the request leaves
//! the queue and its key stays refused, a repeat is told apart from the first rejection, a
//! request already decided is not rejected, and a full queue takes a new request once one is
//! rejected.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApproveError, EnrollmentStore, IssueError, PostError, RejectError, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, Op, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use support::{HostKey, NOW, TestCa, decision, host_id, request, scratch};

/// A store over the real file system in its own scratch directory.
fn store(dir: &hostkeeper_testkit::Scratch) -> EnrollmentStore {
    EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open")
}

/// Proves: a rejected request is denied and no longer pending; a repost of its key is the same
/// request, still denied; a poll of it is answered denied; an approval of it is `not_pending`;
/// a second rejection answers the request as rejected without writing (`first` false); and the
/// rejection survives a reopen of the store.
///
/// Catches: a rejection that leaves the request listed or approvable, a key that comes back as
/// a new pending request, and a repeat that cannot be told from the first rejection (which
/// would audit it twice).
#[test]
fn a_rejected_request_leaves_the_queue_and_its_key_stays_refused() {
    let dir = scratch("reject");
    let ca = TestCa::new("reject-ca");
    let s = store(&dir);
    let (key, other) = (HostKey::new(), HostKey::new());
    let id = s.post(request(&key, "m-1"), NOW).expect("post").request_id;
    s.post(request(&other, "m-2"), NOW).expect("post");

    let rejected = s.reject(&id).expect("rejected");
    assert!(rejected.first);
    assert_eq!(
        (rejected.request.request_id, rejected.request.state),
        (id, RequestState::Denied)
    );
    let pending: Vec<KeyHash> = s
        .pending()
        .expect("read")
        .iter()
        .map(|view| view.request_id)
        .collect();
    assert_eq!(pending, [other.key_hash()], "the other request still waits");

    let reposted = s.post(request(&key, "m-1"), NOW).expect("post");
    assert!(reposted.reposted);
    assert_eq!(reposted.state, RequestState::Denied);
    let refused = s.issue(&ca.ca, &id).expect_err("denied");
    assert!(matches!(refused, IssueError::Denied), "{refused}");
    let refused = s
        .approve(&id, &id, &decision("web-1", false))
        .expect_err("not pending");
    assert!(
        matches!(refused, ApproveError::NotPending(RequestState::Denied)),
        "{refused}"
    );

    let again = s.reject(&id).expect("rejected again");
    assert!(!again.first, "a repeat writes nothing");
    assert_eq!(again.request, rejected.request);
    let reopened = store(&dir).reject(&id).expect("rejected after a reopen");
    assert!(!reopened.first);
}

/// Proves: a request flagged `duplicate_claim` is rejected as a pending one is; a request id
/// the store never held is `unknown_request`; an approved request, an issued one, and one denied
/// after its approval (its host retired) are each `not_pending`, and none of them is changed.
///
/// Catches: a rejection that revokes an approved or enrolled host by the back door (that is
/// `Hosts.Retire`'s, with its deny row and slot), a rejection of a retired host's request
/// answered as a repeat, and an unknown id answered as success.
#[test]
fn only_a_waiting_request_is_rejected() {
    let dir = scratch("reject-states");
    let ca = TestCa::new("reject-states-ca");
    let s = store(&dir);
    let first = HostKey::new();
    s.post(request(&first, "m-1"), NOW).expect("post");
    let twin = HostKey::new();
    let posted = s.post(request(&twin, "m-1"), NOW).expect("post");
    assert_eq!(posted.state, RequestState::DuplicateClaim);
    assert!(s.reject(&posted.request_id).expect("rejected").first);

    let unknown = s.reject(&HostKey::new().key_hash()).expect_err("unknown");
    assert!(matches!(unknown, RejectError::UnknownRequest), "{unknown}");
    assert_eq!(unknown.token(), Some("unknown_request"));

    let approved = HostKey::new();
    let id = s
        .post(request(&approved, "m-3"), NOW)
        .expect("post")
        .request_id;
    s.approve(&id, &id, &decision("web-3", false))
        .expect("approve");
    let refused = s.reject(&id).expect_err("approved");
    assert!(
        matches!(refused, RejectError::NotPending(RequestState::Approved)),
        "{refused}"
    );
    assert_eq!(refused.token(), Some("not_pending"));

    s.issue(&ca.ca, &id).expect("issued");
    let refused = s.reject(&id).expect_err("issued");
    assert!(
        matches!(refused, RejectError::NotPending(RequestState::Issued)),
        "{refused}"
    );

    assert!(s.deny(&host_id("web-3")).expect("denied"));
    let refused = s.reject(&id).expect_err("retired");
    assert!(
        matches!(refused, RejectError::NotPending(RequestState::Denied)),
        "{refused}"
    );
    let view = s.request(&id).expect("read").expect("held");
    assert_eq!(view.host_id, Some(host_id("web-3")), "the approval is kept");
}

/// Proves: with the pending queue at its cap, a new request is refused `pending_full`, and once
/// one waiting request is rejected the same new request is taken, pending.
///
/// Catches: a rejected request still counted toward the cap, so the queue only grows.
#[test]
fn a_rejection_makes_room_in_a_full_queue() {
    let dir = scratch("reject-full");
    let s = store(&dir).with_max_pending(2);
    let waiting: Vec<KeyHash> = (0..2)
        .map(|n| {
            s.post(request(&HostKey::new(), &format!("m-{n}")), NOW)
                .expect("post")
                .request_id
        })
        .collect();
    let late = HostKey::new();
    let full = s.post(request(&late, "m-late"), NOW).expect_err("full");
    assert!(matches!(full, PostError::PendingFull), "{full}");

    s.reject(&waiting[0]).expect("rejected");
    let taken = s.post(request(&late, "m-late"), NOW).expect("room");
    assert_eq!(
        (taken.state, taken.reposted),
        (RequestState::Pending, false)
    );
}

/// Proves: `deny` says whether it wrote a new deny row: `true` for the first deny of a host,
/// `false` for a host denied already, retiring or retired, whose row it keeps.
///
/// Catches: a repeat retirement that cannot be told from the first (which would audit it twice
/// and drop assignments again), and a retired row turned back to retiring.
#[test]
fn deny_says_whether_the_row_is_new() {
    let dir = scratch("deny-new");
    let s = store(&dir);
    let web = host_id("web-1");
    assert!(s.deny(&web).expect("denied"));
    assert!(!s.deny(&web).expect("denied again"));
    assert!(s.is_retiring(&web).expect("read"));
    s.retire(&web).expect("retired");
    assert!(!s.deny(&web).expect("denied once retired"));
    assert!(!s.is_retiring(&web).expect("read"), "retired stays retired");
}

/// Proves: a rejection whose request file cannot be written is a store error with no token,
/// and leaves the request pending: asking again rejects it.
///
/// Catches: a failed write answered as a rejection, or a request half-rejected.
#[test]
fn a_rejection_that_cannot_be_written_changes_nothing() {
    let dir = scratch("reject-fault");
    let fs = Arc::new(FaultFs::new());
    let s = EnrollmentStore::open(fs.clone(), dir.path()).expect("open");
    let id = s
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("post")
        .request_id;
    let base = fs.count();
    let probe_dir = scratch("reject-fault-probe");
    let probe_fs = Arc::new(FaultFs::new());
    let probe = EnrollmentStore::open(probe_fs.clone(), probe_dir.path()).expect("open");
    let probe_id = probe
        .post(request(&HostKey::new(), "m-1"), NOW)
        .expect("post")
        .request_id;
    let probe_base = probe_fs.count();
    probe.reject(&probe_id).expect("rejected");
    let ops = probe_fs.ops();
    let skip = usize::try_from(probe_base).expect("small");
    let write = ops[skip..]
        .iter()
        .position(|op| matches!(op, Op::WriteNew(path) if path.parent().is_some_and(|d| d.ends_with("requests"))))
        .expect("the request is written");
    fs.fail_at(base + 1 + u64::try_from(write).expect("small"));

    let failed = s.reject(&id).expect_err("the write fails");
    assert!(
        matches!(failed, RejectError::Store(StoreError::Io { .. })),
        "{failed}"
    );
    assert_eq!(failed.token(), None);
    let view = s.request(&id).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Pending);
    assert!(s.reject(&id).expect("rejected").first);
}

/// Proves: a request that expired undecided can be rejected: it is denied, it leaves the
/// expired list, and a repost of its key, which would make an expired request wait again, is
/// answered denied instead.
///
/// Catches: an expired request answered `not_pending`, so that its host gets back in the queue
/// at its next post however often the operator turns it away.
#[test]
fn an_expired_request_can_be_rejected_and_its_key_stays_out() {
    let dir = scratch("reject-expired");
    let s = store(&dir);
    let key = HostKey::new();
    let id = s.post(request(&key, "m-1"), NOW).expect("post").request_id;
    let ttl = 60;
    let expired = s.expire(NOW + ttl + 1, ttl).expect("expired");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].state, RequestState::Expired);

    let rejected = s.reject(&id).expect("rejected");
    assert!(rejected.first);
    assert_eq!(rejected.request.state, RequestState::Denied);
    assert!(
        s.expired().expect("read").is_empty(),
        "no longer listed expired"
    );
    let reposted = s
        .post(request(&key, "m-1"), NOW + ttl + 2)
        .expect("answered");
    assert_eq!(
        reposted.state,
        RequestState::Denied,
        "it does not wait again"
    );
    assert!(s.pending().expect("read").is_empty());
}
