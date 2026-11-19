//! Enrollment on two coordinators over one database (docs/design/coordinator-multi-instance.md,
//! section 6, "Enrollment"): two stores, each with sessions of its own as a coordinator task has
//! them, with no lock between them in the process. A decision's write lands only on the records
//! it read; a deny row and a host row are read fresh.
//!
//! Where two decisions race, each reads before either writes: the records of each meet the
//! other's at a barrier at their first write ([`Gate`]), then the two writes run against
//! Postgres side by side, and the database decides which lands. Nothing sleeps and no time is
//! measured: a wait for a lock is seen in `pg_locks`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use hostkeeper_enrollment::ca::{Ca, CaFiles};
use hostkeeper_enrollment::enroll::records::{Record, RecordKey, Records, RecordsLock, Seen, pb};
use hostkeeper_enrollment::enroll::{
    EnrollmentStore, HostRunning, MacNonceHash, Membership, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{HostId, KeyHash};
use hostkeeper_oidc::SpentJtis;

use super::coordinator::Holding;
use super::{TIMEOUTS, TestDatabase, options};
use crate::enrollment::tests::support::{
    Files, FixedClock, HostKey, NOW, approval, enroll_request,
};
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use crate::enrollment::{Admission, Enrollment, Setup, StoreAt, os_random, setup};
use crate::store::postgres::dsn::Timeouts;
use crate::store::postgres::records::RECORD_LOCK_CLASS;
use crate::store::postgres::{PgRecords, Postgres};
use crate::store::{Shared, Spent};
use crate::testutil::{ops_of, scratch};

/// Two coordinators started on `db`, each with its own sessions and its own instance row.
fn two_tasks(db: &TestDatabase) -> (Postgres, Postgres) {
    let (first, _) = db
        .open_options(&options(), "first")
        .expect("the first starts");
    let (second, _) = db
        .open_options(&options(), "second")
        .expect("the second starts");
    (first, second)
}

/// The records of one coordinator, which, once armed, wait at their next write until the other
/// coordinator's records reach theirs: so both decisions have read before either writes.
#[derive(Debug)]
struct Gate {
    inner: PgRecords,
    barrier: Arc<Barrier>,
    armed: AtomicBool,
}

impl Gate {
    fn over(store: &Postgres, barrier: &Arc<Barrier>) -> Arc<Gate> {
        Arc::new(Gate {
            inner: PgRecords::over(store),
            barrier: Arc::clone(barrier),
            armed: AtomicBool::new(false),
        })
    }

    /// Makes the next write wait for the other's.
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    fn meet(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.barrier.wait();
        }
    }
}

impl Records for Gate {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        self.inner.lock()
    }

    fn place(&self, key: &RecordKey) -> PathBuf {
        self.inner.place(key)
    }

    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        self.inner.request(id)
    }

    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        self.inner.requests()
    }

    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        self.inner.issued(id)
    }

    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        self.inner.host(host)
    }

    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        self.inner.host_ids()
    }

    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        self.inner.deny(host)
    }

    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        self.inner.mac_nonce(hash)
    }

    fn commit(&self, seen: &[Seen], records: &[Record]) -> Result<(), StoreError> {
        self.meet();
        self.inner.commit(seen, records)
    }

    fn take_serial(
        &self,
        seen: &[Seen],
        reserving: Option<&pb::Request>,
    ) -> Result<u64, StoreError> {
        self.meet();
        self.inner.take_serial(seen, reserving)
    }

    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.meet();
        self.inner.set_first_heartbeat(host, at_unix)
    }

    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.meet();
        self.inner.set_first_serving(host, at_unix)
    }

    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        self.meet();
        self.inner.set_running(host, running)
    }

    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        self.meet();
        self.inner.set_duplicate(host)
    }
}

/// Two coordinators' enrollment stores over `db`, each over its [`Gate`].
struct Racing {
    gates: [Arc<Gate>; 2],
    stores: [EnrollmentStore; 2],
    _tasks: (Postgres, Postgres),
}

impl Racing {
    fn new(db: &TestDatabase) -> Racing {
        let tasks = two_tasks(db);
        let barrier = Arc::new(Barrier::new(2));
        let gates = [
            Gate::over(&tasks.0, &barrier),
            Gate::over(&tasks.1, &barrier),
        ];
        let stores = [
            EnrollmentStore::new(gates[0].clone()),
            EnrollmentStore::new(gates[1].clone()),
        ];
        Racing {
            gates,
            stores,
            _tasks: tasks,
        }
    }

    /// Runs `first` on the first coordinator and `second` on the other at once, each write of
    /// the two waiting until both have read. Each must write: one that answers without writing
    /// leaves the other at the barrier.
    fn at_once<A: Send, B: Send>(
        &self,
        first: impl FnOnce(&EnrollmentStore) -> A + Send,
        second: impl FnOnce(&EnrollmentStore) -> B + Send,
    ) -> (A, B) {
        for gate in &self.gates {
            gate.arm();
        }
        let [one, two] = &self.stores;
        std::thread::scope(|scope| {
            let a = scope.spawn(|| first(one));
            let b = scope.spawn(|| second(two));
            (
                a.join().expect("the first ends"),
                b.join().expect("the second ends"),
            )
        })
    }

    /// A pending request of a new key, posted on the first coordinator.
    fn posted(&self, machine_id: &str) -> KeyHash {
        let key = HostKey::new();
        self.stores[0]
            .post(
                hostkeeper_enrollment::enroll::NewRequest {
                    csr_der: key.csr(),
                    claims: hostkeeper_enrollment::enroll::Claims {
                        machine_id: machine_id.to_owned(),
                        ..Default::default()
                    },
                    image: "hostkeeper-image-1".to_owned(),
                    renewal_of: None,
                    mac_nonce_presented: false,
                },
                NOW,
            )
            .expect("posted")
            .request_id
    }
}

/// The decision to approve as `host`, a web host.
fn decision(host: &str) -> hostkeeper_enrollment::enroll::Decision {
    hostkeeper_enrollment::enroll::Decision {
        host_id: HostId::parse(host).expect("a host id"),
        role: hostkeeper_enrollment::names::Role::parse("web").expect("a role"),
        services: Vec::new(),
        replace: false,
        host_group: None,
    }
}

/// Two coordinators' enrollment, as `serve` starts it, over `db`: one CA and server name
/// (the files of `files`), each over its own store, both auditing into one log.
fn two_enrollments(
    tasks: &(Postgres, Postgres),
    barrier: &Arc<Barrier>,
    files: &Files,
    ops: &Arc<crate::ops::OpsStore>,
) -> [(Arc<Gate>, Enrollment); 2] {
    let start = |store: &Postgres| {
        let gate = Gate::over(store, barrier);
        let enrollment = setup(
            Arc::new(RealFs),
            &Setup {
                store: StoreAt::Records(gate.clone()),
                ..files.setup(true)
            },
            FixedClock::at(NOW),
            os_random(),
        )
        .expect("enrollment starts");
        enrollment.audit_to(Arc::clone(ops));
        (gate, enrollment)
    };
    [start(&tasks.0), start(&tasks.1)]
}

/// Proves: two operators approving one request on two coordinators at once, each having read it
/// pending, approve it once: one is answered with the approval, the other `not_pending`, as the
/// request stands after the first; the audit log holds one `enroll_approve` entry, the
/// winner's, and the other's refusal; and the host id the other named was not taken.
///
/// Catches: an approval written over the other coordinator's because both read the request
/// pending (two approvals audited, the first operator told of an approval that was replaced).
#[test]
fn one_request_is_approved_once() {
    let db = TestDatabase::new();
    let tasks = two_tasks(&db);
    let dir = scratch("approved-once");
    let files = Files::in_dir(&dir);
    let ops = Arc::new(ops_of(&dir));
    let barrier = Arc::new(Barrier::new(2));
    let [(gate_a, a), (gate_b, b)] = two_enrollments(&tasks, &barrier, &files, &ops);
    let key = HostKey::new();
    a.enroll(enroll_request(&key, "m-1")).expect("enrolled");
    gate_a.arm();
    gate_b.arm();
    let known = |_: &str| true;
    let (first, second) = std::thread::scope(|scope| {
        let one = scope.spawn(|| a.approve("alice", approval(&key, "web-1"), &known, &[]));
        let two = scope.spawn(|| b.approve("bob", approval(&key, "web-2"), &known, &[]));
        (
            one.join().expect("the first ends"),
            two.join().expect("the second ends"),
        )
    });
    let results = [first.map(|_| "web-1"), second.map(|_| "web-2")];
    let won: Vec<&str> = results
        .iter()
        .filter_map(|r| r.as_ref().ok().copied())
        .collect();
    assert_eq!(won.len(), 1, "{results:?}");
    assert!(
        results.contains(&Err(EnrollmentRefusal::NotPending)),
        "{results:?}"
    );
    let lost = if won[0] == "web-1" { "web-2" } else { "web-1" };
    let store = b.store();
    let host = |id: &str| HostId::parse(id).expect("a host id");
    assert!(store.enrolled_host(&host(won[0])).expect("read").is_some());
    assert!(store.enrolled_host(&host(lost)).expect("read").is_none());

    let log = ops.audit_log().expect("the audit log");
    let actions = |action: &str| log.iter().filter(|e| e.action == action).count();
    assert_eq!(actions("enroll_approve"), 1, "{log:?}");
    let refused: Vec<_> = log
        .iter()
        .filter(|e| e.action == "enroll_approve_refused")
        .map(|e| e.reason.clone())
        .collect();
    assert_eq!(
        refused,
        [Some(format!(
            "not_pending; request {}",
            key.key_hash().to_hex()
        ))]
    );
}

/// Proves: a host approved on one coordinator is enrolled on the other at its next read, and a
/// host retired on one is refused by the other's next heartbeat, with no restart: the deny row
/// is read fresh.
///
/// Catches: a host or deny row served from one coordinator's memory, so the other refuses a host
/// approved elsewhere, or keeps admitting one retired elsewhere.
#[test]
fn denied_host_is_refused_by_the_other_task() {
    let db = TestDatabase::new();
    let tasks = two_tasks(&db);
    let dir = scratch("denied-elsewhere");
    let files = Files::in_dir(&dir);
    let ops = Arc::new(ops_of(&dir));
    let barrier = Arc::new(Barrier::new(2));
    let [(_, a), (_, b)] = two_enrollments(&tasks, &barrier, &files, &ops);
    let id = HostId::parse("web-1").expect("a host id");
    let wire: hostkeeper_wire::HostId = "web-1".parse().expect("a host id");
    assert_eq!(b.admission(&wire), Admission::Admitted, "never approved");
    let key = HostKey::new();
    a.enroll(enroll_request(&key, "m-1")).expect("enrolled");
    a.approve("alice", approval(&key, "web-1"), &|_| true, &[])
        .expect("approved");
    assert!(
        b.store().enrolled_host(&id).expect("read").is_some(),
        "approved on the other"
    );
    assert_eq!(b.admission(&wire), Admission::Admitted);

    assert_eq!(a.deny_host(&wire), Ok(true));
    assert_eq!(
        b.admission(&wire),
        Admission::Retiring,
        "retired on the other"
    );
    assert_eq!(b.deny_host(&wire), Ok(false), "denied already");
    a.retire(&wire);
    assert_eq!(b.admission(&wire), Admission::Denied);
}

/// Proves: two coordinators issuing one approved request at once return the same certificate,
/// with one serial, and exactly one of them says it issued it.
///
/// Catches: two serials reserved, or two certificates recorded, for one request and round (two
/// `enroll_issued` audit entries, and a daemon holding a certificate the store does not).
#[test]
fn one_certificate_is_issued_per_round_by_two_tasks() {
    let db = TestDatabase::new();
    let racing = Racing::new(&db);
    let dir = scratch("issued-once");
    let (key, certificate) = (dir.path().join("ca.key"), dir.path().join("ca.cert"));
    let ca = Ca::init(
        &RealFs,
        CaFiles {
            key: &key,
            certificate: &certificate,
        },
        FixedClock::at(NOW),
    )
    .expect("a CA");
    let id = racing.posted("m-1");
    racing.stores[0]
        .approve(&id, &id, &decision("web-1"))
        .expect("approved");
    let (one, two) = racing.at_once(|s| s.issue(&ca, &id), |s| s.issue(&ca, &id));
    let (one, two) = (one.expect("issued"), two.expect("issued"));
    assert_eq!(one.certificate_der, two.certificate_der);
    assert_eq!(one.serial, two.serial);
    assert!(
        one.first != two.first,
        "one says it was first: {one:?} {two:?}"
    );
    let again = racing.stores[1].issue(&ca, &id).expect("read back");
    assert_eq!(again.certificate_der, one.certificate_der);
    assert!(!again.first);
}

/// Proves: two coordinators expiring the same requests at once expire each once: between them
/// they answer every request that waited too long, and no request twice.
///
/// Catches: one request expired, and audited `request_expired`, by both coordinators.
#[test]
fn expiry_on_two_tasks_expires_each_request_once() {
    let db = TestDatabase::new();
    let racing = Racing::new(&db);
    let mut posted: Vec<KeyHash> = ["m-1", "m-2", "m-3"]
        .iter()
        .map(|m| racing.posted(m))
        .collect();
    let (ttl, now) = (600, NOW + 601);
    let (one, two) = racing.at_once(|s| s.expire(now, ttl), |s| s.expire(now, ttl));
    let mut expired: Vec<KeyHash> = one
        .expect("expired")
        .into_iter()
        .chain(two.expect("expired"))
        .map(|request| request.request_id)
        .collect();
    expired.sort();
    posted.sort();
    assert_eq!(expired, posted);
    for id in &posted {
        let request = racing.stores[0].request(id).expect("read").expect("there");
        assert_eq!(request.state, RequestState::Expired);
    }
}

/// Proves: an approval and a rejection of one request on two coordinators at once decide it
/// once: exactly one of them is answered, and the request and the host row are as that one
/// left them.
///
/// Catches: a rejection written over an approval (a host enrolled whose request is denied), or
/// the reverse.
#[test]
fn an_approval_and_a_rejection_on_two_tasks_decide_once() {
    let db = TestDatabase::new();
    let racing = Racing::new(&db);
    let id = racing.posted("m-1");
    let (approved, rejected) = racing.at_once(
        |s| s.approve(&id, &id, &decision("web-1")).map(drop),
        |s| s.reject(&id).map(drop),
    );
    assert!(
        approved.is_ok() != rejected.is_ok(),
        "{approved:?} {rejected:?}"
    );
    let state = racing.stores[1]
        .request(&id)
        .expect("read")
        .expect("there")
        .state;
    let enrolled = racing.stores[1]
        .enrolled_host(&HostId::parse("web-1").expect("a host id"))
        .expect("read")
        .is_some();
    if approved.is_ok() {
        assert_eq!((state, enrolled), (RequestState::Approved, true));
    } else {
        assert_eq!((state, enrolled), (RequestState::Denied, false));
    }
}

/// Proves: two coordinators denying one host at once: exactly one is told the deny row is new.
///
/// Catches: both told it is new (two `host_retire` audit entries for one retirement).
#[test]
fn two_tasks_denying_one_host_see_it_new_once() {
    let db = TestDatabase::new();
    let racing = Racing::new(&db);
    let id = racing.posted("m-1");
    racing.stores[0]
        .approve(&id, &id, &decision("web-1"))
        .expect("approved");
    let host = HostId::parse("web-1").expect("a host id");
    let (one, two) = racing.at_once(|s| s.deny(&host), |s| s.deny(&host));
    let (one, two) = (one.expect("denied"), two.expect("denied"));
    assert!(one != two, "one sees the row new: {one} {two}");
    assert!(racing.stores[0].is_denied(&host).expect("read"));
}

/// Proves: a heartbeat's record of what a host runs, on one coordinator, and a move of the host
/// to another group, on the other, at once: both land, whichever writes first; the host is in
/// the group it was moved to and runs what it said.
///
/// Catches: a host row read before the move and written back whole after it, undoing the move.
#[test]
fn a_move_on_one_task_is_kept_by_a_heartbeat_on_the_other() {
    let db = TestDatabase::new();
    let racing = Racing::new(&db);
    let id = racing.posted("m-1");
    racing.stores[0]
        .approve(&id, &id, &decision("web-1"))
        .expect("approved");
    let host = HostId::parse("web-1").expect("a host id");
    let runs = HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "1.0".to_owned(),
        services: Vec::new(),
    };
    let moved = Membership {
        host_group: "db".to_owned(),
        moving_from: None,
    };
    let (noted, set) = racing.at_once(
        |s| s.note_running(&host, &runs, NOW + 5),
        |s| s.set_membership(&host, moved.clone()),
    );
    noted.expect("noted");
    set.expect("moved");
    let enrolled = racing.stores[0]
        .enrolled_host(&host)
        .expect("read")
        .expect("enrolled");
    assert_eq!(enrolled.host_group, "db");
    assert_eq!(enrolled.running, Some(runs));
}

/// Proves: a write of a record its decision saw absent waits for another session that holds that
/// record's lock (as another coordinator's write does), and when that session wrote the record
/// meanwhile, the write is refused [`StoreError::Raced`] by the record's name and writes
/// nothing: a row that is not there yet is locked too.
///
/// Catches: a record seen absent written over one another coordinator wrote in between (two
/// keys approved for one new host id), which a row lock alone does not stop.
#[test]
fn a_record_seen_absent_and_written_meanwhile_is_refused_not_overwritten() {
    let db = TestDatabase::new();
    // Waits far longer than the test takes, so the write's wait ends by the release alone.
    let patient = Timeouts {
        statement: Duration::from_secs(600),
        lock: Duration::from_secs(300),
        ..TIMEOUTS
    };
    let (store, _) = db.open_with(patient, "records").expect("the store opens");
    let records = PgRecords::over(&store);
    let host = HostId::parse("web-1").expect("a host id");
    let place = records.place(&RecordKey::Host(host.clone()));
    let lock = format!("{RECORD_LOCK_CLASS}, hashtext('{}')", place.display());
    let other = Holding::start(
        &db,
        &format!("SELECT pg_advisory_lock({lock})"),
        &format!(
            "BEGIN; INSERT INTO hostkeeper.enrollment_hosts (host_id, key_hash, machine_id, \
             role, services, duplicate) VALUES ('web-1', '\\x{}', 'm-2', 'other', '{{}}', false); \
             COMMIT; SELECT pg_advisory_unlock({lock})",
            "07".repeat(32)
        ),
    );
    let written = pb::Host {
        schema: 1,
        host_id: "web-1".to_owned(),
        key_hash: vec![9; 32],
        machine_id: "m-1".to_owned(),
        role: "mine".to_owned(),
        ..pb::Host::default()
    };
    let answer = std::thread::scope(|scope| {
        let write = scope
            .spawn(|| records.commit(&[Seen::Host(host.clone(), None)], &[Record::Host(written)]));
        let waiting = format!(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
             AND classid = {RECORD_LOCK_CLASS} AND objsubid = 2 AND database = \
             (SELECT oid FROM pg_database WHERE datname = current_database())"
        );
        // A write that ended without waiting (refused at once) ends the loop too, and fails below.
        while db.rows(&waiting) != [[Some("1".to_owned())]] && !write.is_finished() {
            std::thread::yield_now();
        }
        other.release();
        write.join().expect("the write ends")
    });
    let refused = answer.expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "database:enrollment_hosts/web-1 was changed by another writer since it was read: \
         nothing is written"
    );
    assert_eq!(
        records.host(&host).expect("read").map(|h| h.role),
        Some("other".to_owned())
    );
}

/// Proves: a CI token id spent through one coordinator is refused `replayed` by the other, whose
/// memory never held it, because the database refuses the insert: the record of the second
/// answers that the id exists; a record that fails for another reason (the session lost) is
/// not answered as a replay.
///
/// Catches: a replay accepted because the spent ids that decide it are one coordinator's memory,
/// or one refused as the database being down.
#[test]
fn a_token_id_spent_on_one_task_is_refused_as_spent_on_the_other() {
    let db = TestDatabase::new();
    let (first, second) = two_tasks(&db);
    let clock: hostkeeper_oidc::Clock = Arc::new(|| super::SPENT_NOW);
    let spent = |store: Postgres| Spent::new(Arc::new(store) as Shared, Vec::new(), clock.clone());
    let (one, two) = (spent(first), spent(second));
    one.record("ci-run-1", super::SPENT_NOW + 600)
        .expect("spent on the first");
    let replayed = two
        .record("ci-run-1", super::SPENT_NOW + 600)
        .expect_err("refused on the second");
    assert_eq!(
        replayed.kind(),
        std::io::ErrorKind::AlreadyExists,
        "{replayed}"
    );
    two.record("ci-run-2", super::SPENT_NOW + 600)
        .expect("another id is spent");
    db.end_sessions();
    let lost = two
        .record("ci-run-3", super::SPENT_NOW + 600)
        .expect_err("the session is lost");
    assert_eq!(
        lost.kind(),
        std::io::ErrorKind::Other,
        "not a replay: {lost}"
    );
}
