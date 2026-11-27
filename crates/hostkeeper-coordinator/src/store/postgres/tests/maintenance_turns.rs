//! The two counts `0030` adds to a host's turn of a maintenance run (`offered_again` and
//! `baked_beats` of `maintenance_hosts`; `docs/design/maintenance-updates.md`, sections 5.3, 5.6
//! and 5.7): kept through the real store, refused below 0, 0 in a row written without them, and
//! the release before runs beside them. Nothing here reads a clock.

use super::{TestDatabase, with_session};
use crate::maintenance_runs::Step;
use crate::maintenance_runs::tests::sample;
use crate::store::postgres::migrate;
use crate::store::{Backend, Commit};

/// The SQL of a host row of the run `r` written by the columns of `0029` alone, as the release
/// before writes it.
const ROW_BEFORE: &str = "INSERT INTO maintenance_hosts (run_id, host_id, step, since_unix, \
     kinds, applied, reason, position) VALUES ('r', 'h', 'deferred', 0, '{}', '{}', '', 0)";

/// The SQL of the run `r` the row of [`ROW_BEFORE`] is a host of.
const RUN_BEFORE: &str = "INSERT INTO maintenance_runs (run_id, kinds, host_ids, \
     window_start_unix, window_end_unix, take_last_host, reason, by_name, scheduled_unix, \
     position) VALUES ('r', '{}', '{}', 0, 0, false, '', 'op', 0, 0)";

/// Proves: a host's offers after a deferral and its bake are kept in the rows, and a restart reads
/// them back equal; a row written without them (as the release before writes it) reads both as 0;
/// and the table refuses a negative count of either.
///
/// Catches: a count dropped by the rows (a deferred host offered again would be read back under its
/// first order id, or a baking host would free its batch place at a restart), and a `0030`
/// without its checks.
#[test]
fn the_counts_of_a_turn_are_kept_and_a_row_without_them_reads_zero() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let runs = sample();
    let counted: Vec<(u32, u32)> = runs
        .iter()
        .flat_map(|run| &run.hosts)
        .map(|host| (host.offered_again, host.baked_beats))
        .filter(|counts| *counts != (0, 0))
        .collect();
    assert_eq!(counted, vec![(0, 2), (1, 0)], "the sample counts both");
    let mut full = loaded.ops.clone();
    full.maintenance_runs = runs.clone();
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    store.stop();
    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.ops.maintenance_runs, runs);
    store.stop();

    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    db.run(RUN_BEFORE).expect("a run");
    for column in ["offered_again", "baked_beats"] {
        let below = ROW_BEFORE
            .replace("position)", &format!("position, {column})"))
            .replace("'', 0)", "'', 0, -1)");
        assert!(db.run(&below).is_err(), "{column} below 0");
    }
    db.run(ROW_BEFORE).expect("a row of the columns before");
    store.stop();
    let (store, read) = db.open("this release").expect("it starts again");
    let host = &read.ops.maintenance_runs[0].hosts[0];
    assert_eq!(
        (host.step, host.offered_again, host.baked_beats),
        (Step::Deferred, 0, 0)
    );
    store.stop();
}

/// Proves (B8, both ways, for `0030`): the release before (migrations `0001` to `0029`) runs on a
/// database this release migrated and holds runs with both counts set, running nothing; this
/// release, started on a database the release before migrated, runs `0030` alone.
///
/// Catches: a `0030` that is not `may-run`, which would turn a rollback into a restore.
#[test]
fn the_release_before_runs_on_a_database_with_0030_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("this release starts");
    let mut full = loaded.ops.clone();
    full.maintenance_runs = sample();
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    store.stop();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..29]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..29]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 29 ORDER BY version"),
        [
            vec![Some("30".to_owned())],
            vec![Some("31".to_owned())],
            vec![Some("32".to_owned())],
            vec![Some("33".to_owned())]
        ],
        "0030 to 0033 after it ran"
    );
}
