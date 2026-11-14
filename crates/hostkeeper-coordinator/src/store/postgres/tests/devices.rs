//! The MDM gate's device records on Postgres (`mdm_devices`, migration `0005`): the gate's case
//! list for every device store, what the store says when the database cannot be reached, and the
//! release before on a database holding `0005`, and this release on the release before's.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_mdm_gate::conformance::{self, Subject};
use hostkeeper_mdm_gate::{DeviceRecord, DeviceStore, EnrollmentId};

use super::{SPENT_NOW, TestDatabase, off_runtime};
use crate::store::postgres::session::Session;
use crate::store::postgres::{PgDevices, Postgres, TIMEOUTS, load_all, migrate};

/// The case list's subject on a database of its own: each open is the device store made again
/// over the one store, which keeps nothing in memory and reads the table anew, as a restart's
/// does. (A store opened again would be refused the lock while the case still holds the first.)
struct OnPostgres {
    database: TestDatabase,
    live: Mutex<Option<Postgres>>,
}

impl Subject for OnPostgres {
    fn open(&self) -> Box<dyn DeviceStore> {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let store =
            live.get_or_insert_with(|| self.database.open("devices").expect("the store opens").0);
        Box::new(PgDevices::over(store))
    }

    fn fail_next_save(&self) {
        self.database
            .run(
                "DROP TRIGGER IF EXISTS test_once ON lease; DROP SEQUENCE IF EXISTS test_once; \
                 CREATE SEQUENCE test_once; \
                 CREATE OR REPLACE FUNCTION test_once() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN IF nextval('test_once') = 1 THEN \
                 RAISE EXCEPTION 'the test refuses this commit'; END IF; RETURN NEW; END $$; \
                 CREATE TRIGGER test_once BEFORE UPDATE ON lease FOR EACH ROW \
                 EXECUTE FUNCTION test_once()",
            )
            .expect("the refusal is set");
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) {
        drop(self.open());
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.database
            .run(&format!(
                "INSERT INTO mdm_devices (enrollment_id, record) VALUES ('{key}', '\\x{hex}')"
            ))
            .expect("the row is put");
    }
}

/// Proves: the database's device store passes every case the gate holds a device store to, the
/// same list the file store passes: empty at first, every field back after a restart, a save
/// replaces, a failed commit is an error that leaves the row as it was, and a row that is not
/// whole, of a newer schema or of another enrollment id stops the load while a field of a later
/// release is skipped.
#[test]
fn the_device_store_cases_pass_on_postgres() {
    let failures = conformance::run(&|| {
        Box::new(OnPostgres {
            database: TestDatabase::new(),
            live: Mutex::new(None),
        })
    });
    let failures: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert_eq!(failures, Vec::<String>::new());
}

fn record(id: &str) -> DeviceRecord {
    DeviceRecord::new(EnrollmentId::parse(id).expect("an enrollment id"), 5)
}

/// Proves: once the store has stopped, a load and a save each fail with the text that names the
/// table or the row, the session's state, and neither the connection string nor its file; and
/// the store's `Debug` shows nothing of the database.
///
/// Catches: a device store that answers an empty load when it cannot read (every device
/// forgotten), or a save that answers Ok when nothing was committed.
#[test]
fn a_device_store_that_cannot_reach_the_database_says_so() {
    let db = TestDatabase::new();
    let (store, _) = db.open("devices").expect("the store opens");
    let devices = PgDevices::over(&store);
    assert_eq!(format!("{devices:?}"), "PgDevices");
    devices.save(&record("a-1")).expect("saved");
    store.stop();
    assert_eq!(
        devices.load().expect_err("refused").to_string(),
        "database:mdm_devices: the reading session stopped"
    );
    assert_eq!(
        devices
            .save(&record("a-1"))
            .expect_err("refused")
            .to_string(),
        "database:mdm_devices/a-1: the writing session stopped"
    );
}

/// A session on `db` and `f` run with it on a thread of its own.
pub(super) fn with_session<R: Send + 'static>(
    db: &TestDatabase,
    f: impl FnOnce(&mut Session) -> R + Send + 'static,
) -> R {
    let config = db.dsn().config(TIMEOUTS);
    off_runtime(move || {
        let mut session =
            Session::connect(&config, Arc::new(AtomicU64::new(0)), false).expect("a session");
        f(&mut session)
    })
}

/// Proves: `0005` does not stop the release before it (migrations `0001` to `0004`, whose start
/// never reads `mdm_devices`) on a database this release migrated and whose gate keeps a device:
/// its runner is refused naming `0008`, the first migration after its own that says `refuse`, not
/// `0005`, which says `may-run`; and the state reads as this release loads it. And the other way: this release starts on a database the release
/// before migrated, runs `0005` and `0006`, and its gate holds no device.
///
/// Catches: a device table that makes the previous coordinator refuse to start (the back-out of a
/// release would need the database restored), or a migration that this release does not run on a
/// database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0005_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    PgDevices::over(&store)
        .save(&record("a-1"))
        .expect("the gate keeps a device");
    store.stop();
    let (ran, before_loaded) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..4]).expect("the release before's set");
        let ran = migrate::apply(session, &before)
            .expect_err("its runner is refused")
            .to_string();
        (ran, load_all(session, SPENT_NOW).expect("the state reads"))
    });
    assert_eq!(
        ran,
        "the database's schema is newer than this coordinator (migration 8, host_groups)"
    );
    assert_eq!(before_loaded, loaded);
    assert_eq!(db.rows("SELECT enrollment_id FROM mdm_devices").len(), 1);

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..4]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, vec![1, 2, 3, 4]);
    let (store, loaded) = older.open("this release").expect("this release starts");
    // `0013` made the built-in host group `all`; nothing else is held.
    let mut empty = crate::store::Loaded::default();
    empty.desired.host_groups = crate::host_groups::HostGroups::default().with_all();
    assert_eq!(loaded, empty);
    assert_eq!(
        PgDevices::over(&store).load().expect("the table reads"),
        Vec::<DeviceRecord>::new()
    );
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=16)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
}
