//! `export` of the lab's layout: the tables `lab_rows`, `lab_racks` and `lab_placements` of a
//! database become the layout file of a file store.

use super::export;
use super::tests::{Store, clock, count, export_flags, loaded, seeded};
use crate::backup_layout_tests::put_layout;
use crate::layout::{Layout, LayoutStore, layout_path, parse_file};
use crate::lock::FileLock;
use crate::store::conformance_tests::rich_layout;
use crate::store::postgres::tests::TestDatabase;

/// Proves: a database holding a layout (filled from the layout file of a recorded file store) is
/// exported with the layout file beside the state file, which reads as the layout the database
/// held and is byte for byte the file the database was filled from, and the report counts each
/// table's rows.
///
/// Catches: an export that drops the layout, so a database moved back to files would forget where
/// the hosts stand.
#[test]
fn export_writes_the_layout_as_the_layout_file() {
    let from = Store::new("transfer-layout-export-from");
    put_layout(&from.paths.state_file);
    let database = TestDatabase::new();
    seeded(&from, &database);
    assert_eq!(count(&database, "lab_rows"), 1);
    assert_eq!(count(&database, "lab_racks"), 2);
    assert_eq!(count(&database, "lab_placements"), 2);
    assert_eq!(loaded(&database).layout, rich_layout());

    let into = Store::new("transfer-layout-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();

    assert!(report.contains("  lab_rows: 1 rows"), "{report}");
    assert!(report.contains("  lab_racks: 2 rows"), "{report}");
    assert!(report.contains("  lab_placements: 2 rows"), "{report}");
    let path = layout_path(&into.paths.state_file);
    let written = std::fs::read(&path).expect("the file");
    assert_eq!(parse_file(&path, &written).expect("reads"), rich_layout());
    assert_eq!(
        std::fs::read(layout_path(&from.paths.state_file)).expect("the original"),
        written
    );
    assert_eq!(
        LayoutStore::open(&path).expect("a store").snapshot(),
        rich_layout()
    );
}

/// Proves: a database that holds no layout is exported with no layout file.
///
/// Catches: an empty layout file written for a state without one.
#[test]
fn export_writes_no_layout_file_for_a_database_without_a_layout() {
    let plain = Store::recorded("transfer-layout-none-from");
    let empty = TestDatabase::new();
    seeded(&plain, &empty);
    assert_eq!(loaded(&empty).layout, Layout::default());

    let out = Store::new("transfer-layout-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    assert!(!layout_path(&out.paths.state_file).exists());
}
