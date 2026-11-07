//! The schema: the migrations a start runs, what refuses a start, and the database's own
//! guards (the audit table's triggers and the fence on the lease).

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use super::{TestDatabase, admin, off_runtime};
use crate::store::conformance_tests::lines;
use crate::store::postgres::migrate::{self, ApplyError, Migration, MigrationError, OlderReleases};
use crate::store::postgres::session::Session;
use crate::store::postgres::writer::LOCK_KEY;
use crate::store::postgres::{DatabaseError, TIMEOUTS};
use crate::store::{Backend, Commit};

fn versions(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows("SELECT version, name, older_releases FROM schema_version ORDER BY version")
}

/// The rows of `schema_version` after the shipped migrations ran.
fn shipped_rows() -> Vec<Vec<Option<String>>> {
    vec![
        row("1", "state", "refuse"),
        row("2", "first_serving", "refuse"),
    ]
}

fn row(version: &str, name: &str, older: &str) -> Vec<Option<String>> {
    vec![
        Some(version.to_owned()),
        Some(name.to_owned()),
        Some(older.to_owned()),
    ]
}

/// A session on `db` as the migrations' runner has it, and `f` run with it on a thread of its own.
fn with_session<R: Send + 'static>(
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

fn message(error: &postgres::Error) -> String {
    error
        .as_db_error()
        .map(|db| db.message().to_owned())
        .unwrap_or_default()
}

/// Proves: a fresh database gets every shipped migration once, in order, each recorded in
/// `schema_version` with its name and its rule for older releases; a start on it again runs
/// nothing and leaves those rows (and the time they ran) as they were.
///
/// Catches: a migration run twice, or recorded under another number, name or rule.
#[test]
fn a_fresh_database_gets_every_migration_in_order() {
    let db = TestDatabase::new();
    let (first, _) = db.open("first").expect("the store opens");
    assert_eq!(versions(&db), shipped_rows());
    let applied = db.rows("SELECT applied_unix FROM schema_version");
    first.stop();
    let (second, _) = db.open("second").expect("the store opens again");
    assert_eq!(versions(&db), shipped_rows());
    assert_eq!(db.rows("SELECT applied_unix FROM schema_version"), applied);
    second.stop();
}

/// Proves: the runner runs the migrations a database lacks and answers their numbers; a
/// migration that fails is rolled back whole (what it made before the failing statement is gone,
/// and its row is not written) and the error names it and the server's words; the migrations
/// before it stay.
///
/// Catches: a migration that half applies and leaves a database no start can finish.
#[test]
fn a_migration_that_fails_is_rolled_back_whole_and_stops_the_start() {
    let db = TestDatabase::new();
    let shipped = migrate::checked(&migrate::SHIPPED).expect("the shipped set");
    let failing = Migration {
        version: 3,
        name: "half",
        older: OlderReleases::Refuse,
        sql: "CREATE TABLE half (x int); SELECT 1/0;",
    };
    let mut set = shipped.clone();
    set.push(failing);
    let error = with_session(&db, move |session| {
        migrate::apply(session, &set).expect_err("the third migration fails")
    });
    assert!(matches!(
        error,
        ApplyError::Failed {
            version: 3,
            name: "half",
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "migration 3 (half) failed and was rolled back: ERROR: division by zero (SQLSTATE 22012)"
    );
    assert_eq!(
        db.rows("SELECT to_regclass('hostkeeper.half')::text"),
        vec![vec![None]]
    );
    assert_eq!(versions(&db), shipped_rows());

    // A migration that works is run, once, and its number answered.
    let good = Migration {
        version: 3,
        name: "later",
        older: OlderReleases::MayRun,
        sql: "CREATE TABLE later (x int)",
    };
    let mut set = shipped;
    set.push(good);
    let (ran, again) = with_session(&db, move |session| {
        let ran = migrate::apply(session, &set).expect("the set applies");
        (ran, migrate::apply(session, &set).expect("and again"))
    });
    assert_eq!((ran, again), (vec![3], Vec::<i32>::new()));
    let mut with_later = shipped_rows();
    with_later.push(row("3", "later", "may-run"));
    assert_eq!(versions(&db), with_later);
}

/// Proves: the runner's failures read as text that says what failed: a session that cannot
/// write its bookkeeping (a read-only one) is `the schema's migrations could not be read`, a
/// migration refused before it runs keeps its own text, and a newer schema names the migration.
///
/// Catches: a failure of the bookkeeping reported as a failed migration, or with no reason.
#[test]
fn the_runners_failures_say_what_failed() {
    let db = TestDatabase::new();
    let config = db.dsn().config(TIMEOUTS);
    let shipped = migrate::checked(&migrate::SHIPPED).expect("the shipped set");
    let error = off_runtime(move || {
        let mut session = Session::connect(&config, Arc::new(AtomicU64::new(0)), true)
            .expect("a read-only session");
        migrate::apply(&mut session, &shipped).expect_err("a read-only session cannot migrate")
    });
    assert_eq!(
        error.to_string(),
        "the schema's migrations could not be read: ERROR: cannot execute CREATE SCHEMA in a read-only transaction (SQLSTATE 25006)"
    );
    let refused = ApplyError::from(MigrationError::Numbering {
        before: 1,
        found: 3,
    });
    assert_eq!(
        refused.to_string(),
        "migration 3 follows 1: the numbers run 1, 2, 3 and on"
    );
}

/// Proves: a database that holds a migration this build does not know is refused when that
/// migration says `refuse`, with the text that names it, and taken when it says `may-run`.
///
/// Catches: an older release that starts on a schema it would act wrongly on, or one that is
/// refused for a migration that only added.
#[test]
fn a_migration_marked_refuse_stops_an_older_release_and_may_run_does_not() {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run(
        "INSERT INTO schema_version (version, name, older_releases) VALUES (3, 'later', 'refuse')",
    )
    .expect("a later migration is recorded");
    let error = db.open("older").err().expect("an older release is refused");
    assert!(matches!(error, DatabaseError::Schema(_)));
    assert_eq!(
        error.to_string(),
        "the database's schema is newer than this coordinator (migration 3, later)"
    );
    db.run("UPDATE schema_version SET older_releases = 'may-run' WHERE version = 3")
        .expect("the rule is changed");
    let (store, _) = db.open("older").expect("an older release may run");
    store.stop();
}

/// Proves: a database whose `synchronous_commit` is off is refused at the start with the text
/// that says why, since the coordinator acknowledges what is committed.
///
/// Catches: a coordinator that acknowledges a change the server has not made durable.
#[test]
fn a_database_with_synchronous_commit_off_is_refused() {
    let db = TestDatabase::new();
    let name = db.name().to_owned();
    off_runtime(move || {
        admin(None)
            .batch_execute(&format!(
                "ALTER DATABASE \"{name}\" SET synchronous_commit = off"
            ))
            .expect("the setting is changed");
    });
    let error = db.open("durable").err().expect("the start is refused");
    assert!(matches!(error, DatabaseError::Refused(_)));
    assert_eq!(
        error.to_string(),
        "the database refused the coordinator: the database has synchronous_commit off; the coordinator acknowledges what is committed, so a commit must be on disk when it returns"
    );
}

fn refusal(result: Result<u64, postgres::Error>) -> String {
    message(&result.expect_err("the statement is refused"))
}

/// Proves: the audit table refuses `UPDATE`, `DELETE` and `TRUNCATE` from any session, each with
/// the trigger's message, and the line committed is still there.
///
/// Catches: an audit line that a client with the schema's rights can change or remove.
#[test]
fn the_audit_table_refuses_update_delete_and_truncate() {
    let db = TestDatabase::new();
    let (store, _) = db.open("audit").expect("the store opens");
    let audit = lines(1, 1);
    store
        .commit(&Commit {
            audit: &audit,
            ..Commit::default()
        })
        .expect("the audit line is committed");
    let mut client = db.client();
    let update = refusal(client.execute("UPDATE hostkeeper.audit SET reason = 'changed'", &[]));
    let delete = refusal(client.execute("DELETE FROM hostkeeper.audit", &[]));
    let truncate = refusal(client.execute("TRUNCATE hostkeeper.audit", &[]));
    assert_eq!(update, "the audit table is append-only: UPDATE is refused");
    assert_eq!(delete, "the audit table is append-only: DELETE is refused");
    assert_eq!(
        truncate,
        "the audit table is append-only: TRUNCATE is refused"
    );
    assert_eq!(
        db.rows("SELECT seq, reason FROM audit"),
        vec![vec![Some("1".to_owned()), Some("change 1".to_owned())]]
    );
    store.stop();
}

/// Proves: an update of the lease from a session that does not hold the coordinator's advisory
/// lock is refused with the fence's message; the same session, once it holds the lock, can make
/// it, and can take the lock only once the store has let go of it.
///
/// Catches: a deposed coordinator that could still commit, and a fence keyed to another lock
/// than the one the store takes.
#[test]
fn a_commit_on_a_session_without_the_lock_fails() {
    let db = TestDatabase::new();
    let (store, _) = db.open("fenced").expect("the store opens");
    let mut client = db.client();
    let bump = "UPDATE \"hostkeeper\".lease SET commits = commits + 1";
    assert_eq!(
        refusal(client.execute(bump, &[])),
        "this session does not hold the coordinator's lock of the database"
    );
    let try_lock = "SELECT pg_try_advisory_lock($1)";
    let taken: bool = client
        .query_one(try_lock, &[&LOCK_KEY])
        .expect("a try")
        .get(0);
    assert!(!taken, "the store holds the lock");
    store.stop();
    let taken: bool = client
        .query_one(try_lock, &[&LOCK_KEY])
        .expect("a try")
        .get(0);
    assert!(taken, "the lock is free once the store stopped");
    assert_eq!(
        client
            .execute(bump, &[])
            .expect("the fence lets the holder through"),
        1
    );
}

/// Proves: a second coordinator on a database whose lock is held is refused with the one text
/// for it, without raising the epoch; once the first lets go a third gets the lock and the epoch
/// one past the first's.
///
/// Catches: two coordinators leading one database, and an epoch raised by a refused start.
#[test]
fn a_second_coordinator_on_the_database_is_refused() {
    let db = TestDatabase::new();
    let (first, _) = db.open("first").expect("the store opens");
    let error = db.open("second").err().expect("the second is refused");
    assert!(matches!(error, DatabaseError::Locked));
    assert_eq!(
        error.to_string(),
        "another coordinator holds the lock of this database"
    );
    let epoch = first.epoch().expect("an epoch");
    first.stop();
    let (third, _) = db.open("third").expect("the third opens");
    assert_eq!(third.epoch(), Some(epoch + 1));
    assert_eq!(
        db.rows("SELECT epoch, holder FROM lease"),
        vec![vec![
            Some((epoch + 1).to_string()),
            Some("third".to_owned())
        ]]
    );
    third.stop();
}
