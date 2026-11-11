//! `export` of the releases: the table `releases` of a database written as the releases file of a
//! file store (docs/design/packages.md section 3.6).

use super::export;
use super::tests::{Store, clock, export_flags, loaded, seeded};
use crate::desired::DesiredStore;
use crate::lock::FileLock;
use crate::releases::{parse_file, releases_path};
use crate::store::conformance_tests::desired_with_release;
use crate::store::postgres::tests::TestDatabase;
use crate::write_record::WriteAsk;

/// A file store holding `kbf` at `2.0` set with a release.
fn store_with_release(name: &str) -> Store {
    let store = Store::new(name);
    let held = desired_with_release("2.0", 'c');
    DesiredStore::open(&store.paths.state_file, None)
        .expect("a store")
        .record_as(
            "kbf",
            held.desired["kbf"].clone(),
            |_| false,
            &WriteAsk::default(),
            Some(held.releases["kbf"]["2.0"].clone()),
        )
        .expect("the set commits");
    store
}

/// Proves: exporting a database that holds a release writes the releases file beside the state
/// file, which reads as the release the database held, and the report counts the row; a database
/// that holds no release is exported with no releases file.
///
/// Catches: an export that drops the releases, or writes an empty file for a state without any.
#[test]
fn export_writes_the_releases_file_only_when_the_database_holds_a_release() {
    let from = store_with_release("transfer-releases-export-from");
    let database = TestDatabase::new();
    seeded(&from, &database);
    let into = Store::new("transfer-releases-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  releases: 1 rows"), "{report}");
    let written = std::fs::read(releases_path(&into.paths.state_file)).expect("the file");
    let books = parse_file(&written).expect("the file reads");
    assert_eq!(books, loaded(&database).desired.releases);
    assert_eq!(
        std::fs::read(releases_path(&from.paths.state_file)).expect("the original"),
        written
    );

    let plain = Store::recorded("transfer-releases-none-from");
    let empty = TestDatabase::new();
    seeded(&plain, &empty);
    let out = Store::new("transfer-releases-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    assert!(!releases_path(&out.paths.state_file).exists());
}
