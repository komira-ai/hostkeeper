//! Decisions of the store raced by another writer of the same records: two stores over one fake
//! stand for two coordinators over one database, and the fake runs the other one's decision
//! between this one's reads and its write ([`MemoryRecords::interleave`]). Each write lands only
//! on what its decision read, so the second to write decides again on what the first left, and
//! answers as the decision stands after it (docs/design/coordinator-multi-instance.md,
//! section 6).

mod support;

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, EnrollmentStore, HostRunning, IssuedCertificate, MacNonceHash,
    Membership, RejectError, RequestState, RunningService, StoreError,
};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use support::{HostKey, NOW, TestCa, decision, host_id, request};

/// The request time-to-live the expiry tests use.
const TTL: u64 = 600;

/// Two coordinators over one database: this one's store, the other's, and the records they share.
struct Two {
    records: Arc<MemoryRecords>,
    mine: EnrollmentStore,
    theirs: Arc<EnrollmentStore>,
}

impl Two {
    fn new() -> Two {
        let records = Arc::new(MemoryRecords::new());
        Two {
            mine: EnrollmentStore::new(records.clone()),
            theirs: Arc::new(EnrollmentStore::new(records.clone())),
            records,
        }
    }

    /// Runs `write` with the other coordinator's store before this one's next write checks what
    /// its decision read.
    fn between(&self, write: impl FnOnce(&EnrollmentStore) + Send + 'static) {
        let theirs = Arc::clone(&self.theirs);
        self.records.interleave(move || write(&theirs));
    }

    /// The pending request of `key`, posted by the other coordinator.
    fn posted(&self, key: &HostKey, machine_id: &str) -> KeyHash {
        self.theirs
            .post(request(key, machine_id), NOW)
            .expect("posted")
            .request_id
    }

    /// `host` approved for a new key, by the other coordinator.
    fn enrolled(&self, host: &str) -> (HostId, HostKey) {
        let key = HostKey::new();
        let id = self.posted(&key, &format!("machine-of-{host}"));
        self.theirs
            .approve(&id, &id, &decision(host, false))
            .expect("approved");
        (host_id(host), key)
    }
}

/// The membership of a host in `group`, not moving.
fn in_group(group: &str) -> Membership {
    Membership {
        host_group: group.to_owned(),
        moving_from: None,
    }
}

/// The approval record of `approver`.
fn record(approver: &str) -> ApprovalRecord {
    ApprovalRecord {
        approver: approver.to_owned(),
        approved_unix: NOW + 10,
        reason: "racing".to_owned(),
        mac_nonce: None,
        mac_enrolled_since_unix: None,
    }
}

/// Proves: of two approvals of one request, the one whose write comes second answers
/// `not_pending` (approved), as the request stands after the first, and writes nothing: the
/// host id it named is not taken, and the request is approved as the first said.
///
/// Catches: an approval that writes over another coordinator's approval of the same request
/// because both read it pending (two host rows, or the first operator told of an approval the
/// second replaced).
#[test]
fn an_approval_raced_by_another_approval_of_the_request_answers_not_pending() {
    let two = Two::new();
    let id = two.posted(&HostKey::new(), "m-1");
    two.between(move |theirs| {
        theirs
            .approve(&id, &id, &decision("web-1", false))
            .expect("the first approval");
    });
    let mine = two.mine.approve(&id, &id, &decision("web-2", false));
    assert!(
        matches!(mine, Err(ApproveError::NotPending(RequestState::Approved))),
        "{mine:?}"
    );
    assert!(
        two.mine
            .enrolled_host(&host_id("web-1"))
            .expect("read")
            .is_some()
    );
    assert!(
        two.mine
            .enrolled_host(&host_id("web-2"))
            .expect("read")
            .is_none()
    );
    let request = two.mine.request(&id).expect("read").expect("the request");
    assert_eq!(request.host_id, Some(host_id("web-1")));
}

/// Proves: the same for an approval recorded with its approver: one recorded approval, the
/// first's, and the second approver told `not_pending`.
///
/// Catches: two `enroll_approve` audit entries for one request, one per coordinator.
#[test]
fn a_recorded_approval_raced_by_another_is_recorded_once() {
    let two = Two::new();
    let id = two.posted(&HostKey::new(), "m-1");
    two.between(move |theirs| {
        theirs
            .approve_recorded(&id, &id, &decision("web-1", false), &record("first"))
            .expect("the first approval");
    });
    let mine = two
        .mine
        .approve_recorded(&id, &id, &decision("web-1", false), &record("second"));
    assert!(
        matches!(mine, Err(ApproveError::NotPending(RequestState::Approved))),
        "{mine:?}"
    );
    let recorded = two.mine.recorded_approvals().expect("read");
    assert_eq!(
        recorded
            .iter()
            .map(|a| a.approver.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
}

/// Proves: of two approvals of two requests for one new host id, the second to write answers
/// `host_id_taken` and the host id is the first key's.
///
/// Catches: a host row written where the decision read none, over the one another coordinator
/// wrote in between (two keys approved for one host id).
#[test]
fn two_approvals_for_one_new_host_id_raced_admit_one() {
    let two = Two::new();
    let (first, second) = (HostKey::new(), HostKey::new());
    let (a, b) = (two.posted(&first, "m-1"), two.posted(&second, "m-2"));
    two.between(move |theirs| {
        theirs
            .approve(&b, &b, &decision("web-1", false))
            .expect("the first approval");
    });
    let mine = two.mine.approve(&a, &a, &decision("web-1", false));
    assert!(matches!(mine, Err(ApproveError::HostIdTaken)), "{mine:?}");
    assert!(
        two.mine
            .host_role_for(&host_id("web-1"), &second.key_hash())
            .expect("read")
            .is_some()
    );
    assert_eq!(
        two.mine.request(&a).expect("read").expect("there").state,
        RequestState::Pending
    );
}

/// Proves: a key posted on two coordinators at once is one request: the second post finds the
/// first's and answers it as a repost.
///
/// Catches: a request written where the post read none, over the one the other wrote.
#[test]
fn a_key_posted_by_two_at_once_is_one_request() {
    let two = Two::new();
    let key = Arc::new(HostKey::new());
    let theirs = Arc::clone(&key);
    two.between(move |store| {
        store
            .post(request(&theirs, "m-1"), NOW)
            .expect("the first post");
    });
    let mine = two
        .mine
        .post(request(&key, "m-1"), NOW + 1)
        .expect("posted");
    assert!(mine.reposted, "{mine:?}");
    assert_eq!(two.mine.pending().expect("read").len(), 1);
    let posted = two
        .mine
        .request(&mine.request_id)
        .expect("read")
        .expect("there");
    assert_eq!(posted.posted_unix, NOW, "the first post's");
}

/// Issues the request `id` with the other coordinator's store and CA, into `slot`.
fn issue_into(
    ca: &Arc<TestCa>,
    id: KeyHash,
    slot: &Arc<Mutex<Option<IssuedCertificate>>>,
) -> impl FnOnce(&EnrollmentStore) + Send + 'static {
    let (ca, slot) = (Arc::clone(ca), Arc::clone(slot));
    move |theirs| {
        let issued = theirs.issue(&ca.ca, &id).expect("the first issue");
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(issued);
    }
}

/// Proves: when another coordinator issues a request between this one's read of it and its
/// reservation of a serial, this one returns the certificate the other recorded, the same bytes
/// and serial, and does not say it is the first.
///
/// Catches: a second serial reserved, or a second certificate recorded, for one request and round.
#[test]
fn an_issue_raced_at_its_reservation_returns_the_certificate_recorded() {
    let two = Two::new();
    let ca = Arc::new(TestCa::new("raced-reserve"));
    let id = two.posted(&HostKey::new(), "m-1");
    two.theirs
        .approve(&id, &id, &decision("web-1", false))
        .expect("approved");
    let theirs = Arc::new(Mutex::new(None));
    two.between(issue_into(&ca, id, &theirs));
    let mine = two.mine.issue(&ca.ca, &id).expect("issued");
    let theirs = theirs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("the other issued");
    assert!(theirs.first && !mine.first);
    assert_eq!(mine.certificate_der, theirs.certificate_der);
    assert_eq!(mine.serial, theirs.serial);
}

/// Proves: when another coordinator records the certificate of a request between this one's
/// reservation and its own record, this one returns the other's: one certificate is ever
/// recorded and returned for the request and round, whichever was signed first.
///
/// Catches: a certificate recorded over another coordinator's, so two hosts' daemons hold two
/// certificates of one round.
#[test]
fn an_issue_raced_at_its_record_returns_the_certificate_recorded() {
    let two = Two::new();
    let ca = Arc::new(TestCa::new("raced-record"));
    let id = two.posted(&HostKey::new(), "m-1");
    two.theirs
        .approve(&id, &id, &decision("web-1", false))
        .expect("approved");
    two.between(|_| {});
    let theirs = Arc::new(Mutex::new(None));
    two.between(issue_into(&ca, id, &theirs));
    let mine = two.mine.issue(&ca.ca, &id).expect("issued");
    let theirs = theirs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("the other issued");
    assert!(theirs.first && !mine.first);
    assert_eq!(mine.certificate_der, theirs.certificate_der);
    let again = two.mine.issue(&ca.ca, &id).expect("read back");
    assert_eq!(again.certificate_der, theirs.certificate_der);
}

/// Proves: a rejection raced by an approval of the request answers `not_pending` and leaves the
/// approval; an approval raced by a rejection answers `not_pending` (denied).
///
/// Catches: a rejection written over an approval (a host row whose request is denied), or an
/// approval written over a rejection.
#[test]
fn a_rejection_and_an_approval_of_one_request_decide_once() {
    let two = Two::new();
    let id = two.posted(&HostKey::new(), "m-1");
    two.between(move |theirs| {
        theirs
            .approve(&id, &id, &decision("web-1", false))
            .expect("approved");
    });
    let rejected = two.mine.reject(&id);
    assert!(
        matches!(
            rejected,
            Err(RejectError::NotPending(RequestState::Approved))
        ),
        "{rejected:?}"
    );

    let other = two.posted(&HostKey::new(), "m-2");
    two.between(move |theirs| {
        theirs.reject(&other).expect("rejected");
    });
    let approved = two.mine.approve(&other, &other, &decision("web-2", false));
    assert!(
        matches!(
            approved,
            Err(ApproveError::NotPending(RequestState::Denied))
        ),
        "{approved:?}"
    );
    assert!(
        two.mine
            .enrolled_host(&host_id("web-2"))
            .expect("read")
            .is_none()
    );
}

/// Proves: an expiry raced by an approval of one of the requests it expires expires the others
/// only; an expiry raced by another expiry expires nothing more, so each request is expired, and
/// answered expired, once.
///
/// Catches: an approval undone by an expiry that read the request pending, or one request
/// returned as expired by two coordinators (two `request_expired` audit entries).
#[test]
fn an_expiry_raced_by_an_approval_or_an_expiry_expires_each_request_once() {
    let two = Two::new();
    let (a, b) = (
        two.posted(&HostKey::new(), "m-1"),
        two.posted(&HostKey::new(), "m-2"),
    );
    two.between(move |theirs| {
        theirs
            .approve(&a, &a, &decision("web-1", false))
            .expect("approved");
    });
    let expired = two.mine.expire(NOW + TTL + 1, TTL).expect("expired");
    assert_eq!(
        expired.iter().map(|r| r.request_id).collect::<Vec<_>>(),
        [b]
    );
    assert_eq!(
        two.mine.request(&a).expect("read").expect("there").state,
        RequestState::Approved
    );

    let c = two.posted(&HostKey::new(), "m-3");
    two.between(|theirs| {
        let theirs = theirs.expire(NOW + TTL + 2, TTL).expect("expired");
        assert_eq!(theirs.len(), 1);
    });
    let again = two.mine.expire(NOW + TTL + 2, TTL).expect("expired");
    assert_eq!(again, Vec::new());
    assert_eq!(
        two.mine.request(&c).expect("read").expect("there").state,
        RequestState::Expired
    );
}

/// Proves: of two denials of one host at once, exactly one says the deny row is new; a
/// retirement raced by another leaves the row retired.
///
/// Catches: two coordinators both told the deny is new (two `host_retire` audit entries).
#[test]
fn two_denials_of_one_host_at_once_are_new_once() {
    let two = Two::new();
    let (host, _) = two.enrolled("web-1");
    let theirs_host = host.clone();
    let first = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&first);
    two.between(move |theirs| {
        *slot.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(theirs.deny(&theirs_host).expect("denied"));
    });
    let mine = two.mine.deny(&host).expect("denied");
    assert_eq!(
        (*first.lock().unwrap_or_else(PoisonError::into_inner), mine),
        (Some(true), false)
    );

    let theirs_host = host.clone();
    two.between(move |theirs| theirs.retire(&theirs_host).expect("retired"));
    two.mine.retire(&host).expect("retired");
    assert!(two.mine.is_denied(&host).expect("read"));
    assert!(!two.mine.is_retiring(&host).expect("read"));
}

/// Proves: a heartbeat's first-heartbeat, first-serving and running records, and the duplicate
/// mark, written over a host record another coordinator changed since it was read, are written
/// on the record as that coordinator left it: the group it moved the host to stays.
///
/// Catches: a stale host record written back whole by a heartbeat, undoing another
/// coordinator's change of the host (its group, its key, its role).
#[test]
fn a_heartbeat_mark_raced_by_a_change_of_the_host_keeps_the_change() {
    let two = Two::new();
    let (host, _) = two.enrolled("web-1");
    let moved = |group: &'static str| {
        let host = host.clone();
        move |theirs: &EnrollmentStore| {
            theirs
                .set_membership(&host, in_group(group))
                .expect("moved");
        }
    };
    two.between(moved("a"));
    two.mine.note_heartbeat(&host, NOW + 1).expect("noted");
    two.between(moved("b"));
    two.mine.note_serving(&host, NOW + 2).expect("noted");
    two.between(moved("c"));
    let running = HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "1.0".to_owned(),
        services: vec![RunningService {
            service: "api".to_owned(),
            version: "2.0".to_owned(),
            installer: "deb".to_owned(),
        }],
    };
    two.mine
        .note_running(&host, &running, NOW + 3)
        .expect("noted");
    assert!(!two.mine.report_live(&host, "m-1", NOW + 4).expect("live"));
    two.between(moved("d"));
    assert!(two.mine.report_live(&host, "m-2", NOW + 5).expect("live"));

    let enrolled = two
        .mine
        .enrolled_host(&host)
        .expect("read")
        .expect("enrolled");
    assert_eq!(enrolled.host_group, "d");
    assert_eq!(enrolled.first_heartbeat_unix, Some(NOW + 1));
    assert_eq!(enrolled.first_serving_unix, Some(NOW + 2));
    assert_eq!(enrolled.running, Some(running));
    assert!(two.mine.is_duplicate(&host).expect("read"));
}

/// Proves: a heartbeat raced by another coordinator's record of the same first heartbeat keeps
/// the first one's time.
///
/// Catches: a first heartbeat written twice, the later time over the earlier.
#[test]
fn a_first_heartbeat_raced_by_another_keeps_the_first() {
    let two = Two::new();
    let (host, _) = two.enrolled("web-1");
    let theirs_host = host.clone();
    two.between(move |theirs| theirs.note_heartbeat(&theirs_host, NOW + 1).expect("noted"));
    two.mine.note_heartbeat(&host, NOW + 2).expect("noted");
    let enrolled = two
        .mine
        .enrolled_host(&host)
        .expect("read")
        .expect("enrolled");
    assert_eq!(enrolled.first_heartbeat_unix, Some(NOW + 1));
}

/// Proves: a move of a host raced by another answers the membership the other left, which the
/// caller writes back should its own other write fail.
///
/// Catches: a move that answers the membership it read before the other's move, so a caller's
/// undo writes back a group the host had already left.
#[test]
fn a_move_raced_by_another_answers_the_membership_the_other_left() {
    let two = Two::new();
    let (host, _) = two.enrolled("web-1");
    let theirs_host = host.clone();
    two.between(move |theirs| {
        theirs
            .set_membership(&theirs_host, in_group("db"))
            .expect("moved");
    });
    let before = two
        .mine
        .set_membership(&host, in_group("web"))
        .expect("moved");
    assert_eq!(before, in_group("db"));
    assert_eq!(
        two.mine.membership(&host).expect("read"),
        Some(in_group("web"))
    );
}

/// Proves: a Mac nonce issued by another coordinator between this one's read and write is not
/// issued again: this one answers that it exists.
///
/// Catches: a nonce record written over another's (its request or serial replaced).
#[test]
fn a_mac_nonce_raced_by_the_same_nonce_is_issued_once() {
    let two = Two::new();
    let hash = MacNonceHash::of(b"a nonce of sixteen bytes or more");
    let (first, second) = (KeyHash::of_spki(b"first"), KeyHash::of_spki(b"second"));
    two.between(move |theirs| {
        assert!(
            theirs
                .issue_mac_nonce("C02", &first, &hash, NOW)
                .expect("issued")
        );
    });
    let mine = two
        .mine
        .issue_mac_nonce("C02", &second, &hash, NOW + 1)
        .expect("answered");
    assert!(!mine);
}

/// Proves: a renewal's serial is taken only while the host holds the key and is not denied, in
/// the same write: a deny another coordinator records in between refuses it and takes no serial;
/// another key is refused; the host's own key, with nothing in between, takes the next serial.
///
/// Catches: a certificate signed for a host denied between the renewal's check and its serial.
#[test]
fn a_renewal_raced_by_a_deny_takes_no_serial() {
    let two = Two::new();
    let (denied, denied_key) = two.enrolled("web-1");
    let (renewing, renewing_key) = two.enrolled("web-2");
    let theirs_host = denied.clone();
    two.between(move |theirs| {
        theirs.deny(&theirs_host).expect("denied");
    });
    let refused = two.mine.renewal_serial(&denied, &denied_key.key_hash());
    assert_eq!(refused.expect("decided"), None);
    let other = two
        .mine
        .renewal_serial(&renewing, &denied_key.key_hash())
        .expect("decided");
    assert_eq!(other, None, "another key may not renew");
    let serial = two
        .mine
        .renewal_serial(&renewing, &renewing_key.key_hash())
        .expect("decided");
    assert_eq!(
        serial,
        Some(hostkeeper_enrollment::enroll::FIRST_SERIAL),
        "the refused renewals took no serial"
    );
}

/// Proves: a decision raced on two runs lands on its third, answered as the two writers before
/// it left the records; one raced on each of its three runs is refused, with nothing written,
/// and the refusal names the record another writer changed.
///
/// Catches: a decision retried until it lands however often the records change under it, a
/// refusal answered as if the write had landed, or one returned while a run that would land is
/// left.
#[test]
fn a_decision_raced_on_every_run_is_refused_and_writes_nothing() {
    let two = Two::new();
    let (host, _) = two.enrolled("web-1");
    let moves = |groups: &[&'static str]| {
        for &group in groups {
            let host = host.clone();
            two.between(move |theirs| {
                theirs
                    .set_membership(&host, in_group(group))
                    .expect("moved");
            });
        }
    };
    moves(&["a", "b"]);
    let before = two
        .mine
        .set_membership(&host, in_group("web"))
        .expect("moved on the third run");
    assert_eq!(before, in_group("b"));

    moves(&["c", "d", "e"]);
    let refused = two
        .mine
        .set_membership(&host, in_group("web"))
        .expect_err("raced on every run");
    assert!(matches!(refused, StoreError::Raced { .. }), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        "memory/hosts/web-1 was changed by another writer since it was read: nothing is written"
    );
    assert_eq!(
        two.mine.membership(&host).expect("read"),
        Some(in_group("e"))
    );
}
