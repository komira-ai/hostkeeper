//! A migration's lock timeout and its bounded retry (`docs/design/coordinator-multi-instance.md`,
//! section 4, "Migrations"): a running coordinator's transaction holds a table, a starting one's
//! migration needs that table exclusively, and the statements queued behind the migration's wait
//! are let go when its lock timeout ends the wait, not when the holder ends.
//!
//! No test reads a clock: a wait is seen in `pg_stat_activity`, the pause between attempts is a
//! function the test passes and which never sleeps, and each step waits for the one before by a
//! channel.

use std::sync::mpsc;
use std::time::Duration;

use postgres::Client;

use super::{TestDatabase, admin, off_runtime, with_session};
use crate::store::postgres::migrate::{self, ApplyError, MIGRATE_KEY, Migration, Retry, SHIPPED};

/// The migration after the shipped ones: a table that records the lock timeout its transaction
/// runs under, then a column added to `spent_tokens`, which needs that table exclusively.
const FIXTURE: &str = "-- older-releases: may-run\n\
    CREATE TABLE m_seen AS SELECT current_setting('lock_timeout') AS lock_timeout;\n\
    ALTER TABLE spent_tokens ADD COLUMN m_added int;\n";

/// The shipped migrations and [`FIXTURE`] after them, and the fixture's number.
fn with_fixture() -> (Vec<Migration>, i32) {
    let mut set = migrate::checked(&SHIPPED).expect("the shipped set");
    let next = set.last().map_or(1, |m| m.version + 1);
    set.push(
        migrate::migration(next, "ddl_fixture", FIXTURE).expect("the fixture passes the lint"),
    );
    (set, next)
}

/// `db` brought up to the shipped migrations.
fn migrated() -> TestDatabase {
    let db = TestDatabase::new();
    with_session(&db, |session| {
        let shipped = migrate::checked(&SHIPPED).expect("the shipped set");
        migrate::apply(session, &shipped).expect("the shipped migrations run");
    });
    db
}

/// A client of `db` that holds `spent_tokens` (a row inserted in a transaction left open), as a
/// running coordinator's transaction holds the table it writes.
fn holder(db: &TestDatabase) -> Client {
    let mut client = db.client();
    client
        .batch_execute(
            "SET search_path TO hostkeeper; BEGIN; \
             INSERT INTO spent_tokens (jti, until_unix) VALUES ('held', 1)",
        )
        .expect("the holder's transaction is open");
    client
}

/// Waits until a session of `db` waits for a lock with a query that contains `text`.
fn seen_waiting(db: &TestDatabase, text: &str) {
    let (name, like) = (db.name().to_owned(), format!("%{text}%"));
    off_runtime(move || {
        let mut watcher = admin(None);
        for _ in 0..200_000 {
            let waiting: i64 = watcher
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE datname = $1 AND wait_event_type = 'Lock' AND query LIKE $2",
                    &[&name, &like],
                )
                .expect("the activity")
                .get(0);
            if waiting > 0 {
                return;
            }
            std::thread::yield_now();
        }
        panic!("no session waited for a lock with {like}");
    });
}

/// Whether `db` has the table `table` in the store's schema.
fn has_table(db: &TestDatabase, table: &str) -> bool {
    db.rows(&format!(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = 'hostkeeper' AND table_name = '{table}'"
    )) == vec![vec![Some("1".to_owned())]]
}

/// Runs `set` on `db` with `retry`, on a session whose own waits are off, so only the
/// migration's lock timeout can end a wait for a lock (a statement still ends after a minute, so a
/// runner without it fails rather than hangs). Each pause is told to `paused` and waits for `go`.
fn run(
    db: &TestDatabase,
    set: Vec<Migration>,
    retry: Retry,
    paused: mpsc::Sender<Duration>,
    go: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<Result<Vec<i32>, ApplyError>> {
    let config = db.dsn().config(super::TIMEOUTS);
    std::thread::spawn(move || {
        let mut session = crate::store::postgres::session::Session::connect(
            &config,
            std::sync::Arc::default(),
            false,
        )
        .expect("a session");
        session
            .batch("SET lock_timeout = 0; SET statement_timeout = '60s'")
            .expect("the session's own waits are off");
        let mut pause = |wait: Duration| {
            paused.send(wait).expect("the test listens");
            go.recv().expect("the test lets the runner go on");
        };
        migrate::apply_with(&mut session, &set, retry, &mut pause)
    })
}

/// Proves: a migration whose `ALTER TABLE` waits behind a running coordinator's open transaction
/// runs under the lock timeout it is given (`SET LOCAL lock_timeout`, seen from inside the
/// migration), gives up when it runs out, and is rolled back whole (the table its first statement
/// made is gone); a heartbeat's statement on that table queued behind the waiting migration is let
/// through while the holder's transaction is still open; the runner pauses once, for the retry
/// time it is given, with the runners' lock free, and its second attempt, once the holder is gone,
/// applies the migration once.
///
/// Catches: a migration transaction with no lock timeout of its own, whose wait for the holder
/// stalls every heartbeat queued behind it for as long as the holder runs; a retry that keeps the
/// runners' lock while it pauses; and a half-applied migration.
#[test]
fn migration_ddl_gives_up_by_lock_timeout_instead_of_stalling_heartbeats() {
    super::logging();
    let db = migrated();
    let mut holding = holder(&db);
    let (set, next) = with_fixture();
    let (paused, pauses) = mpsc::channel();
    let (go, going) = mpsc::channel();
    let retry = Retry {
        lock_timeout: Duration::from_millis(2000),
        pause: Duration::from_secs(7),
        attempts: std::num::NonZeroU32::new(2).expect("two"),
    };
    let runner = run(&db, set, retry, paused, going);

    seen_waiting(&db, "ALTER TABLE spent_tokens ADD COLUMN m_added");
    let name = db.name().to_owned();
    let beat = std::thread::spawn(move || {
        let mut client = admin(Some(&name));
        // A guard against a hang, never reached when the migration gives up.
        client
            .batch_execute("SET search_path TO hostkeeper; SET lock_timeout = '60s'")
            .expect("the beat's session");
        client.execute(
            "INSERT INTO spent_tokens (jti, until_unix) VALUES ('beat', 2)",
            &[],
        )
    });
    let beat = beat.join().expect("the beat's thread");
    assert_eq!(
        beat.expect("the beat's statement is let through"),
        1,
        "the beat's row is written while the holder's transaction is still open"
    );

    assert_eq!(pauses.recv().expect("the runner pauses"), retry.pause);
    assert!(
        !has_table(&db, "m_seen"),
        "the first attempt was rolled back whole"
    );
    let name = db.name().to_owned();
    let free = off_runtime(move || {
        let mut other = admin(Some(&name));
        let taken: bool = other
            .query_one("SELECT pg_try_advisory_lock($1)", &[&MIGRATE_KEY])
            .expect("a try")
            .get(0);
        taken
    });
    assert!(free, "the runners' lock is free while the runner pauses");
    holding.batch_execute("COMMIT").expect("the holder ends");
    go.send(()).expect("the runner goes on");

    let ran = runner.join().expect("the runner's thread");
    assert_eq!(ran.expect("the second attempt applies it"), vec![next]);
    assert!(super::logged("it is tried again"), "the retry is logged");
    assert!(pauses.try_recv().is_err(), "one pause only");
    assert_eq!(
        db.rows("SELECT lock_timeout FROM m_seen"),
        vec![vec![Some("2s".to_owned())]],
        "the migration ran under the lock timeout it was given"
    );
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM schema_version WHERE version = {next}"
        )),
        vec![vec![Some("1".to_owned())]]
    );
    assert_eq!(
        db.rows("SELECT count(*) FROM spent_tokens WHERE m_added IS NULL"),
        vec![vec![Some("2".to_owned())]],
        "the column is there, over the holder's row and the beat's"
    );
}

/// Proves: a migration that cannot have its lock at any of its attempts (the holder never ends)
/// is tried exactly the number of attempts it is given, with a pause of the retry time between
/// each two and none after the last, and is then refused with the text that names it, the
/// attempts and the server's refusal; nothing of it is applied.
///
/// Catches: a runner that retries for ever (the start never ends and never says why), one that
/// gives up at the first refusal, and one that leaves part of the migration behind.
#[test]
fn a_migration_that_never_has_its_lock_is_refused_after_its_attempts() {
    let db = migrated();
    let mut holding = holder(&db);
    let (set, next) = with_fixture();
    let (paused, pauses) = mpsc::channel();
    let (go, going) = mpsc::channel();
    let retry = Retry {
        lock_timeout: Duration::from_millis(50),
        pause: Duration::from_secs(7),
        attempts: std::num::NonZeroU32::new(3).expect("three"),
    };
    for _ in 0..2 {
        go.send(()).expect("the runner may go on at each pause");
    }
    let runner = run(&db, set, retry, paused, going);
    let refused = runner
        .join()
        .expect("the runner's thread")
        .expect_err("the migration never has its lock");
    assert_eq!(
        refused.to_string(),
        format!(
            "migration {next} (ddl_fixture) could not have its locks within its lock timeout in 3 \
             attempts, each rolled back: ERROR: canceling statement due to lock timeout \
             (SQLSTATE 55P03)"
        )
    );
    let seen: Vec<Duration> = pauses.try_iter().collect();
    assert_eq!(seen, vec![retry.pause, retry.pause]);
    assert!(!has_table(&db, "m_seen"));
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM schema_version WHERE version = {next}"
        )),
        vec![vec![Some("0".to_owned())]]
    );
    holding.batch_execute("ROLLBACK").expect("the holder ends");
}
