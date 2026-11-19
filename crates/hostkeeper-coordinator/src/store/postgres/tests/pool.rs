//! The pool of writing sessions (`--database-pool`; `docs/design/coordinator-multi-instance.md`,
//! section 4, "Sessions"): each session holds the coordinator's lock shared, writes run side by
//! side on them, a write waits for a free one, and one session lost stops every write until the
//! pool is revived.
//!
//! No test reads a clock: two writes meet at a barrier inside their transactions, and a write that
//! waits for a free session is seen by the pool's count of waits.

use std::sync::mpsc;
use std::sync::{Arc, Barrier};

use postgres::Client;

use super::{SPENT_NOW, TIMEOUTS, TestDatabase, pooled, unix};
use crate::store::postgres::Postgres;
use crate::store::postgres::writer::{LOCK_KEY, WriteFail};
use crate::store::{Backend, Health};

/// The text of a write refused at once because the store is lost.
const LOST: &str = "the database session is lost";

/// How many sessions of `db` hold the coordinator's lock shared.
fn shared_holders(client: &mut Client) -> i64 {
    client
        .query_one(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND mode = 'ShareLock' \
             AND granted AND database = (SELECT oid FROM pg_database \
                                         WHERE datname = current_database()) \
             AND ((classid::bigint << 32) | objid::bigint) = $1",
            &[&LOCK_KEY],
        )
        .expect("the locks")
        .get(0)
}

/// Whether a session of `client` takes the coordinator's lock exclusively at once, as the release
/// before this one does; it is let go at once when it does.
fn exclusive_taken(client: &mut Client) -> bool {
    let taken: bool = client
        .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])
        .expect("a try")
        .get(0);
    client
        .batch_execute("SELECT pg_advisory_unlock_all()")
        .expect("the lock is let go");
    taken
}

/// The store on `db` with a pool of `pool` sessions, its sessions named `name`.
fn open_named(db: &TestDatabase, pool: usize, name: &str) -> Postgres {
    let (store, _) = Postgres::open_with(
        &db.dsn().named(name),
        TIMEOUTS,
        &pooled(pool),
        name,
        &unix(),
        Arc::new(|| SPENT_NOW),
    )
    .expect("the store opens");
    store
}

/// A write of `store` that inserts a row into `spent_tokens`.
fn spend(store: &Postgres, jti: &str) -> Result<u64, WriteFail> {
    let jti = jti.to_owned();
    store.write(move |tx| {
        tx.execute(
            "INSERT INTO spent_tokens (jti, until_unix) VALUES ($1, 1)",
            &[&jti],
        )
    })
}

/// Proves: every session of a pool of three, and the session of heartbeats' decisions, holds the
/// coordinator's lock shared, so the release
/// before this one (an exclusive taker) is refused while the store runs; once the store is dropped
/// no session holds it and the exclusive taker has it at once.
///
/// Catches: a pool session that writes without the fence, so the release before could start and
/// write beside it.
#[test]
fn every_pool_session_holds_the_lock_shared_and_the_release_before_is_refused() {
    let db = TestDatabase::new();
    let store = open_named(&db, 3, "pool-locks");
    let mut client = db.client();
    assert_eq!(
        shared_holders(&mut client),
        4,
        "three and the decisions' own"
    );
    assert!(!exclusive_taken(&mut client));
    drop(store);
    assert_eq!(shared_holders(&mut client), 0);
    assert!(exclusive_taken(&mut client));
}

/// Proves: two writes of a pool of two are open at once, each in a transaction of its own session
/// (they meet at a barrier inside their transactions, which a pool of one never lets them reach
/// together, and their backends differ), and both commit.
///
/// Catches: a pool that runs every write on one session, one after another, so a write waits for
/// every write before it.
#[test]
fn writes_run_side_by_side_on_the_pool_sessions() {
    let db = TestDatabase::new();
    let store = open_named(&db, 2, "pool-side");
    let barrier = Arc::new(Barrier::new(2));
    let write = |jti: &'static str| {
        let (store, barrier) = (store.clone(), Arc::clone(&barrier));
        std::thread::spawn(move || {
            store.write(move |tx| {
                tx.execute(
                    "INSERT INTO spent_tokens (jti, until_unix) VALUES ($1, 1)",
                    &[&jti],
                )?;
                barrier.wait();
                Ok(tx
                    .query_one("SELECT pg_backend_pid()", &[])?
                    .get::<_, i32>(0))
            })
        })
    };
    let (a, b) = (write("a"), write("b"));
    let a = a.join().expect("a's thread").expect("a commits");
    let b = b.join().expect("b's thread").expect("b commits");
    assert_ne!(a, b, "each write ran on a session of its own");
    assert_eq!(
        db.rows("SELECT jti FROM spent_tokens ORDER BY jti"),
        vec![vec![Some("a".to_owned())], vec![Some("b".to_owned())]]
    );
}

/// Proves: with every session of the pool busy, a write waits for one to be free (the pool counts
/// its wait), then runs on it once the write before commits, after it.
///
/// Catches: a write that is refused, or that runs on a busy session inside another write's
/// transaction, when the pool is full.
#[test]
fn a_write_waits_for_a_free_session() {
    let db = TestDatabase::new();
    let store = open_named(&db, 1, "pool-wait");
    let (inside, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let first = {
        let store = store.clone();
        std::thread::spawn(move || {
            store.write(move |tx| {
                tx.execute(
                    "INSERT INTO spent_tokens (jti, until_unix) VALUES ('first', 1)",
                    &[],
                )?;
                inside.send(()).expect("the test listens");
                released.recv().expect("the test releases it");
                Ok(())
            })
        })
    };
    entered
        .recv()
        .expect("the first write is inside its transaction");
    let second = {
        let store = store.clone();
        std::thread::spawn(move || spend(&store, "second"))
    };
    let mut looks = 0;
    while store.waited() == 0 {
        looks += 1;
        assert!(looks < 10_000_000, "the second write never waited");
        std::thread::yield_now();
    }
    release.send(()).expect("the first write is released");
    first
        .join()
        .expect("the first's thread")
        .expect("the first commits");
    assert_eq!(
        second
            .join()
            .expect("the second's thread")
            .expect("the second commits"),
        1
    );
    assert_eq!(store.waited(), 1);
    assert_eq!(
        db.rows("SELECT jti FROM spent_tokens ORDER BY jti"),
        vec![
            vec![Some("first".to_owned())],
            vec![Some("second".to_owned())]
        ]
    );
}

/// Proves: one session of a pool of three ended at the server is found by the write that takes
/// it (the writes take the sessions in turn, so one of three writes in a row does); from then on
/// every write is refused at once, though the other two sessions are up, and the store says it is
/// lost; a revival opens the lost session again and the store is up, every session holding the
/// lock shared again, and a write on each of them commits.
///
/// Catches: a store that goes on writing with part of its pool lost (so an instance whose session
/// is gone still decides), and a revival that leaves a pool session without the lock.
#[test]
fn one_lost_session_stops_every_write_until_the_pool_is_revived() {
    super::logging();
    let db = TestDatabase::new();
    let name = "pool-lost";
    let store = open_named(&db, 3, name);
    let mut client = db.client();
    let ended: bool = client
        .query_one(
            "SELECT pg_terminate_backend((SELECT pid FROM pg_stat_activity \
             WHERE application_name = $1 AND datname = current_database() \
             ORDER BY backend_start LIMIT 1))",
            &[&name],
        )
        .expect("one session is ended")
        .get(0);
    assert!(ended);

    let mut lost = None;
    for n in 0..3 {
        if let Err(fail) = spend(&store, &format!("before-{n}")) {
            lost = Some(n);
            assert!(matches!(fail, WriteFail::Lost(_)), "{fail:?}");
            break;
        }
    }
    let lost = lost.expect("one of three writes in a row takes the ended session");
    assert_eq!(store.health(), Health::Lost);
    assert_eq!(
        spend(&store, "while-lost")
            .expect_err("refused at once")
            .why(),
        LOST
    );
    assert_eq!(
        db.rows("SELECT count(*) FROM spent_tokens"),
        vec![vec![Some(lost.to_string())]],
        "the writes before the loss landed, and none after"
    );

    store.revive();
    store.settle().expect("the writers run");
    assert_eq!(store.health(), Health::Up);
    assert_eq!(
        shared_holders(&mut client),
        4,
        "three and the decisions' own"
    );
    for n in 0..3 {
        spend(&store, &format!("after-{n}")).expect("each session writes again");
    }
}

/// Proves: a writer whose thread is gone (a job of it panicked) is found by the next revival,
/// which cannot hand it the revival: the store is then stopped for good, and every write and load
/// says the writing session stopped, instead of waiting for ever on a revival that never ends.
///
/// Catches: a pool whose dead writer leaves the store "being revived" for ever, so no later
/// heartbeat asks for a revival and the alert says the database is unreachable.
#[test]
fn a_writer_whose_thread_is_gone_stops_the_store_at_the_next_revival() {
    super::logging();
    let db = TestDatabase::new();
    let store = open_named(&db, 2, "pool-dead");
    let gone: Option<()> = store.on_writer(|_| panic!("a job of the writer panics"));
    assert!(gone.is_none(), "the writer's thread is gone");
    db.end_sessions();
    assert!(matches!(spend(&store, "lost"), Err(WriteFail::Lost(_))));
    assert_eq!(store.health(), Health::Lost);
    store.revive();
    assert_eq!(store.health(), Health::Lost);
    assert_eq!(
        spend(&store, "after").expect_err("stopped").why(),
        "the writing session stopped"
    );
    assert_eq!(
        store.load().expect_err("stopped").to_string(),
        "database:database: the writing session stopped"
    );
}

/// Proves: a load that takes a writer whose thread is gone, on a store that still says it is up,
/// stops the store and says the writing session stopped.
///
/// Catches: a load that answers as lost (and so asks for revivals) when no revival can help.
#[test]
fn a_load_that_finds_its_writer_gone_stops_the_store() {
    let db = TestDatabase::new();
    let store = open_named(&db, 1, "pool-load");
    let gone: Option<()> = store.on_writer(|_| panic!("a job of the writer panics"));
    assert!(gone.is_none(), "the writer's thread is gone");
    assert_eq!(store.health(), Health::Up, "no write has found it yet");
    assert_eq!(
        store.load().expect_err("stopped").to_string(),
        "database:database: the writing session stopped"
    );
    assert_eq!(store.health(), Health::Lost);
}
