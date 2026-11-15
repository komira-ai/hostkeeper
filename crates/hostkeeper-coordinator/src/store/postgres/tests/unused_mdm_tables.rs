//! The tables the MDM's migrations (`0005`, `0011` and `0012`) made: no code reads or writes
//! them now, and they stay in place, unused, so a release before this one still finds them and a
//! format changes only by addition.

use super::{SPENT_NOW, TestDatabase};
use crate::store::postgres::transfer;

/// Proves: a database whose MDM tables hold rows (a device record, an enrollment, a sealed
/// secret, a setting) starts this release, which reads none of them and leaves each as it was, and
/// the release's `export` of it succeeds without an enrollment directory and counts none of those
/// tables.
///
/// Catches: a start or an export that refuses a database for rows of tables it no longer owns,
/// or that deletes them, which would make the release before this one unable to run on a database
/// this one has started.
#[test]
fn a_database_whose_mdm_tables_hold_rows_starts_and_exports_and_keeps_them() {
    let db = TestDatabase::new();
    let (first, _) = db.open("first").expect("the store opens");
    first.stop();
    db.run(
        "INSERT INTO mdm_devices (enrollment_id, record) VALUES ('mac-1', '\\x0102'); \
         INSERT INTO mdm_enrollments (enrollment_id, serial, udid, state, created_unix) \
           VALUES ('mac-1', 'C02XK1ABJG5J', 'udid-1', 'live', 1); \
         INSERT INTO mdm_secrets (kind, version, sealed, fingerprint, created_unix) \
           VALUES ('push_key', 1, decode(repeat('ab', 28), 'hex'), repeat('0', 64), 1); \
         INSERT INTO mdm_settings (name, value) VALUES ('push_alert_days', '60,14')",
    )
    .expect("the rows are put");

    let (second, loaded) = db.open("second").expect("the start reads none of the rows");
    assert!(loaded.ops.audit.is_empty());
    second.stop();

    let (moved, counts) = transfer::export(&db.dsn(), SPENT_NOW).expect("the export succeeds");
    assert!(moved.records.is_empty(), "no enrollment store is held");
    let tables: Vec<&str> = counts.iter().map(|(table, _)| *table).collect();
    assert!(
        !tables.iter().any(|table| table.starts_with("mdm")),
        "{tables:?}"
    );

    for (table, rows) in [
        ("mdm_devices", 1),
        ("mdm_enrollments", 1),
        ("mdm_secrets", 1),
        ("mdm_settings", 1),
    ] {
        let held = db.rows(&format!("SELECT count(*)::text FROM {table}"));
        assert_eq!(held, [[Some(rows.to_string())]], "{table} keeps its row");
    }
}
