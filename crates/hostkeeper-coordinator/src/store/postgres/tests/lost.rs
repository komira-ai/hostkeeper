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
/// again on the same epoch, and writes go on, nothing written meanwhile.
///
/// Catches: a write that waits on a dead session, a revival that is never asked for, and a store
/// that comes back on another epoch.
#[test]
fn a_lost_session_is_revived_on_the_same_epoch() {
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
    assert!(store.take_landing().is_none());
    assert!(super::logged("store_unreachable"));
    assert!(super::logged(
        "the database session is back, on the same epoch"
    ));
    assert_eq!(
        store
            .on_writer(|w| (w.epoch(), w.commits()))
            .expect("the writer"),
        (1, 2),
        "the epoch and the transactions known committed"
    );
}

/// Proves: a commit whose `COMMIT` landed but whose answer was lost is refused to its caller, is
/// unsettled until the revival, and is then settled as landed with its number and its audit lines,
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
    let landing = store.take_landing().expect("the commit is settled");
    assert!(landing.landed);
    assert_eq!(landing.number, 1);
    assert_eq!(landing.lines().len(), 2);
    assert_eq!(seqs(&db), vec!["1", "2"]);
    assert_eq!(
        db.rows("SELECT service FROM services ORDER BY service"),
        vec![vec![Some("kbf".to_owned())]]
    );
    assert!(store.take_landing().is_none(), "a landing is taken once");
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
         CREATE CONSTRAINT TRIGGER test_die AFTER UPDATE ON lease DEFERRABLE INITIALLY DEFERRED \
         FOR EACH ROW EXECUTE FUNCTION test_die()",
    )
    .expect("the trigger is set");
    assert!(line(&store, 1).is_err());
    assert!(store.unsettled());
    db.run("DROP TRIGGER test_die ON lease; DROP FUNCTION test_die()")
        .expect("the trigger is dropped");
    store.revive();
    settle(&store);
    let landing = store.take_landing().expect("the commit is settled");
    assert!(!landing.landed);
    assert_eq!(landing.number, 1);
    assert!(landing.lines().is_empty());
    assert_eq!(seqs(&db), Vec::<String>::new());
    // The number of the commit that did not land is taken by the next.
    line(&store, 2).expect("a commit once revived");
    assert_eq!(seqs(&db), vec!["2"]);
}

/// Proves: when another coordinator led the database while the session was lost, the revival
/// finds another epoch and deposes the store with the epoch found; a deposed store refuses every
/// write and does not revive.
///
/// Catches: a coordinator that goes on writing over the state of the one that took its place.
#[test]
fn a_store_whose_epoch_was_raised_meanwhile_is_deposed() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("deposed").expect("the store opens");
    let epoch = store.epoch().expect("an epoch");
    db.end_sessions();
    assert!(line(&store, 1).is_err());
    let mut client = db.client();
    client
        .batch_execute(&format!(
            "SELECT pg_advisory_lock({LOCK_KEY}); \
             UPDATE hostkeeper.lease SET epoch = epoch + 1; \
             SELECT pg_advisory_unlock({LOCK_KEY})"
        ))
        .expect("another coordinator leads");
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Deposed { found: epoch + 1 });
    assert_eq!(line(&store, 2).expect_err("deposed").to_string(), LOST);
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Deposed { found: epoch + 1 });
    assert_eq!(seqs(&db), Vec::<String>::new());
}

/// Proves: a commit that finds the lease's epoch no longer its own loses the session at once, and
/// the revival deposes the store.
///
/// Catches: a commit made over a lease another coordinator holds.
#[test]
fn a_commit_that_finds_another_epoch_loses_the_session() {
    super::logging();
    let db = TestDatabase::new();
    let (store, _) = db.open("moved").expect("the store opens");
    let epoch = store.epoch().expect("an epoch");
    db.run("ALTER TABLE lease DISABLE TRIGGER lease_fenced; UPDATE lease SET epoch = epoch + 1")
        .expect("the epoch is raised");
    assert_eq!(
        line(&store, 1).expect_err("the epoch moved").to_string(),
        "database:database: the lease's epoch is no longer this coordinator's"
    );
    assert_eq!(store.health(), Health::Lost);
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Deposed { found: epoch + 1 });
}

/// Proves: a revival that finds the lock held by another session, or the database unreachable,
/// or refusing the session, leaves the store lost and tries again at the next heartbeat; once the
/// cause is gone the store is up again.
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

    // The lease cannot be read.
    db.run("ALTER TABLE lease RENAME TO lease_away")
        .expect("the table is moved");
    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Lost);
    db.run("ALTER TABLE lease_away RENAME TO lease")
        .expect("the table is back");

    store.revive();
    settle(&store);
    assert_eq!(store.health(), Health::Up);
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
            tx.query("SELECT 1", &[])?;
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
        .on_writer(|w| w.transact(|_| Ok(())))
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
/// caller is told it is lost and the store revives; and a writer that is deposed while it still
/// holds a session stays deposed when that session finds the epoch moved.
///
/// Catches: a refusal that keeps a session the server has already ended, and a deposed store that
/// is turned back into a lost one.
#[test]
fn a_session_lost_in_a_rollback_is_lost_and_a_deposed_store_stays_deposed() {
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

    // The epoch moves under a session that still holds the lock, which is then given up.
    let epoch = store.epoch().expect("an epoch");
    db.run("ALTER TABLE lease DISABLE TRIGGER lease_fenced; UPDATE lease SET epoch = epoch + 1")
        .expect("the epoch is raised");
    let answer = store
        .on_writer(|w| {
            w.session()
                .expect("a session")
                .query_one("SELECT pg_advisory_unlock($1)", &[&LOCK_KEY])
                .expect("the lock is given up");
            w.revive();
            w.transact(|_| Ok(()))
        })
        .expect("the writer runs");
    assert_eq!(
        answer.expect_err("the epoch moved").why(),
        "the lease's epoch is no longer this coordinator's"
    );
    assert_eq!(store.health(), Health::Deposed { found: epoch + 1 });
}
