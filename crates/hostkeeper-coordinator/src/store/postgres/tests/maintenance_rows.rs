//! The maintenance run rows (`maintenance_runs` and `maintenance_hosts`, migration `0029`;
//! `docs/design/maintenance-updates.md`, sections 5.1 and 5.7): the tables refuse what the reader
//! refuses, the runs of the operations state are held in them through the real store, and the
//! release before runs beside them. Nothing here reads a clock.

use super::{TestDatabase, with_session};
use crate::maintenance_runs::tests::sample;
use crate::store::postgres::migrate;
use crate::store::{Backend, Commit};

/// How many rows `table` holds.
fn count(db: &TestDatabase, table: &str) -> String {
    db.rows(&format!("SELECT count(*) FROM {table}"))[0][0]
        .clone()
        .expect("a count")
}

/// The hosts of `runs`, all runs together.
fn host_total(runs: &[crate::maintenance_runs::Run]) -> usize {
    runs.iter().map(|run| run.hosts.len()).sum()
}

/// The SQL of a host row whose step is `step`.
fn host_row(step: &str) -> String {
    format!(
        "INSERT INTO maintenance_hosts (run_id, host_id, step, since_unix, kinds, applied, \
         reason, position) VALUES ('r', 'h', '{step}', 0, '{{}}', '{{}}', '', 0)"
    )
}

/// The SQL of a run row with the given `batch` and `scheduled_unix`.
fn run_row(batch: &str, scheduled: &str) -> String {
    format!(
        "INSERT INTO maintenance_runs (run_id, kinds, host_ids, window_start_unix, \
         window_end_unix, batch, take_last_host, reason, by_name, scheduled_unix, position) \
         VALUES ('r', '{{}}', '{{}}', 0, 0, {batch}, false, '', 'op', {scheduled}, 0)"
    )
}

/// Proves: the host table accepts each step the reader knows and refuses a step it has no name
/// for, so that a row the coordinator reads always has a step it knows.
///
/// Catches: a `0029` without its check on `step`.
#[test]
fn the_host_table_refuses_a_step_that_is_not_a_known_one() {
    let db = TestDatabase::new();
    let (store, _) = db.open("maintenance-rows").expect("the store opens");
    db.run(&host_row("halted")).expect("a known step is kept");
    db.run("DELETE FROM maintenance_hosts").expect("cleared");
    assert!(db.run(&host_row("sleeping")).is_err());
    assert_eq!(count(&db, "maintenance_hosts"), "0");
    store.stop();
}

/// Proves: the run table refuses a batch of 0 and a negative time, and keeps a run that has a
/// batch of 1 and the time 0.
///
/// Catches: a `0029` without its checks on `batch` and on the times, which would let a row be
/// stored that the coordinator would not have written.
#[test]
fn the_run_table_refuses_a_batch_of_zero_and_a_negative_time() {
    let db = TestDatabase::new();
    let (store, _) = db.open("maintenance-rows").expect("the store opens");
    assert!(db.run(&run_row("0", "0")).is_err(), "a batch of 0");
    assert!(db.run(&run_row("1", "-1")).is_err(), "a negative time");
    assert_eq!(count(&db, "maintenance_runs"), "0");
    db.run(&run_row("1", "0")).expect("the boundary is kept");
    assert_eq!(count(&db, "maintenance_runs"), "1");
    store.stop();
}

/// Proves: runs committed with their hosts are rows in both tables, one for each run and one for
/// each host's turn, and a restart loads the runs back equal; a commit that removes one run deletes
/// its rows of both tables and keeps the other's; a commit that removes the rest empties both.
///
/// Catches: a host's turn left behind in `maintenance_hosts` when its run is removed, a run
/// that comes back changed from the rows, and a run kept in memory only.
#[test]
fn runs_with_hosts_are_rows_in_both_tables_and_follow_their_commits() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let runs = sample();
    assert!(runs.len() > 1, "the sample has runs to remove one by one");
    let mut full = loaded.ops.clone();
    full.maintenance_runs = runs.clone();
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(count(&db, "maintenance_runs"), runs.len().to_string());
    assert_eq!(
        count(&db, "maintenance_hosts"),
        host_total(&runs).to_string()
    );
    store.stop();

    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.ops.maintenance_runs, runs);
    let mut fewer = again.ops.clone();
    fewer.maintenance_runs.truncate(1);
    store
        .commit(&Commit {
            ops: Some((&again.ops, &fewer)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(count(&db, "maintenance_runs"), "1");
    assert_eq!(
        count(&db, "maintenance_hosts"),
        host_total(&runs[..1]).to_string()
    );
    let mut none = fewer.clone();
    none.maintenance_runs.clear();
    store
        .commit(&Commit {
            ops: Some((&fewer, &none)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(count(&db, "maintenance_runs"), "0");
    assert_eq!(count(&db, "maintenance_hosts"), "0");
    store.stop();
    let (store, last) = db.open("this release").expect("it starts again");
    assert!(last.ops.maintenance_runs.is_empty());
    store.stop();
}

/// Proves (B8, both ways, for `0029`): the release before (migrations `0001` to `0028`) runs on a
/// database this release migrated and holds runs, running nothing and without its statements
/// touching the tables it does not know; this release, started on a database the release before
/// migrated, runs `0029` alone and makes both tables empty.
///
/// Catches: a `0029` that is not `may-run`, which would turn a rollback into a restore, and a
/// table the start does not make.
#[test]
fn the_release_before_runs_on_a_database_with_0029_and_this_one_on_its() {
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
        let before = migrate::checked(&migrate::SHIPPED[..28]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());
    let (store, again) = db.open("this release again").expect("starts again");
    assert_eq!(again.ops.maintenance_runs, sample());
    store.stop();

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..28]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    assert_eq!(
        old.rows(
            "SELECT count(*) FROM pg_class WHERE relkind = 'r' \
             AND relname IN ('maintenance_runs', 'maintenance_hosts')"
        ),
        vec![vec![Some("0".to_owned())]],
        "the release before has neither table"
    );
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 28 ORDER BY version"),
        [
            vec![Some("29".to_owned())],
            vec![Some("30".to_owned())],
            vec![Some("31".to_owned())]
        ],
        "0029 ran, and 0030 and 0031 after it"
    );
    assert_eq!(count(&old, "maintenance_runs"), "0");
    assert_eq!(count(&old, "maintenance_hosts"), "0");
}
