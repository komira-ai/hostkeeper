//! Waits between the coordinator's own transactions: the purge of the ledger every transaction
//! makes never waits for another transaction's purge, a heartbeat makes no write beside its open
//! decision, and a `COMMIT` cancelled is a commit of unknown outcome.
//!
//! A wait that must run out is the server's own bounded wait, set short; a wait that must not
//! happen is one the test holds open from a client of its own until its assertions are made.
//! Nothing sleeps and no time is measured.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_enrollment::enroll::records::{Record, RecordKey, Records, RecordsLock, Seen, pb};
use hostkeeper_enrollment::enroll::{Claims, Decision, MacNonceHash, NewRequest, StoreError};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role};

use super::coordinator::{DAEMON, Farm, daemon, waits};
use super::{TIMEOUTS, TestDatabase};
use crate::enrollment::tests::support::{HostKey, NOW};
use crate::store::conformance_tests::lines;
use crate::store::postgres::dsn::Timeouts;
use crate::store::postgres::writer::{Fault, WAITED, WriteFail};
use crate::store::postgres::{PgRecords, Postgres, decide};
use crate::store::{Backend, BackendError, Commit, Health};

/// Waits short enough that a write which waits for a lock the test holds is refused within the
/// test, and long enough that a write which waits for nothing never meets them.
const SHORT: Timeouts = Timeouts {
    statement: Duration::from_secs(2),
    lock: Duration::from_millis(500),
    ..TIMEOUTS
};

/// Commits the audit line `seq`.
fn line(store: &Postgres, seq: u64) -> Result<(), BackendError> {
    store.commit(&Commit {
        audit: &lines(seq, seq),
        ..Commit::default()
    })
}

/// The database's clock now, in seconds.
fn database_now(db: &TestDatabase) -> i64 {
    db.rows("SELECT extract(epoch FROM now())::bigint")[0][0]
        .as_deref()
        .and_then(|now| now.parse().ok())
        .expect("the database's clock")
}

/// The ledger's rows that began over an hour ago, by id.
fn expired(db: &TestDatabase) -> Vec<String> {
    db.rows(
        "SELECT id::text FROM tx_ledger \
         WHERE at_unix < extract(epoch FROM now())::bigint - 3600 ORDER BY id",
    )
    .into_iter()
    .flatten()
    .flatten()
    .collect()
}

/// Proves: a commit made while another transaction holds expired rows of the ledger (it deleted
/// them and has not ended) lands, and the store stays up: its purge skips the rows held and
/// deletes the expired row nothing holds; once the holder rolls back, the next commit deletes the
/// rows it held.
///
/// Catches: a purge that waits for another transaction's purge, so a write waits for whatever
/// that transaction waits for (a heartbeat's open decision) until its lock timeout refuses it.
#[test]
fn a_commit_purges_around_the_ledger_rows_another_transaction_holds() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db
        .open_with(SHORT, "purge-around")
        .expect("the store opens");
    let held = "00000000-0000-0000-0000-0000000000c1";
    let free = "00000000-0000-0000-0000-0000000000c2";
    db.run(&format!(
        "DELETE FROM tx_ledger; \
         INSERT INTO tx_ledger (id, at_unix) VALUES ('{held}', extract(epoch FROM now())::bigint - 7200)"
    ))
    .expect("the ledger holds the expired row");
    let mut holder = db.client();
    holder
        .batch_execute(
            "BEGIN; DELETE FROM hostkeeper.tx_ledger \
             WHERE at_unix < extract(epoch FROM now())::bigint - 3600",
        )
        .expect("the holder deletes the expired row, and holds it");
    db.run(&format!(
        "INSERT INTO tx_ledger (id, at_unix) VALUES ('{free}', extract(epoch FROM now())::bigint - 7200)"
    ))
    .expect("a second expired row, which nothing holds");

    line(&store, 1).expect("the commit lands beside the holder");
    assert_eq!(store.health(), Health::Up);
    assert_eq!(
        expired(&db),
        vec![held.to_owned()],
        "the row nothing held is deleted, the held one is left"
    );

    holder.batch_execute("ROLLBACK").expect("the holder ends");
    line(&store, 2).expect("the next commit lands");
    assert_eq!(expired(&db), Vec::<String>::new(), "and deletes it");
    assert_eq!(store.health(), Health::Up);
}

/// Proves: one commit deletes at most 100 expired rows of the ledger, the oldest first: of 150
/// rows a second apart, the 50 youngest are left, and a row a minute old is kept.
///
/// Catches: a purge that deletes a whole backlog in one transaction (made long by it), and one
/// that deletes the rows nearest the hour first, so the oldest are kept longest.
#[test]
fn a_commit_purges_at_most_a_batch_of_the_ledger_the_oldest_first() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("purge-batch").expect("the store opens");
    let base = database_now(&db) - 7200;
    db.run(&format!(
        "DELETE FROM tx_ledger; \
         INSERT INTO tx_ledger (at_unix) SELECT {base} - i FROM generate_series(0, 149) AS i; \
         INSERT INTO tx_ledger (at_unix) VALUES ({})",
        base + 7140
    ))
    .expect("the ledger holds the seeded rows");

    line(&store, 1).expect("an ordinary commit");

    let left: Vec<i64> = db
        .rows(&format!(
            "SELECT at_unix FROM tx_ledger WHERE at_unix <= {base} ORDER BY at_unix DESC"
        ))
        .into_iter()
        .flatten()
        .flatten()
        .map(|at| at.parse().expect("a number"))
        .collect();
    assert_eq!(left, (0..50).map(|i| base - i).collect::<Vec<_>>());
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM tx_ledger WHERE at_unix = {}",
            base + 7140
        )),
        vec![vec![Some("1".to_owned())]],
        "the row a minute old is kept"
    );
}

/// Proves: a commit whose `COMMIT` the server cancels is refused as of unknown outcome, with the
/// server's exact text: the store is lost with it unsettled, and once revived it is settled as
/// not landed, its audit line nowhere.
///
/// Catches: a cancelled `COMMIT` taken as a plain refusal that keeps the session, which would
/// call a commit that may have landed not made.
#[test]
fn a_commit_whose_commit_is_cancelled_is_of_unknown_outcome() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("cancelled-commit").expect("the store opens");
    store.fault(Fault::CancelAtCommit);
    assert_eq!(
        line(&store, 1).expect_err("not known").to_string(),
        "database:database: ERROR: canceling statement due to user request (SQLSTATE 57014)"
    );
    assert_eq!(store.health(), Health::Lost);
    assert!(store.unsettled());

    store.revive();
    store.settle().expect("the writers run");
    assert_eq!(store.health(), Health::Up);
    let landing = store.take_landings().pop().expect("the commit is settled");
    assert!(!landing.landed);
    assert_eq!(
        db.rows("SELECT seq FROM audit"),
        Vec::<Vec<Option<String>>>::new()
    );
}

/// Proves: a write whose wait for a lock runs out (the audit table locked by another session past
/// the store's `lock_timeout`) is refused with the exact text that says another transaction held
/// what it waited for, and the server's, and writes nothing; the store stays `Up`, and once the
/// lock is released the next write lands on the same session, with no revival.
///
/// Catches: a lock timeout that drops the session, and a refusal that does not say it waited for
/// another transaction (an operator's `store_failed` the log cannot explain).
#[test]
fn a_write_whose_lock_wait_runs_out_is_refused_with_what_it_waited_for() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open_with(SHORT, "lock-wait").expect("the store opens");
    let mut holder = db.client();
    holder
        .batch_execute("BEGIN; LOCK TABLE hostkeeper.audit IN ACCESS EXCLUSIVE MODE")
        .expect("the holder locks the audit table");

    assert_eq!(
        line(&store, 1).expect_err("refused").to_string(),
        "database:database: a statement of this write ran out its bounded wait (for a lock \
         another transaction held, or for its own run): nothing of it is written, and the \
         session stays: ERROR: canceling statement due to lock timeout (SQLSTATE 55P03)"
    );
    assert_eq!(store.health(), Health::Up);

    holder.batch_execute("ROLLBACK").expect("the holder ends");
    assert_eq!(
        db.rows("SELECT seq FROM audit"),
        Vec::<Vec<Option<String>>>::new()
    );
    line(&store, 2).expect("the next write lands");
    assert_eq!(
        db.rows("SELECT seq FROM audit"),
        vec![vec![Some("2".to_owned())]]
    );
    assert_eq!(store.health(), Health::Up);
}

/// Proves: one statement made outside a transaction (what a heartbeat's row is written with)
/// whose wait for a lock runs out is refused with the exact text that says it waited, and the
/// session stays: the store is `Up`, and once the lock is released the same writer's next
/// statement answers.
///
/// Catches: a heartbeat's row whose lock timeout drops the session, so every write after it is
/// refused until a revival.
#[test]
fn a_statement_alone_whose_lock_wait_runs_out_is_refused_and_keeps_the_session() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open_with(SHORT, "once-wait").expect("the store opens");
    let read = || {
        store
            .on_writer(|writer| {
                writer.once(|session| {
                    session
                        .query_one("SELECT count(*) FROM fleet_hosts", &[])
                        .map(|row| row.get::<_, i64>(0))
                })
            })
            .expect("the writer runs")
    };
    let mut holder = db.client();
    holder
        .batch_execute("BEGIN; LOCK TABLE hostkeeper.fleet_hosts IN ACCESS EXCLUSIVE MODE")
        .expect("the holder locks the fleet's rows");

    assert_eq!(
        read(),
        Err(WriteFail::Refused(format!(
            "{WAITED}: ERROR: canceling statement due to lock timeout (SQLSTATE 55P03)"
        )))
    );
    assert_eq!(store.health(), Health::Up);

    holder.batch_execute("ROLLBACK").expect("the holder ends");
    assert_eq!(read(), Ok(0));
    assert_eq!(store.health(), Health::Up);
}

/// Approves `host` (machine `m-1`, the role [`DAEMON`]) in the farm's enrollment.
fn approve(farm: &Farm, host: &str) {
    let enrollment = farm.enrollment.as_ref().expect("enrollment");
    let key = HostKey::new();
    let posted = enrollment
        .store()
        .post(
            NewRequest {
                csr_der: key.csr(),
                claims: Claims {
                    machine_id: "m-1".to_owned(),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            NOW,
        )
        .expect("posted");
    enrollment
        .store()
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &Decision {
                host_id: HostId::parse(host).expect("a host id"),
                role: Role::parse(DAEMON).expect("a role"),
                services: Vec::new(),
                replace: false,
                host_group: None,
            },
        )
        .expect("approved");
}

/// A heartbeat of `host` from its daemon on the machine `m-1`, running `web` and `db` at `at`.
fn beat(farm: &Farm, host: &str, at: &str) {
    farm.rig
        .coordinator
        .heartbeat_from(
            crate::marks_support::who(host, DAEMON),
            daemon(at),
            Some("m-1"),
        )
        .expect("answered");
}

/// The version of each service the enrollment of `host` keeps as what it runs.
fn kept_running(farm: &Farm, host: &str) -> Vec<(String, String)> {
    let enrollment = farm.enrollment.as_ref().expect("enrollment");
    let enrolled = enrollment
        .store()
        .enrolled_host(&HostId::parse(host).expect("a host id"))
        .expect("read")
        .expect("enrolled");
    enrolled
        .running
        .expect("a record of what it runs")
        .services
        .into_iter()
        .map(|one| (one.service, one.version))
        .collect()
}

/// Proves: with expired rows in the ledger, a heartbeat that changes what its host runs is
/// decided and recorded, and the store stays up: the host's enrollment keeps what it runs now.
/// This is the heartbeat whose decision deleted the expired rows in its own transaction while
/// the record of what the host runs waited for them on another session.
///
/// Catches: a heartbeat's write that waits for its own open decision until its lock timeout
/// refuses it and drops the session, so every write of the coordinator, an operator's change
/// among them, is refused until a revival.
#[tokio::test]
async fn a_heartbeat_beside_expired_ledger_rows_records_what_its_host_runs_and_stays_up() {
    super::logging();
    let farm = Farm::start_over("pg-beside", SHORT, |store| {
        Some(Arc::new(PgRecords::over(store)) as Arc<dyn Records>)
    })
    .seeded()
    .await;
    approve(&farm, "d1");
    beat(&farm, "d1", "1.0");
    assert_eq!(
        kept_running(&farm, "d1"),
        [("db", "1.0"), ("web", "1.0")].map(|(s, v)| (s.to_owned(), v.to_owned()))
    );
    farm.database
        .run(
            "INSERT INTO tx_ledger (at_unix) \
             SELECT extract(epoch FROM now())::bigint - 7200 FROM generate_series(1, 5)",
        )
        .expect("expired rows of the ledger");

    beat(&farm, "d1", "2.0");
    assert_eq!(farm.store.health(), Health::Up);
    assert_eq!(
        kept_running(&farm, "d1"),
        [("db", "2.0"), ("web", "2.0")].map(|(s, v)| (s.to_owned(), v.to_owned()))
    );
}

/// The enrollment's records over one store, which note for each heartbeat's record (the first
/// serving, what the host runs) whether a decision was open on the writing thread.
struct Watch {
    inner: PgRecords,
    store: Postgres,
    writes: Mutex<Vec<(&'static str, bool)>>,
}

impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Watch")
    }
}

impl Watch {
    fn note(&self, what: &'static str) {
        let deciding = decide::seated(&self.store).is_some();
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((what, deciding));
    }

    fn writes(&self) -> Vec<(&'static str, bool)> {
        std::mem::take(&mut *self.writes.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Records for Watch {
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
        self.inner.commit(seen, records)
    }

    fn take_serial(
        &self,
        seen: &[Seen],
        reserving: Option<&pb::Request>,
    ) -> Result<u64, StoreError> {
        self.inner.take_serial(seen, reserving)
    }

    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.inner.set_first_heartbeat(host, at_unix)
    }

    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.note("first serving");
        self.inner.set_first_serving(host, at_unix)
    }

    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        self.note("running");
        self.inner.set_running(host, running)
    }

    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        self.inner.set_duplicate(host)
    }
}

/// Proves: a heartbeat writes what it noted of its host (its first serving, what it runs) once
/// its decision is over, never while the decision's transaction is open on the writing thread;
/// both are written, and kept.
///
/// Catches: a heartbeat that writes the host's enrollment on a second session while its
/// decision holds its transaction open and waits for the same thread: any lock the decision
/// holds that the write needs is then a wait no database can see, which only the lock timeout
/// ends.
#[tokio::test]
async fn a_heartbeat_writes_what_it_noted_of_its_host_once_its_decision_is_over() {
    super::logging();
    let watch: Arc<Mutex<Option<Arc<Watch>>>> = Arc::default();
    let made = Arc::clone(&watch);
    let farm = Farm::start_over("pg-after-decision", waits(), move |store| {
        let records = Arc::new(Watch {
            inner: PgRecords::over(store),
            store: store.clone(),
            writes: Mutex::default(),
        });
        *made.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&records));
        Some(records as Arc<dyn Records>)
    })
    .seeded()
    .await;
    let watch = watch
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .expect("the records");
    approve(&farm, "d1");

    beat(&farm, "d1", "2.0");
    assert_eq!(
        watch.writes(),
        vec![("first serving", false), ("running", false)]
    );
    assert_eq!(
        kept_running(&farm, "d1"),
        [("db", "2.0"), ("web", "2.0")].map(|(s, v)| (s.to_owned(), v.to_owned()))
    );
    let enrolled = farm
        .enrollment
        .as_ref()
        .expect("enrollment")
        .store()
        .enrolled_host(&HostId::parse("d1").expect("a host id"))
        .expect("read")
        .expect("enrolled");
    assert!(
        enrolled.first_serving_unix.is_some(),
        "the first serving is kept"
    );
}
