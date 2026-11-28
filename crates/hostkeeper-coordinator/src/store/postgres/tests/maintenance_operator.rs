//! The two columns `0031` adds to a host's turn of a maintenance run (`queued` and `order_try` of
//! `maintenance_hosts`; `docs/design/maintenance-updates.md`, sections 5.4 and 5.7): kept through
//! the real store, none and 0 in a row written without them, an unknown act refused by the table,
//! and the release before runs beside them. Nothing here reads a clock.

use super::{TestDatabase, with_session};
use crate::maintenance_runs::tests::sample;
use crate::maintenance_runs::{Act, Step};
use crate::store::postgres::migrate;
use crate::store::{Backend, Commit};

/// The SQL of a host row of the run `r` written by the columns of `0030` alone, as the release
/// before writes it.
const ROW_BEFORE: &str = "INSERT INTO maintenance_hosts (run_id, host_id, step, since_unix, \
     kinds, applied, reason, position) VALUES ('r', 'h', 'halted', 0, '{}', '{}', '', 0)";

/// The SQL of the run `r` the row of [`ROW_BEFORE`] is a host of.
const RUN_BEFORE: &str = "INSERT INTO maintenance_runs (run_id, kinds, host_ids, \
     window_start_unix, window_end_unix, take_last_host, reason, by_name, scheduled_unix, \
     position) VALUES ('r', '{}', '{}', 0, 0, false, '', 'op', 0, 0)";

/// The SQL of a host row of `r` that sets the `queued` column to `queued` and `order_try` to `try`.
fn row_with(queued: &str, order_try: &str) -> String {
    ROW_BEFORE
        .replace("position)", "position, queued, order_try)")
        .replace("'', 0)", &format!("'', 0, {queued}, {order_try})"))
}

/// Proves: a halted host's queued act and last order try are kept in the rows, each act under its
/// name, and a restart reads them back equal; a row written without them (as the release before
/// writes it) reads as no act and try 0; and the table refuses an act it does not know and a
/// negative try.
///
/// Catches: an act or a try dropped by the rows (a queued retry never sent after a restart, or
/// sent again under the older try), and a `0031` without its checks.
#[test]
fn a_queued_act_and_the_order_try_are_kept_and_a_row_without_them_reads_none_and_zero() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let mut runs = sample();
    let hosts = &mut runs[0].hosts;
    hosts[0].step = Step::Halted;
    hosts[0].queued = Some(Act::Retry);
    hosts[0].order_try = 2;
    hosts[1].step = Step::Halted;
    hosts[1].queued = Some(Act::PutBack);
    hosts[1].order_try = 7;
    let mut full = loaded.ops.clone();
    full.maintenance_runs = runs.clone();
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(
        db.rows(
            "SELECT queued, order_try FROM maintenance_hosts \
             WHERE queued IS NOT NULL ORDER BY queued"
        ),
        vec![
            vec![Some("put_back".to_owned()), Some("7".to_owned())],
            vec![Some("retry".to_owned()), Some("2".to_owned())],
        ]
    );
    store.stop();
    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.ops.maintenance_runs, runs);
    store.stop();

    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    db.run(RUN_BEFORE).expect("a run");
    assert!(db.run(&row_with("'nap'", "0")).is_err(), "an unknown act");
    assert!(db.run(&row_with("NULL", "-1")).is_err(), "a try below 0");
    db.run(ROW_BEFORE).expect("a row of the columns before");
    store.stop();
    let (store, read) = db.open("this release").expect("it starts again");
    let host = &read.ops.maintenance_runs[0].hosts[0];
    assert_eq!(
        (host.step, host.queued, host.order_try),
        (Step::Halted, None, 0)
    );
    store.stop();
}

/// Proves (B8, both ways, for `0031`): the release before (migrations `0001` to `0030`) runs on a
/// database this release migrated and holds queued acts, running nothing; this release, started on
/// a database the release before migrated, runs `0031` to `0033`.
///
/// Catches: a `0031` that is not `may-run`, which would turn a rollback into a restore.
#[test]
fn the_release_before_runs_on_a_database_with_0031_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("this release starts");
    let mut runs = sample();
    runs[0].hosts[0].step = Step::Halted;
    runs[0].hosts[0].queued = Some(Act::Retry);
    runs[0].hosts[0].order_try = 2;
    let mut full = loaded.ops.clone();
    full.maintenance_runs = runs;
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    store.stop();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..30]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..30]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 30 ORDER BY version"),
        [
            vec![Some("31".to_owned())],
            vec![Some("32".to_owned())],
            vec![Some("33".to_owned())]
        ],
        "0031 ran, and 0032 and 0033 after it"
    );
}
