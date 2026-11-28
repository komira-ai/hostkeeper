//! The hardware event log as rows of `hardware_events` (migration `0033`;
//! `docs/design/host-health-alerts.md`, section 2.3): the contract every store of the log keeps,
//! the same pages as the file store's, a row another coordinator wrote left as it is, the table
//! append-only, and the release before beside it. Nothing here reads a clock.

use std::sync::{Arc, Mutex, PoisonError};

use super::{TestDatabase, off_runtime, with_session};
use crate::hardware_files::FileLog;
use crate::hardware_log::tests::{Subject, conformance, contract_records, record};
use crate::hardware_log::{HardwareLog, Place, Query};
use crate::store::postgres::Postgres;
use crate::store::postgres::hardware_rows::PgHardwareLog;
use crate::store::postgres::migrate;
use crate::testutil::scratch;

/// The message of a write refused at once because the store is lost.
const WRITE_LOST: &str = "the writing session stopped";

/// The message of a read refused at once because the store is lost.
const READ_LOST: &str = "the reading session stopped";

/// The whole log, newest first.
fn every(log: &dyn HardwareLog) -> Vec<crate::hardware_log::Record> {
    log.page(&Query {
        limit: usize::MAX,
        ..Query::default()
    })
    .expect("read")
}

/// A database with a store open on it, and the log over it.
fn rows() -> (TestDatabase, Postgres, PgHardwareLog) {
    let db = TestDatabase::new();
    let (store, _) = db.open("hardware-rows").expect("the store opens");
    let log = PgHardwareLog::new(store.clone());
    (db, store, log)
}

/// The database: started again by stopping the live store and opening another, which takes the
/// lock.
struct OnPostgres {
    database: TestDatabase,
    live: Mutex<Option<Postgres>>,
}

impl OnPostgres {
    fn opened(&self) -> Postgres {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = live.take() {
            old.stop();
        }
        let (store, _) = self
            .database
            .open("hardware-rows")
            .expect("the store opens");
        *live = Some(store.clone());
        store
    }

    fn stop(&self) {
        if let Some(store) = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            store.stop();
        }
    }
}

impl Subject for OnPostgres {
    fn live(&self) -> Arc<dyn HardwareLog> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Arc::new(PgHardwareLog::new(live.unwrap_or_else(|| self.opened())))
    }

    fn restarted(&self) -> Arc<dyn HardwareLog> {
        Arc::new(PgHardwareLog::new(self.opened()))
    }
}

/// Proves: the table keeps the contract every store of the log keeps, a store started again on
/// the database included: the same order (keys compared byte by byte), filters and pages as the
/// memory and the file, and a record written once.
///
/// Catches: a database that orders the keys by a language's collation, so that a page token
/// skips or repeats records, a filter compared on another column, and a repeat that fails the
/// write or replaces the row.
#[test]
fn the_table_keeps_the_contract() {
    let subject = OnPostgres {
        database: TestDatabase::new(),
        live: Mutex::new(None),
    };
    conformance(&subject);
    subject.stop();
}

/// Proves: the file store and the table, given the same writes, answer the same pages, record
/// for record, for every filter and for a walk of pages of 2 after each page's last record.
///
/// Catches: the two stores of the log parting, so that a coordinator moved from files to a
/// database lists the log otherwise.
#[test]
fn the_file_and_the_table_give_the_same_pages() {
    let (_db, store, table) = rows();
    let dir = scratch("hardware-rows-alike");
    let file = FileLog::new(dir.path().join("desired.json.hardware-events"));
    let logs: [&dyn HardwareLog; 2] = [&table, &file];
    for log in logs {
        log.write(&contract_records()[..5]).expect("written");
        log.write(&contract_records()).expect("written");
    }
    let mut after: Option<Place> = None;
    loop {
        let query = Query {
            limit: 2,
            after: after.clone(),
            ..Query::default()
        };
        let page = table.page(&query).expect("read");
        assert_eq!(page, file.page(&query).expect("read"), "{query:?}");
        match page.last() {
            Some(last) => after = Some(last.place()),
            None => break,
        }
    }
    for host in ["host-a", "host-b", "host-c"] {
        let query = Query {
            host_id: Some(host.to_owned()),
            limit: 100,
            ..Query::default()
        };
        assert_eq!(table.page(&query), file.page(&query), "{host}");
    }
    assert_eq!(every(&table), every(&file));
    store.stop();
}

/// Proves: a record whose row another coordinator wrote first (the same host and `event_id`) is
/// left as that one wrote it, and the write that repeats it, with a new record beside it, writes
/// the new one and succeeds.
///
/// Catches: a write that fails on a key another coordinator holds, which would make the daemon
/// send its records for ever, and one that overwrites the first coordinator's row.
#[test]
fn a_row_another_coordinator_wrote_is_left_as_it_is() {
    let (db, store, log) = rows();
    let first = record("host-a", "sel:01", 100, "memory", "critical");
    PgHardwareLog::new(store.clone())
        .write(std::slice::from_ref(&first))
        .expect("the other coordinator's write");
    let mut repeat = first.clone();
    repeat.observed_unix += 30;
    let beside = record("host-a", "sel:02", 101, "memory", "critical");
    log.write(&[repeat, beside.clone()]).expect("written");
    assert_eq!(every(&log), [beside, first]);
    assert_eq!(
        db.rows("SELECT count(*) FROM hardware_events"),
        vec![vec![Some("2".to_owned())]]
    );
    store.stop();
}

/// Proves: the table refuses to change or remove a row (`UPDATE`, `DELETE`, `TRUNCATE`), and an
/// `event_id` that is empty or over 64 bytes.
///
/// Catches: a `0033` without its append-only trigger or its bound on the key, so that the log a
/// person reads as the host's history could be rewritten.
#[test]
fn the_table_is_append_only() {
    let (db, store, log) = rows();
    log.write(&[record("host-a", "sel:01", 100, "memory", "critical")])
        .expect("written");
    for refused in [
        "UPDATE hardware_events SET message = 'other'",
        "DELETE FROM hardware_events",
        "TRUNCATE hardware_events",
    ] {
        assert!(db.run(refused).is_err(), "{refused}");
    }
    let insert = |event_id: &str| {
        format!(
            "INSERT INTO hardware_events (host_id, event_id, time_unix, observed_unix, \
             component, severity, state, source, code, message) \
             VALUES ('host-b', '{event_id}', 1, 1, 'disk', 'info', 'event', 'smart', 'c', 'm')"
        )
    };
    assert!(db.run(&insert("")).is_err());
    assert!(db.run(&insert(&"e".repeat(65))).is_err());
    db.run(&insert(&"e".repeat(64))).expect("a key of 64 bytes");
    assert_eq!(every(&log).len(), 2);
    store.stop();
}

/// Proves: over a store that is lost, a write and a read each return the store's error, and the
/// rows written before stand.
///
/// Catches: a write answered as written that the table does not hold (the daemon would drop the
/// records), and a read that answers an empty log for a store it could not read.
#[test]
fn a_lost_store_writes_and_reads_nothing() {
    let (db, store, log) = rows();
    log.write(&[record("host-a", "sel:01", 100, "memory", "critical")])
        .expect("written");
    store.stop();
    off_runtime(move || {
        assert_eq!(
            log.write(&[record("host-a", "sel:02", 101, "memory", "critical")]),
            Err(WRITE_LOST.to_owned())
        );
        assert_eq!(log.page(&Query::default()), Err(READ_LOST.to_owned()));
    });
    assert_eq!(
        db.rows("SELECT count(*) FROM hardware_events"),
        vec![vec![Some("1".to_owned())]]
    );
}

/// Proves (B8, both ways, for `0033`): the release before (migrations `0001` to `0032`) runs on a
/// database this release migrated and holds records, running nothing and leaving them; this
/// release, started on a database the release before migrated, runs `0033` alone and makes the
/// table empty.
///
/// Catches: a `0033` that is not `may-run`, which would turn a rollback into a restore, and a
/// table the start does not make.
#[test]
fn the_release_before_runs_on_a_database_with_0033_and_this_one_on_its() {
    let (db, store, log) = rows();
    log.write(&[record("host-a", "sel:01", 100, "memory", "critical")])
        .expect("written");
    store.stop();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..32]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert_eq!(
        db.rows("SELECT count(*) FROM hardware_events"),
        vec![vec![Some("1".to_owned())]]
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..32]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    assert_eq!(
        old.rows(
            "SELECT count(*) FROM pg_class WHERE relkind = 'r' AND relname = 'hardware_events'"
        ),
        vec![vec![Some("0".to_owned())]],
        "the release before has no such table"
    );
    let (store, _) = old.open("this release").expect("this release starts on it");
    assert!(every(&PgHardwareLog::new(store.clone())).is_empty());
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 32"),
        [vec![Some("33".to_owned())]],
        "0033 alone ran"
    );
}

/// Proves: a record whose time is past what the table's `bigint` holds is refused whole, naming
/// the time, and so is a page asked from such a time; nothing is written; and the log's debug form
/// shows no connection detail.
///
/// Catches: a time wrapped into a negative number, which would put the record at the start of the
/// log, and a write that lands the records before the one refused.
#[test]
fn a_time_past_the_table_is_refused_whole() {
    let (_db, store, log) = rows();
    let past = record("host-a", "sel:09", u64::MAX, "memory", "critical");
    let fine = record("host-a", "sel:08", 1, "memory", "critical");
    let why = format!("{} is above the largest time the database keeps", u64::MAX);
    assert_eq!(log.write(&[fine, past]), Err(why.clone()));
    assert_eq!(
        log.page(&Query {
            since_unix: u64::MAX,
            ..Query::default()
        }),
        Err(why)
    );
    assert!(every(&log).is_empty());
    assert_eq!(format!("{log:?}"), "PgHardwareLog { .. }");
    store.stop();
}
