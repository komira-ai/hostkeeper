//! The enrollment store: posting, approval, issuance, polling, renewal, duplicates and deny
//! rows, over a scratch directory with the real file system.

mod support;

use std::path::Path;
use std::sync::{Arc, Barrier};

use hostkeeper_enrollment::ca::{HOST_VALIDITY_SECS, ProfileError, Signer};
use hostkeeper_enrollment::enroll::{
    ApproveError, EnrollmentStore, IssueError, LIVE_WINDOW_SECS, MAX_NONCE_BYTES, MIN_NONCE_BYTES,
    NewRequest, POLL_DOMAIN, PollError, PostError, RequestState, StoreError, poll_message,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{KeyHash, Role, ServiceName};
use support::{
    HostKey, NOW, TestCa, capture, claims, decision, event, host_id, renewal, request, scratch,
};

/// A store in its own scratch directory, with a CA.
struct Fixture {
    ca: TestCa,
    store: EnrollmentStore,
    dir: hostkeeper_testkit::Scratch,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = scratch(name);
        let store = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open");
        Fixture {
            ca: TestCa::new(&format!("{name}-ca")),
            store,
            dir,
        }
    }

    fn reopen(&self) -> EnrollmentStore {
        EnrollmentStore::open(Arc::new(RealFs), self.dir.path()).expect("reopen")
    }

    /// Posts, approves as `host` and issues the enrollment of `key`.
    fn enroll(&self, key: &HostKey, machine_id: &str, host: &str) -> Vec<u8> {
        let posted = self
            .store
            .post(request(key, machine_id), NOW)
            .expect("post");
        let id = posted.request_id;
        self.store
            .approve(&id, &id, &decision(host, false))
            .expect("approve");
        self.store
            .issue(&self.ca.ca, &id)
            .expect("issue")
            .certificate_der
    }

    fn files_in(&self, sub: &str) -> usize {
        std::fs::read_dir(self.dir.path().join(sub))
            .expect("list")
            .count()
    }
}

/// Every file under `dir`, with its bytes, sorted by path.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for sub in ["requests", "issued", "hosts", "deny"] {
        for entry in std::fs::read_dir(dir.join(sub)).expect("list") {
            let path = entry.expect("entry").path();
            out.push((
                path.display().to_string(),
                std::fs::read(&path).expect("read"),
            ));
        }
    }
    out.sort();
    out
}

/// Catches a re-post that makes a second row or a second place in the queue, and a request id
/// that is not the SHA-256 of the CSR's key (the library takes no id from the caller: the
/// request type has no field for one).
#[test]
fn a_reposted_request_is_the_same_request() {
    let f = Fixture::new("repost");
    let key = HostKey::new();
    let first = f.store.post(request(&key, "m-1"), NOW).expect("post");
    let again = f.store.post(request(&key, "m-1"), NOW + 30).expect("post");
    assert_eq!(first.request_id, key.key_hash());
    assert_eq!(first.request_id, again.request_id);
    assert_eq!(
        (first.state, first.reposted, again.state, again.reposted),
        (RequestState::Pending, false, RequestState::Pending, true)
    );
    assert_eq!(f.files_in("requests"), 1);
    assert_eq!(f.store.pending().expect("pending").len(), 1);
    let view = f
        .store
        .request(&first.request_id)
        .expect("read")
        .expect("held");
    assert_eq!(view.posted_unix, NOW, "the first post is kept");
    assert_eq!(view.claims, claims("m-1"));
    assert_eq!(view.image, "hostkeeper-image-1");
    assert_eq!((view.renewal_of, view.round, view.host_id), (None, 0, None));
    // A CSR that is not in profile is refused before anything is written.
    let refused = f
        .store
        .post(
            NewRequest {
                csr_der: b"no".to_vec(),
                ..request(&key, "m-1")
            },
            NOW,
        )
        .expect_err("bad_csr");
    assert!(matches!(
        refused,
        PostError::Profile(ProfileError::BadCsr(_))
    ));
    assert_eq!(refused.token(), Some("bad_csr"));
    assert_eq!(f.files_in("requests"), 1);
    // Requests without a machine id are never duplicates of one another.
    let a = f
        .store
        .post(request(&HostKey::new(), ""), NOW)
        .expect("post");
    let b = f
        .store
        .post(request(&HostKey::new(), ""), NOW)
        .expect("post");
    assert_eq!(
        (a.state, b.state),
        (RequestState::Pending, RequestState::Pending)
    );
    assert_eq!(f.store.pending().expect("pending").len(), 3);
    let unknown = KeyHash::of_spki(b"no such key");
    assert_eq!(f.store.request(&unknown).expect("read"), None);
}

/// Catches a second request with another request's machine id, or the same key coming back
/// with another machine id, going unflagged or unalerted, and a flagged request being issued
/// before an operator approves it.
#[test]
fn a_second_request_with_the_same_machine_id_or_key_is_duplicate_claim() {
    let f = Fixture::new("duplicate-claim");
    let first = HostKey::new();
    let second = HostKey::new();
    f.store.post(request(&first, "m-1"), NOW).expect("post");
    let (posted, events) = capture(|| f.store.post(request(&second, "m-1"), NOW));
    let posted = posted.expect("post");
    assert_eq!(
        (posted.state, posted.reposted),
        (RequestState::DuplicateClaim, false)
    );
    let id = posted.request_id.to_string();
    assert_eq!(
        events,
        [event(
            "WARN",
            &[
                ("message", "duplicate enrollment claim"),
                ("request_id", id.as_str()),
                ("machine_id", "m-1"),
                ("reason", "another request has this machine id"),
            ]
        )]
    );
    let refused = f
        .store
        .issue(&f.ca.ca, &posted.request_id)
        .expect_err("not approved");
    assert!(matches!(
        refused,
        IssueError::NotApproved(RequestState::DuplicateClaim)
    ));
    assert_eq!(refused.token(), None);

    // The same key, posted again from another machine.
    let third = HostKey::new();
    f.store.post(request(&third, "m-3"), NOW).expect("post");
    let (reposted, events) = capture(|| f.store.post(request(&third, "m-4"), NOW));
    let reposted = reposted.expect("post");
    assert_eq!(
        (reposted.state, reposted.reposted),
        (RequestState::DuplicateClaim, true)
    );
    let id = reposted.request_id.to_string();
    assert_eq!(
        events,
        [event(
            "WARN",
            &[
                ("message", "duplicate enrollment claim"),
                ("request_id", id.as_str()),
                ("machine_id", "m-4"),
                ("reason", "the same key came back with another machine id"),
            ]
        )]
    );
    assert_eq!(f.files_in("requests"), 3);
    assert_eq!(f.store.pending().expect("pending").len(), 3);
    // An explicit approval is the one way out.
    f.store
        .approve(
            &reposted.request_id,
            &reposted.request_id,
            &decision("web-3", false),
        )
        .expect("an operator may approve a duplicate claim");
    // An issued key coming back from another machine is alerted, and stays issued.
    f.store
        .issue(&f.ca.ca, &reposted.request_id)
        .expect("issue");
    let (late, events) = capture(|| f.store.post(request(&third, "m-5"), NOW));
    assert_eq!(late.expect("post").state, RequestState::Issued);
    assert_eq!(events.len(), 1);
}

/// Catches an approval that a key other than the request's can ride, and a refused approval
/// that writes anything.
#[test]
fn approval_for_a_different_key_is_refused() {
    let f = Fixture::new("key-mismatch");
    let key = HostKey::new();
    let posted = f.store.post(request(&key, "m-1"), NOW).expect("post");
    let before = snapshot(f.dir.path());
    let other = HostKey::new().key_hash();
    let refused = f
        .store
        .approve(&posted.request_id, &other, &decision("web-1", false))
        .expect_err("another key");
    assert!(matches!(refused, ApproveError::KeyMismatch));
    assert_eq!(refused.token(), Some("key_mismatch"));
    assert_eq!(
        refused.to_string(),
        "key_mismatch: the key hash is not the request's"
    );
    assert_eq!(snapshot(f.dir.path()), before);
    let unknown = f
        .store
        .approve(&other, &other, &decision("web-1", false))
        .expect_err("no such request");
    assert!(matches!(unknown, ApproveError::UnknownRequest));
    assert_eq!(unknown.token(), Some("unknown_request"));
    assert_eq!(snapshot(f.dir.path()), before);
    let approval = f
        .store
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect("the request's key");
    assert_eq!(approval.host_id, host_id("web-1"));
    assert_eq!(approval.role, Role::parse("web").expect("role"));
    assert_eq!(approval.services, [ServiceName::parse("api").expect("svc")]);
    assert!(!approval.renewal && !approval.cleared_deny);
    let twice = f
        .store
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &decision("web-1", false),
        )
        .expect_err("approved already");
    assert!(matches!(
        twice,
        ApproveError::NotPending(RequestState::Approved)
    ));
    assert_eq!(twice.token(), Some("not_pending"));
    let view = f
        .store
        .request(&posted.request_id)
        .expect("read")
        .expect("held");
    assert_eq!(view.host_id, Some(host_id("web-1")));
}

/// Catches two approvals racing for one host id both succeeding, and a `replace` that leaves
/// the first key able to renew.
#[test]
fn two_approvals_for_one_host_id_admit_one() {
    let f = Fixture::new("one-host-id");
    let keys = [HostKey::new(), HostKey::new()];
    let ids: Vec<KeyHash> = keys
        .iter()
        .enumerate()
        .map(|(n, key)| {
            f.store
                .post(request(key, &format!("m-{n}")), NOW)
                .expect("post")
                .request_id
        })
        .collect();
    let store = &f.store;
    let barrier = Barrier::new(2);
    let results: Vec<Result<_, ApproveError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = ids
            .iter()
            .map(|id| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    store.approve(id, id, &decision("web-1", false))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("no panic"))
            .collect()
    });
    let admitted: Vec<usize> = (0..2).filter(|n| results[*n].is_ok()).collect();
    assert_eq!(admitted.len(), 1, "{results:?}");
    let winner = admitted[0];
    let loser = 1 - winner;
    let refused = results[loser].as_ref().expect_err("taken");
    assert!(matches!(refused, ApproveError::HostIdTaken), "{refused}");
    assert_eq!(refused.token(), Some("host_id_taken"));
    assert!(
        f.store
            .renew_allowed(&host_id("web-1"), &ids[winner])
            .expect("read")
    );

    let replaced = f
        .store
        .approve(&ids[loser], &ids[loser], &decision("web-1", true))
        .expect("replace");
    assert!(!replaced.cleared_deny);
    let web = host_id("web-1");
    assert!(!f.store.renew_allowed(&web, &ids[winner]).expect("read"));
    assert!(f.store.renew_allowed(&web, &ids[loser]).expect("read"));
    assert!(
        !f.store
            .renew_allowed(&host_id("web-9"), &ids[loser])
            .expect("read")
    );
}

/// Catches a poll message that leaves out the domain, a separator or the request id, a
/// signature by another key being accepted, and a nonce outside 16 to 64 bytes being read.
#[test]
fn a_poll_signature_is_checked_over_the_documented_message_and_nothing_else() {
    let f = Fixture::new("poll");
    let key = HostKey::new();
    let id = f
        .store
        .post(request(&key, "m-1"), NOW)
        .expect("post")
        .request_id;
    let other_id = f
        .store
        .post(request(&HostKey::new(), "m-2"), NOW)
        .expect("post")
        .request_id;
    let nonce = [7u8; 32];
    let message = poll_message(&id, &nonce);
    let mut documented = POLL_DOMAIN.to_vec();
    documented.push(0);
    documented.extend_from_slice(id.to_hex().as_bytes());
    documented.push(0);
    documented.extend_from_slice(&nonce);
    assert_eq!(message, documented);
    assert!(message.starts_with(b"hostkeeper-enroll-poll-v1\0"));
    f.store
        .verify_poll(&id, &nonce, &key.sign(&message))
        .expect("the documented message");

    let mut no_separators = POLL_DOMAIN.to_vec();
    no_separators.extend_from_slice(id.to_hex().as_bytes());
    no_separators.extend_from_slice(&nonce);
    let mut no_domain = vec![0];
    no_domain.extend_from_slice(id.to_hex().as_bytes());
    no_domain.push(0);
    no_domain.extend_from_slice(&nonce);
    let refused = [
        key.sign(&nonce),
        key.sign(&poll_message(&other_id, &nonce)),
        key.sign(&no_separators),
        key.sign(&no_domain),
        HostKey::new().sign(&message),
        b"not a signature".to_vec(),
    ];
    for signature in refused {
        let err = f
            .store
            .verify_poll(&id, &nonce, &signature)
            .expect_err("refused");
        assert!(matches!(err, PollError::BadSignature), "{err}");
        assert_eq!(err.token(), Some("bad_signature"));
    }
    for len in [MIN_NONCE_BYTES - 1, MAX_NONCE_BYTES + 1] {
        let nonce = vec![1u8; len];
        let signature = key.sign(&poll_message(&id, &nonce));
        let err = f
            .store
            .verify_poll(&id, &nonce, &signature)
            .expect_err("nonce length");
        assert!(matches!(err, PollError::BadNonce(n) if n == len), "{err}");
        assert_eq!(err.token(), Some("bad_signature"));
    }
    assert_eq!(
        PollError::BadNonce(15).to_string(),
        "bad_signature: the nonce is 15 bytes, not 16 to 64"
    );
    for len in [MIN_NONCE_BYTES, MAX_NONCE_BYTES] {
        let nonce = vec![2u8; len];
        let signature = key.sign(&poll_message(&id, &nonce));
        f.store
            .verify_poll(&id, &nonce, &signature)
            .expect("a nonce at a bound");
    }
    let unknown = KeyHash::of_spki(b"no such key");
    let err = f
        .store
        .verify_poll(&unknown, &nonce, &key.sign(&poll_message(&unknown, &nonce)))
        .expect_err("unknown");
    assert!(matches!(err, PollError::UnknownRequest));
    assert_eq!(err.token(), Some("unknown_request"));
}

/// Catches a second poll that mints a new certificate instead of returning the same bytes.
#[test]
fn a_poll_after_issue_returns_the_same_certificate() {
    let f = Fixture::new("poll-twice");
    let key = HostKey::new();
    let posted = f.store.post(request(&key, "m-1"), NOW).expect("post");
    let id = posted.request_id;
    let pending = f.store.issue(&f.ca.ca, &id).expect_err("pending");
    assert!(matches!(
        pending,
        IssueError::NotApproved(RequestState::Pending)
    ));
    f.store
        .approve(&id, &id, &decision("web-1", false))
        .expect("approve");
    let first = f.store.issue(&f.ca.ca, &id).expect("issue");
    f.ca.clock.set(NOW + 3600);
    let second = f.store.issue(&f.ca.ca, &id).expect("issue");
    let third = f.reopen().issue(&f.ca.ca, &id).expect("issue");
    assert_eq!(first, second);
    assert_eq!(first, third);
    assert_eq!(
        (
            first.serial,
            first.host_id.as_str(),
            first.not_after,
            first.round
        ),
        (2, "web-1", NOW + HOST_VALIDITY_SECS, 0)
    );
    assert_eq!(f.files_in("issued"), 1);
    let view = f.store.request(&id).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Issued);
    let unknown = KeyHash::of_spki(b"no such key");
    let err = f.store.issue(&f.ca.ca, &unknown).expect_err("unknown");
    assert!(matches!(err, IssueError::UnknownRequest));
    assert_eq!(err.token(), Some("unknown_request"));
    assert_eq!(f.store.allocate_serial().expect("serial"), 3);
    // An issued request whose certificate file is gone is refused, never signed again.
    std::fs::remove_file(f.dir.path().join("issued").join(format!("{id}.pb"))).expect("rm");
    let gone = f.store.issue(&f.ca.ca, &id).expect_err("file gone");
    assert!(
        matches!(gone, IssueError::Store(StoreError::Corrupt { .. })),
        "{gone}"
    );
    assert_eq!(gone.token(), None);
}

/// Catches a host whose certificate expired being given a new host id, a renewal approved
/// under another id, and a `renewal_of` accepted for another key, a denied host or an unknown
/// host.
#[test]
fn a_host_offline_past_expiry_returns_as_a_renewal_request() {
    let f = Fixture::new("renewal");
    let key = HostKey::new();
    let first = f.enroll(&key, "m-1", "web-1");
    f.ca.clock.set(NOW + HOST_VALIDITY_SECS + 3600);
    let later = NOW + HOST_VALIDITY_SECS + 3600;
    let posted = f
        .store
        .post(renewal(&key, "m-1", "web-1"), later)
        .expect("renewal");
    assert_eq!(posted.request_id, key.key_hash());
    assert_eq!(
        (posted.state, posted.round, posted.reposted),
        (RequestState::Pending, 1, false)
    );
    assert_eq!(posted.renewal_of, Some(host_id("web-1")));
    // Posting the renewal again is the same request.
    let again = f
        .store
        .post(renewal(&key, "m-1", "web-1"), later)
        .expect("again");
    assert_eq!((again.round, again.reposted), (1, true));
    assert_eq!(again.renewal_of, Some(host_id("web-1")));
    let id = posted.request_id;
    let elsewhere = f
        .store
        .approve(&id, &id, &decision("web-2", false))
        .expect_err("another host id");
    assert!(matches!(elsewhere, ApproveError::RenewalOfOtherHost));
    assert_eq!(elsewhere.token(), Some("renewal_of_other_host"));
    let approval = f
        .store
        .approve(&id, &id, &decision("web-1", true))
        .expect("one approval");
    assert!(approval.renewal);
    let renewed = f.store.issue(&f.ca.ca, &id).expect("issue");
    assert_eq!(renewed.host_id, host_id("web-1"));
    assert_eq!((renewed.round, renewed.serial), (1, 3));
    assert_eq!(renewed.not_after, later + HOST_VALIDITY_SECS);
    assert_ne!(renewed.certificate_der, first);
    assert_eq!(f.files_in("hosts"), 1, "no new host id");
    assert_eq!(f.files_in("requests"), 1);

    // Refused: another key, an unknown host, a denied host.
    let stranger = HostKey::new();
    let refusals = [
        renewal(&stranger, "m-1", "web-1"),
        renewal(&key, "m-1", "nobody"),
    ];
    for refused in refusals {
        let err = f.store.post(refused, later).expect_err("refused");
        assert!(matches!(err, PostError::RenewalNotAllowed), "{err}");
        assert_eq!(err.token(), Some("renewal_not_allowed"));
    }
    f.store.deny(&host_id("web-1")).expect("deny");
    let err = f
        .store
        .post(renewal(&key, "m-1", "web-1"), later)
        .expect_err("denied");
    assert!(matches!(err, PostError::RenewalNotAllowed));
    assert_eq!(
        f.files_in("requests"),
        1,
        "a refused renewal writes nothing"
    );
}

/// Catches a renewal from another machine id going unflagged, and a renewal approved after
/// another key replaced its host.
#[test]
fn a_renewal_from_another_machine_is_a_duplicate_claim() {
    let f = Fixture::new("renewal-machine");
    let key = HostKey::new();
    f.enroll(&key, "m-1", "web-1");
    let (posted, events) = capture(|| f.store.post(renewal(&key, "m-2", "web-1"), NOW));
    let posted = posted.expect("renewal");
    assert_eq!(posted.state, RequestState::DuplicateClaim);
    assert_eq!(
        events[0].get("reason").map(String::as_str),
        Some("the renewal's machine id is not the host's")
    );
    // Another key takes the host id over before the renewal is approved.
    let newcomer = HostKey::new();
    let taken = f.store.post(request(&newcomer, "m-9"), NOW).expect("post");
    f.store
        .approve(
            &taken.request_id,
            &taken.request_id,
            &decision("web-1", true),
        )
        .expect("replace");
    let err = f
        .store
        .approve(
            &posted.request_id,
            &posted.request_id,
            &decision("web-1", true),
        )
        .expect_err("the host holds another key now");
    assert!(matches!(err, ApproveError::HostIdTaken), "{err}");
}

/// Catches a second live machine id on one host id going unmarked, the mark being lost on a
/// reopen, an old report outside the window counting, and the mark being cleared by anything
/// but a new key's approval.
#[test]
fn two_live_daemons_on_one_id_are_duplicate() {
    let f = Fixture::new("live-duplicate");
    let key = HostKey::new();
    f.enroll(&key, "m-1", "web-1");
    let web = host_id("web-1");
    assert!(!f.store.report_live(&web, "m-1", NOW).expect("live"));
    assert!(!f.store.report_live(&web, "m-1", NOW + 30).expect("live"));
    // A second machine after the window: the first one's report has expired.
    let after = NOW + 30 + LIVE_WINDOW_SECS + 1;
    assert!(!f.store.report_live(&web, "m-2", after).expect("live"));
    let (dup, events) = capture(|| f.store.report_live(&web, "m-1", after + LIVE_WINDOW_SECS));
    assert!(dup.expect("live"), "two machines within the window");
    assert_eq!(
        events,
        [event(
            "WARN",
            &[
                ("message", "two live daemons report one host id"),
                ("host_id", "web-1"),
                ("machines", "2"),
            ]
        )]
    );
    let (still, events) = capture(|| f.store.report_live(&web, "m-1", after + LIVE_WINDOW_SECS));
    assert!(still.expect("live"));
    assert!(events.is_empty(), "one alert per mark");
    assert!(f.store.is_duplicate(&web).expect("read"));
    let reopened = f.reopen();
    assert!(reopened.is_duplicate(&web).expect("read"));
    assert!(
        reopened
            .report_live(&web, "m-1", after + 2 * LIVE_WINDOW_SECS)
            .expect("live")
    );
    // A renewal keeps the mark; a new key's approval clears it.
    let renewing = reopened
        .post(renewal(&key, "m-1", "web-1"), NOW)
        .expect("renewal");
    reopened
        .approve(
            &renewing.request_id,
            &renewing.request_id,
            &decision("web-1", false),
        )
        .expect("renewal approval");
    assert!(reopened.is_duplicate(&web).expect("read"));
    let newcomer = HostKey::new();
    let posted = reopened.post(request(&newcomer, "m-7"), NOW).expect("post");
    reopened
        .approve(
            &posted.request_id,
            &posted.request_id,
            &decision("web-1", true),
        )
        .expect("replace");
    assert!(!reopened.is_duplicate(&web).expect("read"));
    let unknown = reopened
        .report_live(&host_id("nobody"), "m-1", NOW)
        .expect_err("never approved");
    assert_eq!(unknown.to_string(), "no host nobody was approved");
    assert!(!reopened.is_duplicate(&host_id("nobody")).expect("read"));
}

/// Catches a deny row that `is_denied` misses, that leaves the host's key able to renew or its
/// request able to be issued, that a plain approval overrides, or that `replace` clears without
/// recording it; and the retiring state not ending at `retire`.
#[test]
fn a_denied_host_is_refused_by_is_denied_and_cannot_renew_in_the_store() {
    let f = Fixture::new("deny");
    let key = HostKey::new();
    f.enroll(&key, "m-1", "web-1");
    let web = host_id("web-1");
    assert!(!f.store.is_denied(&web).expect("read"));
    assert!(f.store.renew_allowed(&web, &key.key_hash()).expect("read"));
    f.store.deny(&web).expect("deny");
    assert!(f.store.is_denied(&web).expect("read"));
    assert!(f.store.is_retiring(&web).expect("read"));
    assert!(!f.store.renew_allowed(&web, &key.key_hash()).expect("read"));
    let id = key.key_hash();
    let view = f.store.request(&id).expect("read").expect("held");
    assert_eq!(view.state, RequestState::Denied);
    let reposted = f.store.post(request(&key, "m-1"), NOW).expect("post");
    assert_eq!(reposted.state, RequestState::Denied);
    let err = f.store.issue(&f.ca.ca, &id).expect_err("denied");
    assert!(matches!(err, IssueError::Denied));
    assert_eq!(err.token(), Some("denied"));
    // Denying again keeps the row retiring; retiring makes it final.
    f.store.deny(&web).expect("deny again");
    assert!(f.store.is_retiring(&web).expect("read"));
    f.store.retire(&web).expect("retire");
    assert!(f.store.is_denied(&web).expect("read"));
    assert!(!f.store.is_retiring(&web).expect("read"));
    f.store.deny(&web).expect("deny a retired host");
    assert!(
        !f.store.is_retiring(&web).expect("read"),
        "retired stays retired"
    );

    // A new key for the id: refused without replace, admitted with it, and recorded.
    let newcomer = HostKey::new();
    let posted = f.store.post(request(&newcomer, "m-2"), NOW).expect("post");
    let nid = posted.request_id;
    let err = f
        .store
        .approve(&nid, &nid, &decision("web-1", false))
        .expect_err("denied");
    assert!(matches!(err, ApproveError::Denied));
    assert_eq!(err.token(), Some("denied"));
    assert!(f.store.is_denied(&web).expect("read"));
    let approval = f
        .store
        .approve(&nid, &nid, &decision("web-1", true))
        .expect("replace");
    assert!(approval.cleared_deny);
    assert!(!f.store.is_denied(&web).expect("read"));
    assert!(!f.store.is_retiring(&web).expect("read"));
    assert!(f.store.renew_allowed(&web, &nid).expect("read"));
    assert!(!f.store.renew_allowed(&web, &id).expect("read"));
    let deny_file = std::fs::read(f.dir.path().join("deny/web-1.pb")).expect("the row is kept");
    assert!(
        deny_file.windows(32).any(|window| window == nid.as_bytes()),
        "the clearing request is recorded"
    );
    // A host id never enrolled can be denied (and retired) ahead of time.
    let ahead = host_id("not-yet");
    f.store.retire(&ahead).expect("retire");
    assert!(f.store.is_denied(&ahead).expect("read"));
    // An approval issued after its host was denied is refused at issue.
    let late = HostKey::new();
    let late_id = f
        .store
        .post(request(&late, "m-3"), NOW)
        .expect("post")
        .request_id;
    f.store
        .approve(&late_id, &late_id, &decision("web-3", false))
        .expect("approve");
    f.store.deny(&host_id("web-3")).expect("deny");
    assert!(matches!(
        f.store.issue(&f.ca.ca, &late_id),
        Err(IssueError::Denied)
    ));
    let signer: &dyn Signer = &f.ca.ca;
    assert_eq!(signer.bundle(), f.ca.ca.bundle());
}

/// Catches a refusal whose message does not start with its token, and a failure of the store
/// being given a refusal token.
#[test]
fn every_refusal_names_its_token() {
    let cases: [(ApproveError, &str); 3] = [
        (ApproveError::Denied, "denied: the host id is denied"),
        (
            ApproveError::HostIdTaken,
            "host_id_taken: another key holds this host id",
        ),
        (
            ApproveError::NotPending(RequestState::Issued),
            "not_pending: the request is Issued",
        ),
    ];
    for (err, message) in cases {
        assert_eq!(err.to_string(), message);
    }
    let store = ApproveError::Store(StoreError::UnknownHost(host_id("web-1")));
    assert_eq!(store.token(), None);
    let post = PostError::Store(StoreError::UnknownHost(host_id("web-1")));
    assert_eq!(post.token(), None);
    let poll = PollError::Store(StoreError::UnknownHost(host_id("web-1")));
    assert_eq!(poll.token(), None);
    assert_eq!(
        PostError::RenewalNotAllowed.to_string(),
        "renewal_not_allowed: the host does not exist, is denied, or holds another key"
    );
}
