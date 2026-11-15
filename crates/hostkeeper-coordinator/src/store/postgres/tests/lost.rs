//! A lost writing session, its revival, and what a commit of unknown outcome turned out to be.
//!
//! A test makes the loss by ending the store's sessions at the server (or, for a commit that is
//! in flight, a fault the writer meets at its `COMMIT`), and waits for a revival by running a job
//! on the writer's own thread behind it: the revival runs there, so the job returns after it.

use super::{TestDatabase, admin, off_runtime};
use crate::store::conformance_tests::{lines, rich_ops};
use crate::store::postgres::Postgres;
use crate::store::postgres::writer::{Fault, LOCK_KEY, WriteFail};
use crate::store::{Backend, BackendError, Commit, Health};

const LOST: &str = "database:database: the database session is lost";

/// Waits for the jobs handed to the writer so far, a revival among them.
fn settle(store: &Postgres) {
    store.on_writer(|_| ()).expect("the writer runs");
}

fn line(store: &Postgres, seq: u64) -> Result<(), BackendError> {
    store.commit(&Commit {
        audit: &lines(seq, seq),
        ..Commit::default()
    })
}

fn seqs(db: &TestDatabase) -> Vec<String> {
    db.rows("SELECT seq FROM audit ORDER BY seq")
        .into_iter()
        .flatten()
        .flatten()
        .collect()
}

/// Proves: after the store's sessions are ended every write is refused at once as lost (a commit,
/// a spend, a load), the store says it is lost, a revival at the next heartbeat opens the session
/// again, and writes go on, nothing written meanwhile; each commit made one row of the ledger.
///
/// Catches: a write that waits on a dead session, a revival that is never asked for, and a store
/// that comes back on another epoch.
#[test]
fn a_lost_session_is_revived() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("lost").expect("the store opens");
    line(&store, 1).expect("a commit while up");
    assert_eq!(store.health(), Health::Up);
    store.revive();
    settle(&store);
    assert_eq!(
        store.health(),
        Health::Up,
        "a revival of a session that is up does nothing"
    );

    db.end_sessions();
    assert!(
        line(&store, 2).is_err(),
        "the commit finds the session gone"
    );
    assert_eq!(store.health(), Health::Lost);
    assert_eq!(line(&store, 3).expect_err("lost").to_string(), LOST);
    assert_eq!(store.load().expect_err("lost").to_string(), LOST);
    assert_eq!(store.spend("j", 10, 1).expect_err("lost").to_string(), LOST);
    assert_eq!(seqs(&db), vec!["1"]);
    assert!(!store.unsettled());

    let epoch = store.epoch();
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Up);
    assert_eq!(store.epoch(), epoch);
    line(&store, 4).expect("a commit once revived");
    assert_eq!(seqs(&db), vec!["1", "4"]);
    assert_eq!(store.last_seq(), 4);
    assert!(store.take_landings().is_empty());
    assert!(super::logged("store_unreachable"));
    assert!(super::logged("the database session is back"));
    assert_eq!(
        db.rows("SELECT count(*) FROM tx_ledger"),
        vec![vec![Some("2".to_owned())]],
        "each commit that landed made a row of the ledger, the refused ones none"
    );
}

/// Proves: a commit whose `COMMIT` landed but whose answer was lost is refused to its caller, is
/// unsettled until the revival, and is then settled as landed with its id and its audit lines,
/// taken once.
///
/// Catches: a change that is in the database but is told to the caller as not made, and then
/// forgotten.
#[test]
fn a_commit_that_landed_but_was_not_answered_is_settled_as_landed() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("landed").expect("the store opens");
    store
        .on_writer(|w| w.fault = Some(Fault::LoseAfterCommit))
        .expect("the fault is set");
    let held = lines(1, 2);
    let before = Default::default();
    let after = rich_ops();
    let change = Commit {
        ops: Some((&before, &after)),
        audit: &held,
        ..Commit::default()
    };
    assert!(store.commit(&change).is_err());
    assert!(store.unsettled());
    assert_eq!(store.health(), Health::Lost);
    store.revive();
    settle(&store);
    assert!(!store.unsettled());
    assert_eq!(store.health(), Health::Up);
    let landing = store.take_landings().pop().expect("the commit is settled");
    assert!(landing.landed);
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM tx_ledger WHERE id = '{}'",
            landing.id
        )),
        vec![vec![Some("1".to_owned())]],
        "the id is the row the transaction itself inserted, and settling made no second"
    );
    assert_eq!(landing.lines().len(), 2);
    assert_eq!(seqs(&db), vec!["1", "2"]);
    assert_eq!(
        db.rows("SELECT service FROM services ORDER BY service"),
        vec![vec![Some("kbf".to_owned())]]
    );
    assert!(store.take_landings().is_empty(), "a landing is taken once");
}

/// Proves: a commit that was lost before it landed (the session ended while its `COMMIT` ran) is
/// settled as not landed: nothing of it is in the database, and its audit lines are not given back.
///
/// Catches: a change taken into memory that the database does not hold.
#[test]
fn a_commit_that_did_not_land_is_settled_as_not_landed() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("not-landed").expect("the store opens");
    db.run(
        "CREATE FUNCTION test_die() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN PERFORM pg_terminate_backend(pg_backend_pid(), 60000); \
         RETURN NULL; END $$; \
         CREATE CONSTRAINT TRIGGER test_die AFTER INSERT ON tx_ledger DEFERRABLE INITIALLY DEFERRED \
         FOR EACH ROW EXECUTE FUNCTION test_die()",
    )
    .expect("the trigger is set");
    assert!(line(&store, 1).is_err());
    assert!(store.unsettled());
    db.run("DROP TRIGGER test_die ON tx_ledger; DROP FUNCTION test_die()")
        .expect("the trigger is dropped");
    store.revive();
    settle(&store);
    let landing = store.take_landings().pop().expect("the commit is settled");
    assert!(!landing.landed);
    assert!(landing.lines().is_empty());
    assert_eq!(seqs(&db), Vec::<String>::new());
    assert_eq!(
        db.rows(&format!(
            "SELECT count(*) FROM tx_ledger WHERE id = '{}'",
            landing.id
        )),
        vec![vec![Some("1".to_owned())]],
        "settling took the id, so the transaction can never land"
    );
    line(&store, 2).expect("a commit once revived");
    assert_eq!(seqs(&db), vec!["2"]);
}

/// Proves: a coordinator that started while this one's session was lost, and so raised the epoch,
/// does not stop it: the revival brings the session back and writes go on beside the other.
///
/// Catches: a coordinator that exits when another starts, which makes a start-first update stop
/// the instance that is still serving.
#[test]
fn a_store_whose_epoch_was_raised_meanwhile_is_revived() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    let epoch = store.epoch().expect("an epoch");
    db.end_sessions();
    assert!(line(&store, 1).is_err());
    let (other, _) = db.open("other").expect("another coordinator starts");
    assert_eq!(other.epoch(), Some(epoch + 1));
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Up);
    line(&store, 2).expect("a commit once revived");
    line(&other, 3).expect("a commit of the other");
    assert_eq!(seqs(&db), vec!["2", "3"]);
}

/// A commit of unknown outcome of `id`, settled at a revival while another session's transaction
/// that inserted `id` is still in progress; that session then commits when `commit`, else rolls
/// back. Answers whether the settle found it landed. The revival waits for the other transaction
/// at the unique index, and the test ends it when it sees the revival wait, not after a time.
fn settled_behind_a_transaction_in_progress(commit: bool) -> bool {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("behind").expect("the store opens");
    let mut other = db.client();
    let id: String = other
        .query_one("SELECT gen_random_uuid()::text", &[])
        .expect("an id")
        .get(0);
    other
        .batch_execute(&format!(
            "BEGIN; INSERT INTO hostkeeper.tx_ledger (id, at_unix) VALUES ('{id}', 1)"
        ))
        .expect("the transaction is in progress");
    store.inner.link.lose(Some((id.clone(), None)));
    store.revive();
    let name = db.name().to_owned();
    let mut watcher = db.client();
    let mut seen = false;
    for _ in 0..200_000 {
        let waiting: i64 = watcher
            .query_one(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = $1 AND wait_event_type = 'Lock' \
                 AND query LIKE 'INSERT INTO tx_ledger%'",
                &[&name],
            )
            .expect("the activity")
            .get(0);
        if waiting > 0 {
            seen = true;
            break;
        }
        std::thread::yield_now();
    }
    assert!(
        seen,
        "the revival never waited for the transaction in progress"
    );
    other
        .batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
        .expect("the transaction ends");
    settle(&store);
    assert_eq!(store.health(), Health::Up);
    let landing = store.take_landings().pop().expect("the commit is settled");
    assert_eq!(landing.id, id);
    landing.landed
}

/// Proves: a commit of unknown outcome whose transaction is still in progress on another session
/// when the revival looks is settled as landed once that transaction commits: the revival waits
/// for it instead of reading the ledger before it is there.
///
/// Catches: a settle by a plain read of the ledger, which finds nothing while the first backend is
/// still finishing its `COMMIT`, calls the commit lost, and lets a second coordinator redo it.
#[test]
fn a_settle_waits_for_the_transaction_in_progress_and_finds_it_landed() {
    assert!(settled_behind_a_transaction_in_progress(true));
}

/// Proves: the same wait, with the other transaction rolled back, settles the commit as not
/// landed.
///
/// Catches: a settle that calls a rolled-back transaction landed.
#[test]
fn a_settle_behind_a_transaction_that_rolls_back_finds_it_not_landed() {
    assert!(!settled_behind_a_transaction_in_progress(false));
}

/// Proves: a revival that finds the lock held exclusively by another session, or the database
/// unreachable, or refusing the session, or the ledger unwritable, leaves the store lost and tries
/// again at the next heartbeat; once the cause is gone the store is up again.
///
/// Catches: a store that gives up after one failed revival, or that comes up without the lock.
#[test]
fn a_revival_that_cannot_be_made_is_tried_again() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("again").expect("the store opens");
    db.end_sessions();
    assert!(line(&store, 1).is_err());

    // Another session holds the lock.
    let mut holder = db.client();
    holder
        .execute("SELECT pg_advisory_lock($1)", &[&LOCK_KEY])
        .expect("the lock is taken");
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Lost);
    holder
        .execute("SELECT pg_advisory_unlock($1)", &[&LOCK_KEY])
        .expect("the lock is given");
    drop(holder);

    // The database does not take connections.
    let name = db.name().to_owned();
    let alter = move |sql: String| {
        off_runtime(move || {
            admin(None)
                .batch_execute(&sql)
                .expect("the setting changes")
        });
    };
    alter(format!("ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS false"));
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Lost);
    alter(format!("ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS true"));

    // The database no longer makes commits durable.
    alter(format!(
        "ALTER DATABASE \"{name}\" SET synchronous_commit = off"
    ));
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Lost);
    alter(format!(
        "ALTER DATABASE \"{name}\" RESET synchronous_commit"
    ));

    // The ledger cannot be written, and a commit waits to be settled in it.
    store.inner.link.lose(Some((
        "00000000-0000-0000-0000-000000000001".to_owned(),
        None,
    )));
    db.run("ALTER TABLE tx_ledger RENAME TO tx_ledger_away")
        .expect("the table is moved");
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Lost);
    assert!(store.unsettled(), "the commit stays to be settled");
    db.run("ALTER TABLE tx_ledger_away RENAME TO tx_ledger")
        .expect("the table is back");

    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Up);
    let landing = store.take_landings().pop().expect("the commit is settled");
    assert!(!landing.landed, "no transaction of that id ever began");
    line(&store, 2).expect("a commit once revived");
}

/// Proves: a statement the server refuses (a body that fails, a deferred check that fails at the
/// `COMMIT`) is a refusal that keeps the session and writes nothing, and a transaction answers
/// what its body reads.
///
/// Catches: a refused statement that loses the session, or leaves half a transaction.
#[test]
fn a_refused_statement_keeps_the_session_and_writes_nothing() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("refused").expect("the store opens");
    let failed = store
        .write(|tx| tx.execute("INSERT INTO no_such_table VALUES (1)", &[]))
        .expect_err("the statement is refused");
    assert!(matches!(failed, WriteFail::Refused(_)));
    assert_eq!(
        failed.why(),
        "ERROR: relation \"no_such_table\" does not exist (SQLSTATE 42P01)"
    );
    let deferred = store
        .write(|tx| {
            tx.execute(
                "INSERT INTO enrollment_issued \
                 (request_id, round, serial, host_id, not_after, certificate_der) \
                 VALUES ('\\x01', 1, 1, 'h', 1, '\\x02')",
                &[],
            )
        })
        .expect_err("the commit is refused");
    assert!(matches!(deferred, WriteFail::Refused(_)));
    assert!(
        deferred
            .why()
            .starts_with("ERROR: insert or update on table \"enrollment_issued\"")
    );
    assert_eq!(store.health(), Health::Up);
    let answer = store
        .write(|tx| {
            tx.query_one("SELECT 1", &[])?;
            let row = tx.query_one("SELECT 7::int", &[])?;
            Ok(row.get::<_, i32>(0))
        })
        .expect("a transaction reads");
    assert_eq!(answer, 7);
    assert_eq!(
        db.rows("SELECT count(*) FROM enrollment_issued"),
        vec![vec![Some("0".to_owned())]]
    );
    assert!(store.statements() > 0);
}

/// Proves: a writer whose session is gone answers a transaction handed to it as lost, and a store
/// that is stopped answers every call that its sessions stopped, without a panic.
///
/// Catches: a call on a stopped store that waits for ever or panics.
#[test]
fn a_stopped_store_answers_that_its_sessions_stopped() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("stopped").expect("the store opens");
    db.end_sessions();
    assert!(line(&store, 1).is_err());
    let direct = store
        .on_writer(|w| w.transact(None, |_| Ok(())))
        .expect("the writer runs");
    assert_eq!(
        direct.expect_err("no session").why(),
        "the database session is lost"
    );
    assert!(
        store
            .on_writer(|w| w.session().is_none())
            .expect("the writer")
    );

    let (second, _) = {
        store.stop();
        db.open("second").expect("the second opens")
    };
    second.stop();
    let failed = second.write(|_| Ok(())).expect_err("stopped");
    assert_eq!(failed.why(), "the writing session stopped");
    assert_eq!(
        crate::store::Backend::health(&second),
        crate::store::Health::Lost,
        "a store whose writer is gone is lost, and its alert stands"
    );
    assert_eq!(
        second.read(|_| Ok(())).expect_err("stopped"),
        "the reading session stopped"
    );
    assert_eq!(
        second.load().expect_err("stopped").to_string(),
        "database:database: the writing session stopped"
    );
    assert!(second.on_writer(|_| ()).is_none());
    second.stop();
}

/// Proves: the reading session is made when a read first needs it, is dropped when a read finds
/// it gone and made again by the next read, and a read that cannot connect says why in the
/// server's words.
///
/// Catches: a reader that stays dead after one lost connection.
#[test]
fn the_reading_session_is_made_again_after_it_is_lost() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("reader").expect("the store opens");
    let one = |store: &Postgres| {
        store.read(|session| Ok(session.query_one("SELECT 1::int", &[])?.get::<_, i32>(0)))
    };
    assert_eq!(one(&store), Ok(1));
    db.end_sessions();
    // The writer is gone too; the reader finds out at its next read.
    assert!(one(&store).is_err());
    assert_eq!(one(&store), Ok(1));

    db.end_sessions();
    assert!(one(&store).is_err());
    let name = db.name().to_owned();
    let alter = move |sql: String| {
        off_runtime(move || {
            admin(None)
                .batch_execute(&sql)
                .expect("the setting changes")
        });
    };
    alter(format!("ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS false"));
    let refused = one(&store).expect_err("no connection");
    assert!(refused.starts_with("FATAL: "), "{refused}");
    alter(format!("ALTER DATABASE \"{name}\" ALLOW_CONNECTIONS true"));
    alter(format!(
        "ALTER DATABASE \"{name}\" SET synchronous_commit = off"
    ));
    assert_eq!(
        one(&store).expect_err("not durable"),
        "the database has synchronous_commit off; the coordinator acknowledges what is committed, so a commit must be on disk when it returns"
    );
    alter(format!(
        "ALTER DATABASE \"{name}\" RESET synchronous_commit"
    ));
    assert_eq!(one(&store), Ok(1));
}

/// Proves: a session lost while a refused statement is rolled back is lost, not refused: the
/// caller is told it is lost and the store revives.
///
/// Catches: a refusal that keeps a session the server has already ended.
#[test]
fn a_session_lost_in_a_rollback_is_lost() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("rollback").expect("the store opens");
    // A statement is refused with a plain error, and the server ends the session before the
    // rollback that follows: the server is asked to end it, and to answer when it has.
    let failed = store
        .write(|tx| {
            let pid: i32 = tx.query_one("SELECT pg_backend_pid()", &[])?.get(0);
            let refused = tx
                .query_one("SELECT 1/0", &[])
                .expect_err("a plain refusal");
            let ended: bool = admin(None)
                .query_one("SELECT pg_terminate_backend($1, 10000)", &[&pid])
                .expect("the session is ended")
                .get(0);
            assert!(ended);
            Err::<(), _>(refused)
        })
        .expect_err("the statement fails");
    assert!(matches!(failed, WriteFail::Lost(_)), "{failed:?}");
    assert_eq!(store.health(), Health::Lost);
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Up);
}
