//! The instance's row of `instances` (migration `0022`; `docs/design/coordinator-multi-instance.md`,
//! section 4, "Instances"): made at each start, upserted by each beat, and the loop that beats.
//!
//! No test reads a clock: a beat is called by the test, or the loop is run with no wait between
//! beats and ended by its stop channel or by the beat.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use super::{TestDatabase, options, with_session};
use crate::store::postgres::instance::{beat_every, beat_weak};
use crate::store::postgres::migrate;
use crate::store::postgres::writer::{LOCK_KEY, try_lock};
use crate::store::postgres::{Options, Postgres};
use crate::store::{Backend, Health};

/// The options of an instance that runs `release` and listens on `listen`.
fn running(release: &str, listen: &str) -> Options {
    Options {
        release: release.to_owned(),
        listen: listen.to_owned(),
        ..options()
    }
}

/// The row of `store`'s instance: its release, its listen addresses, when it started and when it
/// was last seen.
fn row(db: &TestDatabase, store: &Postgres) -> Vec<Option<String>> {
    let rows = db.rows(&format!(
        "SELECT release, listen, started_unix, seen_unix FROM instances WHERE id = '{}'",
        store.instance().id
    ));
    rows.into_iter().next().unwrap_or_default()
}

/// The number in `text`.
fn number(text: &Option<String>) -> i64 {
    text.as_deref()
        .and_then(|n| n.parse().ok())
        .expect("a number")
}

/// Proves: two coordinators started on one database each make a row of their own, with a random
/// id each, the release and addresses each was given, and `seen_unix` its `started_unix`; a beat
/// moves `seen_unix` of its own row on (from a value set back) and leaves `started_unix`, and the
/// other's row, as they were; a beat after the row was deleted makes it again as it was at the
/// start.
///
/// Catches: instances that share a row, a beat that moves the start time or writes another
/// instance's row, and a row that a delete (the prune of rows long unseen) loses for good while
/// the instance runs.
#[test]
fn each_start_makes_its_instance_row_and_a_beat_moves_it_on() {
    let db = TestDatabase::new();
    let (first, _) = db
        .open_options(&running("r1", "heartbeat=10.0.0.1:8443"), "first")
        .expect("the first starts");
    let (second, _) = db
        .open_options(&running("r2", "heartbeat=10.0.0.2:8443"), "second")
        .expect("the second starts");
    assert_ne!(first.instance().id, second.instance().id);
    assert_eq!(
        db.rows("SELECT count(*) FROM instances"),
        vec![vec![Some("2".to_owned())]]
    );
    let started = row(&db, &first);
    assert_eq!(
        started[..2],
        [
            Some("r1".to_owned()),
            Some("heartbeat=10.0.0.1:8443".to_owned())
        ]
    );
    assert_eq!(started[2], started[3], "seen at its start");
    let other = row(&db, &second);
    assert_eq!(other[0].as_deref(), Some("r2"));

    db.run(&format!(
        "UPDATE instances SET seen_unix = 1 WHERE id = '{}'",
        first.instance().id
    ))
    .expect("its seen time is set back");
    first.beat_instance().expect("a beat");
    let beaten = row(&db, &first);
    assert_eq!(beaten[..3], started[..3], "the start is kept");
    assert!(
        number(&beaten[3]) >= number(&started[2]),
        "seen is moved to the database's clock"
    );
    assert_eq!(row(&db, &second), other, "the other's row is left");

    db.run(&format!(
        "DELETE FROM instances WHERE id = '{}'",
        first.instance().id
    ))
    .expect("the row is deleted");
    first.beat_instance().expect("a beat");
    assert_eq!(row(&db, &first)[..3], started[..3], "made again as it was");
}

/// Proves: one beat of the store's thread upserts its row while the store is there, answers that
/// it goes on while the store is lost (nothing written, no panic), and answers that it stops once
/// the store is dropped.
///
/// Catches: a thread that keeps the store alive or beats a dropped store, and one that stops for
/// good at a lost session instead of beating again once the store is back.
#[test]
fn the_instance_thread_beats_while_the_store_lives() {
    let db = TestDatabase::new();
    let (store, _) = db.open("beating").expect("the store opens");
    db.run("UPDATE instances SET seen_unix = 1")
        .expect("its seen time is set back");
    let weak = Arc::downgrade(&store.inner);
    assert!(beat_weak(&weak));
    assert_ne!(row(&db, &store)[3].as_deref(), Some("1"), "the beat landed");

    db.end_sessions();
    assert!(
        store.beat_instance().is_err(),
        "the beat finds the session gone"
    );
    assert_eq!(store.health(), Health::Lost);
    db.run("UPDATE instances SET seen_unix = 1")
        .expect("its seen time is set back");
    assert!(beat_weak(&weak), "a lost store is beaten again later");
    assert_eq!(row(&db, &store)[3].as_deref(), Some("1"), "nothing written");

    drop(store);
    assert!(!beat_weak(&weak), "a dropped store ends the thread");
}

/// Proves: the loop calls its beat after each wait until the beat answers false; it calls none
/// when it is told to stop before its first wait ends, and none when its stop channel is dropped.
///
/// Catches: a loop that beats once its store is gone, or that outlives the store that stops it.
#[test]
fn the_beat_loop_runs_until_it_is_stopped_or_its_store_is_gone() {
    let (_stop, stopped) = mpsc::channel::<()>();
    let mut beats = 0;
    beat_every(&stopped, Duration::ZERO, || {
        beats += 1;
        beats < 3
    });
    assert_eq!(beats, 3, "until the beat answers false");

    let (stop, stopped) = mpsc::channel();
    stop.send(()).expect("told to stop");
    let mut beaten = false;
    beat_every(&stopped, Duration::ZERO, || {
        beaten = true;
        true
    });
    assert!(!beaten, "told to stop: no beat");

    let (stop, stopped) = mpsc::channel::<()>();
    drop(stop);
    beat_every(&stopped, Duration::ZERO, || {
        beaten = true;
        true
    });
    assert!(!beaten, "its stop dropped: no beat");
}

/// Proves (B8, both ways, for `0022`): the release before's runner (migrations `0001` to `0021`)
/// accepts a database this release made its row of `instances` in, runs nothing, and leaves the
/// row, and the release before takes the coordinator's lock exclusively once this release has
/// stopped; this release, started on a database the release before migrated, runs `0022` alone
/// and makes its row.
///
/// Catches: a `0022` marked `refuse`, which would turn a rollback into a restore, and a release
/// that does not bring the release before's database up to `instances`.
#[test]
fn the_release_before_runs_on_a_database_with_0022_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("this release starts");
    store.stop();
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..21]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(locked, "the release before has the lock once this release stopped");
    assert_eq!(
        db.rows("SELECT count(*) FROM instances"),
        vec![vec![Some("1".to_owned())]]
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..21]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let (store, _) = old.open("this release").expect("this release starts on it");
    assert_eq!(
        old.rows("SELECT max(version) FROM schema_version"),
        vec![vec![Some("22".to_owned())]]
    );
    assert_eq!(
        old.rows("SELECT count(*) FROM instances"),
        vec![vec![Some("1".to_owned())]]
    );
    let mut client = old.client();
    let taken: bool = client
        .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])
        .expect("a try")
        .get(0);
    assert!(!taken, "the release before is refused while this release runs");
    drop(store);
}
