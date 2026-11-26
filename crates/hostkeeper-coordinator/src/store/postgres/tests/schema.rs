//! The schema: the migrations a start runs, what refuses a start, and the database's own
//! guards (the audit table's triggers and the fence on the lease).

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use super::{TestDatabase, admin, off_runtime};
use crate::store::conformance_tests::lines;
use crate::store::postgres::migrate::{self, ApplyError, Migration, MigrationError, OlderReleases};
use crate::store::postgres::session::Session;
use crate::store::postgres::writer::{LOCK_KEY, Writer};
use crate::store::postgres::{DatabaseError, TIMEOUTS, Timeouts};
use crate::store::{Backend, Commit};

fn versions(db: &TestDatabase) -> Vec<Vec<Option<String>>> {
    db.rows("SELECT version, name, older_releases FROM schema_version ORDER BY version")
}

/// The rows of `schema_version` after the shipped migrations ran.
fn shipped_rows() -> Vec<Vec<Option<String>>> {
    vec![
        row("1", "state", "refuse"),
        row("2", "first_serving", "refuse"),
        row("3", "generations", "refuse"),
        row("4", "releases", "may-run"),
        row("5", "mdm_devices", "may-run"),
        row("6", "host_running", "may-run"),
        row("7", "repositories", "may-run"),
        row("8", "host_groups", "refuse"),
        row("9", "service_repository", "may-run"),
        row("10", "accounts", "may-run"),
        row("11", "mdm", "may-run"),
        row("12", "mdm_setup", "may-run"),
        row("13", "host_group_all", "may-run"),
        row("14", "trusted_publishers", "may-run"),
        row("15", "lab_layout", "may-run"),
        row("16", "lab_devices", "may-run"),
        row("17", "lab_sleds", "may-run"),
        row("18", "remove_host_group_all", "may-run"),
        row("19", "tx_ledger", "may-run"),
        row("20", "ssh_certs", "may-run"),
        row("21", "ssh_tunnels", "may-run"),
        row("22", "instances", "may-run"),
        row("23", "state_versions", "may-run"),
        row("24", "fleet", "may-run"),
        row("25", "sessions_and_jobs", "may-run"),
        row("26", "decisions", "may-run"),
        row("27", "audit_api_token", "may-run"),
        row("28", "xcode_releases", "may-run"),
        row("29", "maintenance_runs", "may-run"),
        row("30", "maintenance_turns", "may-run"),
        row("31", "maintenance_operator", "may-run"),
        row("32", "maintenance_xcode_check", "may-run"),
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
        version: 33,
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
            version: 33,
            name: "half",
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "migration 33 (half) failed and was rolled back: ERROR: division by zero (SQLSTATE 22012)"
    );
    assert_eq!(
        db.rows("SELECT to_regclass('hostkeeper.half')::text"),
        vec![vec![None]]
    );
    assert_eq!(versions(&db), shipped_rows());

    // A migration that works is run, once, and its number answered.
    let good = Migration {
        version: 33,
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
    assert_eq!((ran, again), (vec![33], Vec::<i32>::new()));
    assert_eq!(
        versions(&db),
        [shipped_rows(), vec![row("33", "later", "may-run")]].concat()
    );
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
        "INSERT INTO schema_version (version, name, older_releases) VALUES (33, 'later', 'refuse')",
    )
    .expect("a later migration is recorded");
    let error = db.open("older").err().expect("an older release is refused");
    assert!(matches!(error, DatabaseError::Schema(_)));
    assert_eq!(
        error.to_string(),
        "the database's schema is newer than this coordinator (migration 33, later)"
    );
    db.run("UPDATE schema_version SET older_releases = 'may-run' WHERE version = 33")
        .expect("the rule is changed");
    let (store, _) = db.open("older").expect("an older release may run");
    store.stop();
}

/// Proves: the release before this one, which ships migrations `0001` and `0002` only, is refused
/// on a database this release migrated, naming `0003`: the runner of the release before, given
/// its own set, answers `the database's schema is newer than this coordinator (migration 3,
/// generations)` and runs nothing, because `0003` says older releases must not run on it.
///
/// Catches: an older coordinator that starts on the database and writes a desired state without
/// raising its generation, so a writer's `if_generation` would pass against a state it never saw.
#[test]
fn an_older_release_is_refused_on_a_database_with_0003() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.stop();
    let before = migrate::checked(&migrate::SHIPPED[..2]).expect("the release before's set");
    let error = with_session(&db, move |session| {
        migrate::apply(session, &before).expect_err("the release before is refused")
    });
    assert!(matches!(error, ApplyError::Newer { version: 3, .. }));
    assert_eq!(
        error.to_string(),
        "the database's schema is newer than this coordinator (migration 3, generations)"
    );
    assert_eq!(versions(&db), shipped_rows());
}

/// Proves: this release starts on a database the release before wrote: a desired state written
/// under `0001` and `0002` (with its registry) is loaded as it was after `0003` runs, its
/// write record read as never written (generation 0, no write id, nothing replaced), and the
/// next set over it is generation 1. A host row written under `0001` reads with no first serving
/// time.
///
/// Catches: a migration that makes rows of the release before unreadable, or reads them as a
/// write this release never saw.
#[test]
fn a_desired_row_written_before_0003_reads_as_never_written() {
    let db = TestDatabase::new();
    let before = migrate::checked(&migrate::SHIPPED[..2]).expect("the release before's set");
    with_session(&db, move |session| {
        migrate::apply(session, &before).expect("the release before's migrations")
    });
    db.run(
        "INSERT INTO desired (service, version, max_updating, min_up, release_digest) VALUES \
         ('web', '1.0', '{\"frontend\":1,\"storage\":1,\"daemon\":2,\"mac-daemon\":2}', \
         '{\"storage\":0,\"frontend\":0}', repeat('ab', 32)); \
         INSERT INTO desired_digests (service, version, release) VALUES \
         ('web', '1.0', to_jsonb(repeat('ab', 32)));",
    )
    .expect("the release before's rows");
    let (store, loaded) = db.open("this release").expect("the store opens");
    assert_eq!(versions(&db), shipped_rows());
    assert_eq!(loaded.desired.desired["web"].version.as_str(), "1.0");
    assert_eq!(loaded.desired.digests["web"].len(), 1);
    assert!(loaded.desired.records.is_empty());
    assert_eq!(
        db.rows("SELECT generation, write_id, replaced_version, replaced_min_up FROM desired"),
        vec![vec![Some("0".to_owned()), None, None, None]]
    );

    let backend: crate::store::Shared = Arc::new(store.clone());
    let desired = crate::desired::DesiredStore::over(backend, loaded.desired);
    let state = desired.get("web").expect("the state");
    desired
        .record("web", state, |_| true)
        .expect("a set over it");
    assert_eq!(desired.write_record("web").map(|r| r.generation), Some(1));
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

/// Proves: any number of coordinators hold the database's lock together, each raising the epoch
/// by one; a session that asks for the lock exclusively (the release before this one, an import)
/// is refused while any of them runs, and takes it once the last has stopped; and a coordinator
/// is refused, with the one text for it, while a session holds the lock exclusively.
///
/// Catches: a second coordinator that cannot start beside the first (which makes a start-first
/// update impossible), and a release before this one that starts beside a coordinator of this one.
#[test]
fn coordinators_share_the_database_and_an_exclusive_taker_is_refused() {
    let db = TestDatabase::new();
    let (first, _) = db.open("first").expect("the store opens");
    let (second, _) = db
        .open("second")
        .expect("a second store opens beside the first");
    assert_eq!(second.epoch(), first.epoch().map(|epoch| epoch + 1));
    let mut client = db.client();
    let exclusive = "SELECT pg_try_advisory_lock($1)";
    let taken = |client: &mut postgres::Client| -> bool {
        client
            .query_one(exclusive, &[&LOCK_KEY])
            .expect("a try")
            .get(0)
    };
    assert!(!taken(&mut client), "two coordinators hold the lock");
    first.stop();
    assert!(!taken(&mut client), "one coordinator still holds the lock");
    second.stop();
    assert!(taken(&mut client), "the lock is free once both stopped");
    let error = db
        .open("third")
        .err()
        .expect("an exclusive holder refuses it");
    assert!(matches!(error, DatabaseError::Locked));
    assert_eq!(
        error.to_string(),
        "another coordinator holds the lock of this database"
    );
    assert_eq!(
        db.rows("SELECT epoch FROM lease"),
        vec![vec![Some("2".to_owned())]],
        "a refused start does not raise the epoch"
    );
}

/// Proves: coordinators that start together on an empty database run each migration once between
/// them: every start succeeds, the numbers the runners ran together are 1 to 19 with none twice,
/// and `schema_version` holds each once.
///
/// Catches: two starters that both find `0019` missing, both run it, and one fails on the table
/// it made, so a start-first update crash-loops its new task.
///
/// The sessions wait without a statement or lock timeout (0 turns each off), so a runner that
/// waits for the runners' lock while another applies every migration of an empty database waits
/// as long as that takes, however loaded the machine; that a wait is bounded by the lock timeout
/// is [`a_runner_that_cannot_get_the_runners_lock_runs_nothing`]'s.
#[test]
fn starters_that_run_together_apply_each_migration_once() {
    let db = TestDatabase::new();
    let unbounded = Timeouts {
        statement: Duration::ZERO,
        lock: Duration::ZERO,
        ..TIMEOUTS
    };
    let config = db.dsn().config(unbounded);
    let set = migrate::checked(&migrate::SHIPPED).expect("the shipped set");
    let runners: Vec<_> = (0..4)
        .map(|_| {
            let (config, set) = (config.clone(), set.clone());
            std::thread::spawn(move || {
                let mut session = Session::connect(&config, Arc::new(AtomicU64::new(0)), false)
                    .expect("a session");
                migrate::apply(&mut session, &set).expect("the runner applies")
            })
        })
        .collect();
    let mut ran: Vec<i32> = runners
        .into_iter()
        .flat_map(|runner| runner.join().expect("the runner ends"))
        .collect();
    ran.sort_unstable();
    assert_eq!(ran, (1..=32).collect::<Vec<i32>>());
    assert_eq!(versions(&db), shipped_rows());
}

/// Proves: a runner that cannot get the runners' lock within the session's lock timeout fails
/// with the bookkeeping's text and runs nothing, and the next runner, once the lock is free, runs
/// everything.
///
/// Catches: a runner that waits for ever behind a starter that hangs, or that migrates without
/// the lock.
#[test]
fn a_runner_that_cannot_get_the_runners_lock_runs_nothing() {
    let db = TestDatabase::new();
    let mut holder = db.client();
    holder
        .execute("SELECT pg_advisory_lock($1)", &[&migrate::MIGRATE_KEY])
        .expect("the runners' lock is taken");
    let set = migrate::checked(&migrate::SHIPPED).expect("the shipped set");
    let waiting = set.clone();
    let error = with_session(&db, move |session| {
        migrate::apply(session, &waiting).expect_err("the lock is not to be had")
    });
    assert!(matches!(error, ApplyError::Database(_)), "{error}");
    holder
        .execute("SELECT pg_advisory_unlock($1)", &[&migrate::MIGRATE_KEY])
        .expect("the lock is given");
    let ran = with_session(&db, move |session| {
        migrate::apply(session, &set).expect("the set applies")
    });
    assert_eq!(ran, (1..=32).collect::<Vec<i32>>());
}

/// How many times the tests below end a store and take its lock at once.
const CYCLES: u32 = 200;

/// The cycles, of [`CYCLES`] on `db`, in which another session could not take the lock at once
/// after a store was opened, had `before` run on its writer, and was ended by `end`.
fn cycles_held(
    db: &TestDatabase,
    before: fn(&mut Writer),
    end: fn(crate::store::postgres::Postgres),
) -> Vec<u32> {
    let mut client = db.client();
    let mut held = Vec::new();
    for cycle in 0..CYCLES {
        let (store, _) = db.open("cycling").expect("the store opens");
        store.on_writer(before).expect("the writer runs");
        end(store);
        let taken: bool = client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])
            .expect("a try")
            .get(0);
        client
            .batch_execute("SELECT pg_advisory_unlock_all()")
            .expect("the lock is freed");
        held.extend((!taken).then_some(cycle));
    }
    held
}

/// Proves: once [`Postgres::stop`](crate::store::postgres::Postgres::stop) returns, the store's
/// advisory lock is free on the server, every time: another session takes it at once, in each
/// of many cycles of open, stop and try.
///
/// Catches: a stop that closes the store's session and returns before the server has ended it
/// and freed the lock with it.
#[test]
fn the_lock_is_free_at_once_after_every_stop() {
    let db = TestDatabase::new();
    let held = cycles_held(&db, |_| (), |store| store.stop());
    assert_eq!(
        held,
        Vec::<u32>::new(),
        "the cycles that found the lock held"
    );
}

/// Proves: once a store is dropped, as a coordinator drops it when it ends, its advisory lock is
/// free on the server, every time: a successor's session takes it at once, in each of many cycles
/// of open, drop and try.
///
/// Catches: a successor refused `another coordinator holds the lock of this database` by the
/// session of a coordinator that already ended.
#[test]
fn the_lock_is_free_at_once_after_every_drop() {
    let db = TestDatabase::new();
    let held = cycles_held(&db, |_| (), drop);
    assert_eq!(
        held,
        Vec::<u32>::new(),
        "the cycles that found the lock held"
    );
}

/// Proves: a store whose writing session is left in an aborted transaction (a job that failed
/// inside one and never rolled it back) still frees its lock at once when it stops, every time.
///
/// Catches: an unlock at the end that the aborted transaction refuses, which leaves the lock to
/// the server's end of the session, after the stop returned.
#[test]
fn the_lock_is_free_at_once_after_a_stop_inside_an_aborted_transaction() {
    let db = TestDatabase::new();
    let abort = |writer: &mut Writer| {
        let session = writer.session().expect("a session");
        session.batch("BEGIN").expect("a transaction");
        let failed = session.batch("SELECT 1 / 0");
        assert!(failed.is_err(), "the statement fails: {failed:?}");
    };
    let held = cycles_held(&db, abort, |store| store.stop());
    assert_eq!(
        held,
        Vec::<u32>::new(),
        "the cycles that found the lock held"
    );
}
