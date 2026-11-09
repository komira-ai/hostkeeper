//! The records port (`enroll::records`): the conformance suite over the files and the fake, the
//! suite's own negative cases, and the store over each: what it keeps whatever keeps its
//! records, and what only the fake's all-or-nothing commit gives.

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::records::conformance::{self, Clause, Subject};
use hostkeeper_enrollment::enroll::records::{
    FileRecords, MemoryRecords, Record, RecordKey, Records, RecordsLock, pb,
};
use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, Claims, EnrollmentStore, FIRST_SERIAL, HostHeld, MacNonceHash,
    MacNonceRefusal, NewRequest, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::{FaultFs, Op};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use hostkeeper_testkit::Scratch;
use support::{HostKey, NOW, TestCa, decision, host_id, renewal, request, scratch};

/// The files of one scratch directory, through a fault-injecting file system.
struct Files {
    dir: Scratch,
    fs: Arc<FaultFs>,
}

impl Files {
    fn new(name: &str) -> Files {
        Files {
            dir: scratch(name),
            fs: Arc::new(FaultFs::new()),
        }
    }
}

impl Subject for Files {
    fn open(&self) -> Arc<dyn Records> {
        Arc::new(FileRecords::open(self.fs.clone(), self.dir.path()).expect("open the files"))
    }

    fn fail_next_commit(&self) {
        // A commit's first operation is the write of its first record's temporary file.
        self.fs.fail_at(self.fs.count() + 1);
    }
}

/// The fake: every open is the same records, as a restart over one database is.
struct Memory(Arc<MemoryRecords>);

impl Subject for Memory {
    fn open(&self) -> Arc<dyn Records> {
        self.0.clone()
    }

    fn fail_next_commit(&self) {
        self.0.fail_at(self.0.count() + 1);
    }
}

/// Catches the files or the fake breaking a clause of the contract: a record that does not
/// load back field for field, a list out of order or missing an unknown state, a serial taken
/// twice, a mark that changes another field, a failed or refused commit that writes, a lock
/// that cannot be taken again.
#[test]
fn the_records_conformance_suite_passes_on_the_files_and_the_fake() {
    let n = AtomicU64::new(0);
    let files = conformance::run(&|| {
        let at = n.fetch_add(1, Ordering::SeqCst);
        Box::new(Files::new(&format!("conformance-{at}")))
    });
    assert_eq!(files, Vec::new(), "the files");
    let memory = conformance::run(&|| Box::new(Memory(Arc::new(MemoryRecords::new()))));
    assert_eq!(memory, Vec::new(), "the fake");
}

/// One way records can be wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Defect {
    /// Loads a host that was never written.
    Ghost,
    /// Writes only the first record of a commit.
    HalfCommit,
    /// Keeps nothing across an open.
    Forgetful,
    /// Keeps the first record of a key; a later commit of it is dropped.
    FirstWins,
    /// Lists the requests backwards.
    Backwards,
    /// Leaves a request of an unknown state out of the list.
    DropsUnknown,
    /// Lists the hosts in the order they were written.
    Unsorted,
    /// Hands out the same serial every time.
    SameSerial,
    /// Takes a serial without writing the request it reserves it in.
    ForgetsReservation,
    /// The duplicate mark also clears the first heartbeat.
    WideMark,
    /// The first serving heartbeat also clears the first heartbeat.
    WideServing,
    /// What the host runs also clears the first serving heartbeat.
    WideRunning,
    /// A commit told to fail writes anyway.
    WritesWhenFailing,
    /// A commit with a record whose key does not parse writes the records before it.
    KeepsBeforeUnkeyed,
    /// The lock is taken once only.
    LockOnce,
    /// Every operation fails.
    Down,
}

/// The fake with one defect.
#[derive(Debug)]
struct Broken {
    inner: Arc<MemoryRecords>,
    defect: Defect,
    /// The hosts in the order they were first written ([`Defect::Unsorted`]).
    written: Mutex<Vec<HostId>>,
    /// Every key committed ([`Defect::FirstWins`]).
    seen: Mutex<Vec<RecordKey>>,
    locks: AtomicU64,
    /// The next commit is told to fail ([`Subject::fail_next_commit`]).
    failing: Arc<Mutex<bool>>,
}

impl Broken {
    fn down<T>(&self) -> Result<T, StoreError> {
        Err(StoreError::UnknownHost(host_id("down")))
    }

    fn up(&self) -> Result<(), StoreError> {
        if self.defect == Defect::Down {
            return self.down();
        }
        Ok(())
    }
}

impl Records for Broken {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        self.up()?;
        if self.defect == Defect::LockOnce && self.locks.fetch_add(1, Ordering::SeqCst) > 0 {
            return self.down();
        }
        self.inner.lock()
    }

    fn place(&self, key: &RecordKey) -> PathBuf {
        self.inner.place(key)
    }

    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        self.up()?;
        self.inner.request(id)
    }

    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        self.up()?;
        let mut listed = self.inner.requests()?;
        match self.defect {
            Defect::Backwards => listed.reverse(),
            Defect::DropsUnknown => listed.retain(|(_, request)| request.state <= 5),
            _ => {}
        }
        Ok(listed)
    }

    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        self.up()?;
        self.inner.issued(id)
    }

    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        self.up()?;
        let found = self.inner.host(host)?;
        if self.defect == Defect::Ghost && found.is_none() {
            return Ok(Some(pb::Host::default()));
        }
        Ok(found)
    }

    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        self.up()?;
        if self.defect == Defect::Unsorted {
            return Ok(self
                .written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone());
        }
        self.inner.host_ids()
    }

    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        self.up()?;
        self.inner.deny(host)
    }

    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        self.up()?;
        self.inner.mac_nonce(hash)
    }

    fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        self.up()?;
        let failing =
            std::mem::take(&mut *self.failing.lock().unwrap_or_else(PoisonError::into_inner));
        let mut kept: Vec<Record> = records.to_vec();
        match self.defect {
            Defect::HalfCommit => kept.truncate(1),
            Defect::FirstWins => {
                let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
                kept.retain(|record| {
                    let Ok(key) = record.key() else {
                        return true;
                    };
                    let new = !seen.contains(&key);
                    seen.push(key);
                    new
                });
            }
            Defect::KeepsBeforeUnkeyed => {
                let good = kept
                    .iter()
                    .take_while(|record| record.key().is_ok())
                    .cloned()
                    .collect::<Vec<_>>();
                if good.len() < kept.len() {
                    self.inner.commit(&good)?;
                }
            }
            _ => {}
        }
        for record in &kept {
            if let Ok(RecordKey::Host(host)) = record.key() {
                let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
                if !written.contains(&host) {
                    written.push(host);
                }
            }
        }
        if failing {
            if self.defect == Defect::WritesWhenFailing {
                self.inner.commit(&kept)?;
            }
            return self.down();
        }
        self.inner.commit(&kept)
    }

    fn take_serial(&self, reserving: Option<&pb::Request>) -> Result<u64, StoreError> {
        self.up()?;
        match self.defect {
            Defect::SameSerial => Ok(FIRST_SERIAL),
            Defect::ForgetsReservation => self.inner.take_serial(None),
            _ => self.inner.take_serial(reserving),
        }
    }

    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.up()?;
        self.inner.set_first_heartbeat(host, at_unix)
    }

    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.up()?;
        if self.defect == Defect::WideServing {
            let cleared = pb::Host {
                first_heartbeat_unix: None,
                ..host.clone()
            };
            return self.inner.set_first_serving(&cleared, at_unix);
        }
        self.inner.set_first_serving(host, at_unix)
    }

    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        self.up()?;
        if self.defect == Defect::WideRunning {
            let cleared = pb::Host {
                first_serving_unix: None,
                ..host.clone()
            };
            return self.inner.set_running(&cleared, running);
        }
        self.inner.set_running(host, running)
    }

    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        self.up()?;
        if self.defect == Defect::WideMark {
            let cleared = pb::Host {
                first_heartbeat_unix: None,
                ..host.clone()
            };
            return self.inner.set_duplicate(&cleared);
        }
        self.inner.set_duplicate(host)
    }
}

/// A subject over the fake with `defect`.
struct BrokenSubject {
    inner: Mutex<Arc<MemoryRecords>>,
    defect: Defect,
    failing: Arc<Mutex<bool>>,
    opened: Mutex<Option<Arc<Broken>>>,
}

impl Subject for BrokenSubject {
    fn open(&self) -> Arc<dyn Records> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if self.defect == Defect::Forgetful {
            *inner = Arc::new(MemoryRecords::new());
        }
        let mut opened = self.opened.lock().unwrap_or_else(PoisonError::into_inner);
        let keep = opened
            .as_ref()
            .filter(|_| self.defect != Defect::Forgetful)
            .cloned();
        let broken = keep.unwrap_or_else(|| {
            Arc::new(Broken {
                inner: inner.clone(),
                defect: self.defect,
                written: Mutex::new(Vec::new()),
                seen: Mutex::new(Vec::new()),
                locks: AtomicU64::new(0),
                failing: self.failing.clone(),
            })
        });
        *opened = Some(broken.clone());
        broken
    }

    fn fail_next_commit(&self) {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// Catches a suite that passes records wrong in a way it claims to catch: each defect breaks
/// the clause named beside it, and records that are down break every clause.
#[test]
fn the_records_suite_fails_against_records_that_break_each_clause() {
    let cases = [
        (Defect::Ghost, Clause::Empty),
        (Defect::HalfCommit, Clause::RoundTrip),
        (Defect::Forgetful, Clause::RoundTrip),
        (Defect::FirstWins, Clause::Overwrite),
        (Defect::Backwards, Clause::RequestsInOrder),
        (Defect::DropsUnknown, Clause::RequestsInOrder),
        (Defect::Unsorted, Clause::HostIdsSorted),
        (Defect::SameSerial, Clause::Serial),
        (Defect::ForgetsReservation, Clause::Serial),
        (Defect::WideMark, Clause::HostMarks),
        (Defect::WideServing, Clause::HostMarks),
        (Defect::WideRunning, Clause::HostMarks),
        (Defect::WritesWhenFailing, Clause::FailedCommit),
        (Defect::KeepsBeforeUnkeyed, Clause::Unkeyed),
        (Defect::LockOnce, Clause::Lock),
    ];
    let suite = |defect: Defect| {
        conformance::run(&|| {
            Box::new(BrokenSubject {
                inner: Mutex::new(Arc::new(MemoryRecords::new())),
                defect,
                failing: Arc::new(Mutex::new(false)),
                opened: Mutex::new(None),
            })
        })
    };
    for (defect, clause) in cases {
        let failures = suite(defect);
        assert!(
            failures.iter().any(|failure| failure.clause == clause),
            "{defect:?} should break {clause:?}: {failures:?}"
        );
    }
    let down: Vec<Clause> = suite(Defect::Down).iter().map(|f| f.clause).collect();
    assert_eq!(
        down,
        [
            Clause::Empty,
            Clause::RoundTrip,
            Clause::Overwrite,
            Clause::RequestsInOrder,
            Clause::HostIdsSorted,
            Clause::Serial,
            Clause::HostMarks,
            Clause::FailedCommit,
            Clause::Unkeyed,
            Clause::Lock,
        ]
    );
    let failure = &suite(Defect::Down)[0];
    assert_eq!(
        failure.to_string(),
        "Empty: a load: no host down was approved"
    );
}

/// A store over the files of `name` or over a fake, opened again as often as a test restarts it.
enum Kept {
    Files(Files),
    Memory(Arc<MemoryRecords>),
}

impl Kept {
    fn both(name: &str) -> [Kept; 2] {
        [
            Kept::Files(Files::new(name)),
            Kept::Memory(Arc::new(MemoryRecords::new())),
        ]
    }

    /// The store, opened (again).
    fn store(&self) -> EnrollmentStore {
        match self {
            Kept::Files(files) => {
                EnrollmentStore::open(files.fs.clone(), files.dir.path()).expect("open")
            }
            Kept::Memory(records) => EnrollmentStore::new(records.clone()),
        }
    }
}

/// Catches a certificate signed twice, or a serial taken twice, for one request and round,
/// over either records: the first issue signs, a repeat and a repeat after a restart give the
/// same bytes and serial and are not the first, a second request gets the next serial, and a
/// renewal's round gets a serial of its own.
#[test]
fn one_certificate_and_one_serial_per_request_and_round_over_the_files_and_the_fake() {
    let ca = TestCa::new("records-one-ca");
    for kept in Kept::both("records-one") {
        let store = kept.store();
        let key = HostKey::new();
        let id = store
            .post(request(&key, "m-1"), NOW)
            .expect("post")
            .request_id;
        store
            .approve(&id, &id, &decision("web-1", false))
            .expect("approve");
        let first = store.issue(&ca.ca, &id).expect("issue");
        assert!(first.first);
        assert_eq!(first.serial, FIRST_SERIAL);
        let again = store.issue(&ca.ca, &id).expect("issue again");
        let restarted = kept
            .store()
            .issue(&ca.ca, &id)
            .expect("issue after a restart");
        for repeat in [again, restarted] {
            assert!(!repeat.first);
            assert_eq!(repeat.certificate_der, first.certificate_der);
            assert_eq!(repeat.serial, first.serial);
        }

        let store = kept.store();
        let other = HostKey::new();
        let second = store
            .post(request(&other, "m-2"), NOW)
            .expect("post")
            .request_id;
        store
            .approve(&second, &second, &decision("web-2", false))
            .expect("approve");
        assert_eq!(
            store.issue(&ca.ca, &second).expect("issue").serial,
            FIRST_SERIAL + 1
        );

        let renewed = store
            .post(renewal(&key, "m-1", "web-1"), NOW)
            .expect("renew");
        assert_eq!((renewed.request_id, renewed.round), (id, 1));
        store
            .approve(&id, &id, &decision("web-1", false))
            .expect("approve the renewal");
        let next = store.issue(&ca.ca, &id).expect("issue the renewal");
        assert!(next.first);
        assert_eq!((next.round, next.serial), (1, FIRST_SERIAL + 2));
        assert_ne!(next.certificate_der, first.certificate_der);
        assert_eq!(store.allocate_serial().expect("serial"), FIRST_SERIAL + 3);
    }
}

const SERIAL: &str = "C02RECORDS01";

/// A Mac's request of `key` claiming [`SERIAL`], which presented a nonce.
fn mac_request(key: &HostKey) -> NewRequest {
    NewRequest {
        claims: Claims {
            mac_serial: SERIAL.to_owned(),
            ..support::claims("m-mac")
        },
        mac_nonce_presented: true,
        ..request(key, "m-mac")
    }
}

fn by_nonce(hash: MacNonceHash) -> ApprovalRecord {
    ApprovalRecord {
        approver: "mdm-nonce".to_owned(),
        approved_unix: NOW,
        reason: "approved by its MDM nonce".to_owned(),
        mac_nonce: Some(hash),
        mac_enrolled_since_unix: None,
    }
}

/// Whether the request `id` is approved and whether the nonce `hash` is used, as `store` reads
/// them: what an approval by nonce commits together.
fn approval_and_nonce(store: &EnrollmentStore, id: &KeyHash, hash: &MacNonceHash) -> (bool, bool) {
    let approved =
        store.request(id).expect("read").map(|r| r.state) == Some(RequestState::Approved);
    let other = HostKey::new();
    let probe = NewRequest {
        claims: Claims {
            machine_id: format!("m-probe-{}", other.key_hash()),
            ..mac_request(&other).claims
        },
        ..mac_request(&other)
    };
    let probe = store.post(probe, NOW).expect("post").request_id;
    // A nonce issued for this request only: used, or wrong request (so still unused).
    let used = match store.approve_recorded(
        &probe,
        &probe,
        &decision("mac-probe", false),
        &by_nonce(*hash),
    ) {
        Err(ApproveError::MacNonce(MacNonceRefusal::Used)) => true,
        Err(ApproveError::MacNonce(MacNonceRefusal::WrongRequest)) => false,
        other => panic!("the probe answered {other:?}"),
    };
    (approved, used)
}

/// Catches an approval by Mac nonce landing without its nonce consumed, or the nonce consumed
/// without the approval. On the fake every failure of the approval's commit leaves neither, and
/// the retry lands both. On the files the request is the commit point: a failed write of the
/// nonce's file still approves and still consumes the nonce, in memory and, after a restart,
/// in the file the next open marks.
#[test]
fn an_approval_and_its_nonce_land_together() {
    let hash = MacNonceHash::of(b"records-nonce-0123456789abcdef01");
    // The fake: fail each operation of the approval in turn.
    let mut n = 1;
    loop {
        let records = Arc::new(MemoryRecords::new());
        let store = EnrollmentStore::new(records.clone());
        let key = HostKey::new();
        let id = store.post(mac_request(&key), NOW).expect("post").request_id;
        assert!(
            store
                .issue_mac_nonce(SERIAL, &id, &hash, NOW)
                .expect("nonce")
        );
        records.fail_at(records.count() + n);
        let approved = store.approve_recorded(&id, &id, &decision("mac-1", false), &by_nonce(hash));
        records.fail_at(0);
        let landed = approval_and_nonce(&store, &id, &hash);
        if approved.is_ok() {
            assert_eq!(landed, (true, true), "operation {n}");
            break;
        }
        assert_eq!(landed, (false, false), "operation {n}: {approved:?}");
        store
            .approve_recorded(&id, &id, &decision("mac-1", false), &by_nonce(hash))
            .expect("the retry");
        assert_eq!(
            approval_and_nonce(&store, &id, &hash),
            (true, true),
            "retry {n}"
        );
        n += 1;
    }
    assert!(n > 3, "the approval reads before it commits");

    // The files: fail the write of the nonce's file, the last of the commit.
    let files = Files::new("records-nonce");
    let store = EnrollmentStore::open(files.fs.clone(), files.dir.path()).expect("open");
    let key = HostKey::new();
    let id = store.post(mac_request(&key), NOW).expect("post").request_id;
    assert!(
        store
            .issue_mac_nonce(SERIAL, &id, &hash, NOW)
            .expect("nonce")
    );
    let nonce_file = files
        .dir
        .path()
        .join("mac-nonces")
        .join(format!("{}.pb", hex::encode(hash.as_bytes())));
    let probe = Files::new("records-nonce-probe");
    let probe_store = EnrollmentStore::open(probe.fs.clone(), probe.dir.path()).expect("open");
    let probe_id = probe_store
        .post(mac_request(&key), NOW)
        .expect("post")
        .request_id;
    probe_store
        .issue_mac_nonce(SERIAL, &probe_id, &hash, NOW)
        .expect("nonce");
    let base = probe.fs.count();
    probe_store
        .approve_recorded(
            &probe_id,
            &probe_id,
            &decision("mac-1", false),
            &by_nonce(hash),
        )
        .expect("approve");
    let ops = probe.fs.ops();
    let at = ops[usize::try_from(base).expect("small")..]
        .iter()
        .position(|op| matches!(op, Op::WriteNew(path) if path.parent().is_some_and(|d| d.ends_with("mac-nonces"))))
        .expect("the nonce's file is written");
    files
        .fs
        .fail_at(files.fs.count() + 1 + u64::try_from(at).expect("small"));
    let (approved, events) = support::capture(|| {
        store.approve_recorded(&id, &id, &decision("mac-1", false), &by_nonce(hash))
    });
    approved.expect("the approval stands");
    assert!(events.iter().any(|event| {
        event.get("message").is_some_and(|m| {
            m == "a consumed Mac nonce's file was not marked: the approval holds the consumption"
        })
    }));
    assert_eq!(approval_and_nonce(&store, &id, &hash), (true, true));
    let unmarked = std::fs::read(&nonce_file).expect("the nonce's file");
    let restarted = EnrollmentStore::open(files.fs.clone(), files.dir.path()).expect("reopen");
    assert_eq!(approval_and_nonce(&restarted, &id, &hash), (true, true));
    assert_ne!(
        std::fs::read(&nonce_file).expect("the nonce's file"),
        unmarked,
        "marked at open"
    );
}

/// Catches the fake's commit writing part of an approval: an approval that clears a deny row
/// writes the deny row, the host and the request in one commit, so whichever of its operations
/// fails, the commit among them, the host id is still denied, no host is enrolled and the
/// request still waits; the approval then lands all three.
#[test]
fn a_failed_commit_on_the_fake_leaves_every_record_as_it_was() {
    let mut n = 1;
    loop {
        let records = Arc::new(MemoryRecords::new());
        let store = EnrollmentStore::new(records.clone());
        let old = HostKey::new();
        let first = store
            .post(request(&old, "m-1"), NOW)
            .expect("post")
            .request_id;
        store
            .approve(&first, &first, &decision("web-1", false))
            .expect("approve");
        assert!(store.deny(&host_id("web-1")).expect("deny"));
        let key = HostKey::new();
        let id = store
            .post(request(&key, "m-2"), NOW)
            .expect("post")
            .request_id;
        records.fail_at(records.count() + n);
        let approved = store.approve(&id, &id, &decision("web-1", true));
        records.fail_at(0);
        let web = host_id("web-1");
        if let Ok(approval) = approved {
            assert!(approval.cleared_deny);
            assert_eq!(store.held(&web).expect("held"), HostHeld::Approved);
            break;
        }
        let err = approved.expect_err("failed");
        assert!(
            err.to_string().contains("injected failure at operation"),
            "{err}"
        );
        assert_eq!(
            store.held(&web).expect("held"),
            HostHeld::Denied,
            "operation {n}"
        );
        assert!(
            store.enrolled_host(&web).expect("read").is_none(),
            "operation {n}"
        );
        assert_eq!(
            store.request(&id).expect("read").map(|r| r.state),
            Some(RequestState::Pending),
            "operation {n}"
        );
        n += 1;
    }
    assert!(n > 3, "the approval reads before it commits");
}

/// Catches an error about a record that does not say where the record is: the files name its
/// path in the store's directory, the fake a name of its kind and key.
#[test]
fn each_record_is_named_where_it_is_kept() {
    let files = Files::new("records-places");
    let opened = files.open();
    let id = KeyHash::of_spki(b"a key");
    let hash = MacNonceHash::of(b"a nonce");
    let nonce = hex::encode(hash.as_bytes());
    let keys = [
        RecordKey::Request(id),
        RecordKey::Issued(id),
        RecordKey::Host(host_id("web-1")),
        RecordKey::Deny(host_id("web-1")),
        RecordKey::MacNonce(hash),
        RecordKey::Serial,
    ];
    let dir = files.dir.path();
    let in_files: Vec<PathBuf> = keys.iter().map(|key| opened.place(key)).collect();
    assert_eq!(
        in_files,
        [
            dir.join(format!("requests/{id}.pb")),
            dir.join(format!("issued/{id}.pb")),
            dir.join("hosts/web-1.pb"),
            dir.join("deny/web-1.pb"),
            dir.join(format!("mac-nonces/{nonce}.pb")),
            dir.join("serial.pb"),
        ]
    );
    let memory = MemoryRecords::new();
    let in_memory: Vec<PathBuf> = keys.iter().map(|key| memory.place(key)).collect();
    assert_eq!(
        in_memory,
        [
            PathBuf::from(format!("memory/requests/{id}")),
            PathBuf::from(format!("memory/issued/{id}")),
            PathBuf::from("memory/hosts/web-1"),
            PathBuf::from("memory/deny/web-1"),
            PathBuf::from(format!("memory/mac-nonces/{nonce}")),
            PathBuf::from("memory/serial"),
        ]
    );
    assert_eq!(
        format!(
            "{:?}",
            EnrollmentStore::open(files.fs.clone(), dir).expect("open")
        ),
        format!("EnrollmentStore(FileRecords({}))", dir.display())
    );
    assert!(
        format!("{:?}", EnrollmentStore::new(Arc::new(memory)))
            .starts_with("EnrollmentStore(MemoryRecords {"),
    );
}

/// Catches the files swallowing the failed write of a consumed Mac nonce whose approval is not
/// the request committed before it: only the approval that names the nonce makes its file a
/// write that may fail quietly. A nonce marked consumed alone, or after a request that is not
/// its approval, fails the commit when its file cannot be written.
#[test]
fn the_files_let_a_nonce_mark_fail_quietly_only_after_its_own_approval() {
    let files = Files::new("records-quiet");
    let records = files.open();
    let id = KeyHash::of_spki(b"the consumer");
    let hash = MacNonceHash::of(b"the nonce");
    let nonce = pb::MacNonce {
        schema: 1,
        nonce_sha256: hash.as_bytes().to_vec(),
        serial: SERIAL.to_owned(),
        issued_unix: NOW,
        consumed_by: id.as_bytes().to_vec(),
        request_id: id.as_bytes().to_vec(),
    };
    let approval = |request: &KeyHash, nonce: &MacNonceHash| {
        Record::Request(pb::Request {
            schema: 1,
            request_id: request.as_bytes().to_vec(),
            approved: Some(pb::Approval {
                mac_nonce_sha256: nonce.as_bytes().to_vec(),
                ..pb::Approval::default()
            }),
            ..pb::Request::default()
        })
    };
    let other = KeyHash::of_spki(b"another request");
    let before = [
        Vec::new(),
        vec![approval(&other, &hash)],
        vec![approval(&id, &MacNonceHash::of(b"another nonce"))],
        vec![Record::Request(pb::Request {
            schema: 1,
            request_id: id.as_bytes().to_vec(),
            ..pb::Request::default()
        })],
    ];
    for records_before in before {
        let mut commit = records_before.clone();
        commit.push(Record::MacNonce(nonce.clone()));
        // The nonce's write is the last; its temporary file is the first operation of it.
        let writes = u64::try_from(4 * records_before.len()).expect("small");
        files.fs.fail_at(files.fs.count() + writes + 1);
        let refused = records
            .commit(&commit)
            .expect_err("the nonce's write fails");
        assert!(
            refused.to_string().contains("injected failure"),
            "{refused}"
        );
        assert_eq!(
            records.mac_nonce(&hash).expect("read"),
            None,
            "{records_before:?}"
        );
    }
    // Its own approval before it: the commit stands, and the nonce loads consumed.
    files.fs.fail_at(files.fs.count() + 5);
    records
        .commit(&[approval(&id, &hash), Record::MacNonce(nonce.clone())])
        .expect("the approval stands");
    assert_eq!(
        records.mac_nonce(&hash).expect("read"),
        None,
        "its file was not written"
    );
    let unconsumed = pb::MacNonce {
        consumed_by: Vec::new(),
        ..nonce.clone()
    };
    records
        .commit(&[Record::MacNonce(unconsumed)])
        .expect("write the nonce unmarked");
    assert_eq!(
        records.mac_nonce(&hash).expect("read"),
        Some(nonce),
        "remembered"
    );
}

/// Catches a deny or a retire that, when the host's request cannot be read, writes nothing and
/// so leaves the host enrolled: over the files and the fake, the call is refused naming the
/// missing request, and the host is denied all the same (retiring after a deny, retired after a
/// retire).
#[test]
fn a_deny_whose_request_cannot_be_read_still_denies_the_host() {
    for (call, held) in [("deny", HostHeld::Denied), ("retire", HostHeld::Retired)] {
        let missing = KeyHash::of_spki(b"a key whose request is gone");
        let orphan = pb::Host {
            schema: 1,
            host_id: "web-1".to_owned(),
            key_hash: missing.as_bytes().to_vec(),
            machine_id: "m-1".to_owned(),
            role: "web".to_owned(),
            services: vec!["api".to_owned()],
            duplicate: false,
            first_heartbeat_unix: None,
            first_serving_unix: None,
            running: None,
        };
        let files = Files::new("records-orphan");
        let memory: Arc<dyn Records> = Arc::new(MemoryRecords::new());
        for records in [files.open(), memory] {
            records
                .commit(&[Record::Host(orphan.clone())])
                .expect("write the host");
            let store = EnrollmentStore::new(records.clone());
            let web = host_id("web-1");
            let refused = match call {
                "deny" => store.deny(&web).map(|_| ()),
                _ => store.retire(&web),
            }
            .expect_err("the host's request is missing");
            let host_place = records.place(&RecordKey::Host(web.clone()));
            assert_eq!(
                refused.to_string(),
                format!(
                    "{} is not a store file this release reads: the host's request is missing",
                    host_place.display()
                ),
                "{call} over {records:?}"
            );
            assert_eq!(
                store.held(&web).expect("held"),
                held,
                "{call} over {records:?}"
            );
            assert!(
                store.is_denied(&web).expect("denied"),
                "{call} over {records:?}"
            );
        }
    }
}
