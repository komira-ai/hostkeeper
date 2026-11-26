//! The column `0032` adds to a maintenance run (`xcode_sha256` of `maintenance_runs`; the archive a
//! run of `probe` checks): kept through the real store, `NULL` in a row written without it, a text
//! that is not a SHA-256 refused by the table, and the release before runs beside it. Nothing here
//! reads a clock.

use super::{TestDatabase, with_session};
use crate::maintenance_runs::tests::sample;
use crate::store::postgres::migrate;
use crate::store::{Backend, Commit};

/// The SQL of the run `r` written by the columns of `0029` alone, as the release before writes it,
/// with `columns` (each led by a comma) and `values` (the same) added after the ones it has.
fn run_with(columns: &str, values: &str) -> String {
    format!(
        "INSERT INTO maintenance_runs (run_id, kinds, host_ids, window_start_unix, \
         window_end_unix, take_last_host, reason, by_name, scheduled_unix, position{columns}) \
         VALUES ('r', '{{}}', '{{}}', 0, 0, false, '', 'op', 0, 0{values})"
    )
}

/// Proves: the SHA-256 of the archive a run checks is kept in its row, read back equal after a
/// restart, and a run that checks none is a `NULL` column; a row written without the column (as
/// the release before writes it) reads as a run with an empty `xcode_sha256`; and the table
/// refuses a text that is not 64 lower-case hex characters.
///
/// Catches: an archive dropped by the rows (a probe run that checks nothing after a restart), and
/// a `0032` without its check.
#[test]
fn the_archive_a_run_checks_is_kept_and_a_row_without_it_reads_none() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let runs = sample();
    let sha256 = runs[2].xcode_sha256.clone();
    assert_eq!(sha256.len(), 64);
    let mut full = loaded.ops.clone();
    full.maintenance_runs = runs.clone();
    store
        .commit(&Commit {
            ops: Some((&loaded.ops, &full)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(
        db.rows("SELECT run_id, xcode_sha256 FROM maintenance_runs ORDER BY position"),
        vec![
            vec![Some("run-1900000000-1".to_owned()), None],
            vec![Some("run-1900000050-1".to_owned()), None],
            vec![Some("run-1900000090-2".to_owned()), Some(sha256)],
        ]
    );
    store.stop();
    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.ops.maintenance_runs, runs);
    store.stop();

    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    for bad in [
        "'ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB'",
        "'abab'",
        "''",
        &format!("'{}'", "g".repeat(64)),
        &format!("'{}'", "a".repeat(65)),
    ] {
        assert!(
            db.run(&run_with(", xcode_sha256", &format!(", {bad}")))
                .is_err(),
            "{bad}"
        );
    }
    db.run(&run_with("", ""))
        .expect("a row of the columns before");
    store.stop();
    let (store, read) = db.open("this release").expect("it starts again");
    assert_eq!(read.ops.maintenance_runs[0].xcode_sha256, "");
    store.stop();
}

/// Proves (B8, both ways, for `0032`): the release before (migrations `0001` to `0031`) runs on a
/// database this release migrated and holds a run that names an archive, running nothing; this
/// release, started on a database the release before migrated, runs `0032` alone.
///
/// Catches: a `0032` that is not `may-run`, which would turn a rollback into a restore.
#[test]
fn the_release_before_runs_on_a_database_with_0032_and_this_one_on_its() {
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
        let before = migrate::checked(&migrate::SHIPPED[..31]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..31]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 31"),
        [vec![Some("32".to_owned())]],
        "0032 alone ran"
    );
}
