//! `import` and `export` of the registry of package repositories: the repositories file of a file
//! store and the tables `repositories` and `repository_generation` of a database
//! (docs/design/coordinator-storage.md section 3.2).

use super::export;
use super::tests::{Store, clock, count, export_flags, files_loaded, imported, loaded};
use crate::backup_repositories_tests::put_komira;
use crate::lock::FileLock;
use crate::repositories::{parse_file, repositories_path};
use crate::store::postgres::tests::TestDatabase;

/// A file store holding the registry `komira`.
fn store_with_repository(name: &str) -> Store {
    let store = Store::new(name);
    put_komira(&store.paths.state_file);
    store
}

/// Proves: importing a file store that holds a repositories file writes its registry as a row of
/// `repositories` and the one row of `repository_generation` (the report counts one of each), and
/// a start on the database loads the registry a start on the files loads, generation and keyring
/// alike.
///
/// Catches: an import that leaves the repositories file behind, so the database would hold no
/// registry for a coordinator whose hosts were being sent one.
#[test]
fn import_writes_the_repositories_file_as_rows() {
    let store = store_with_repository("transfer-repositories-import");
    assert!(repositories_path(&store.paths.state_file).exists());
    let database = TestDatabase::new();
    let report = imported(&store, &database).expect("imported");
    assert!(report.contains("  repositories: 1 rows"), "{report}");
    assert!(
        report.contains("  repository_generation: 1 rows"),
        "{report}"
    );
    assert_eq!(count(&database, "repositories"), 1);
    let held = loaded(&database);
    let files = files_loaded(&store, None);
    assert_eq!(held.desired.repositories, files.desired.repositories);
    assert_eq!(held.desired.repositories.generation, 1);
}

/// Proves: exporting a database that holds a registry writes the repositories file beside the state
/// file, which reads as the registry the database held and is the file the import came from, and the
/// report counts the rows; a database that holds none is exported with no repositories file; a
/// registry emptied by its last removal is exported with its generation.
///
/// Catches: an export that drops the registry, writes an empty file for a state without one, or
/// loses the generation of an emptied one.
#[test]
fn export_writes_the_repositories_file_only_when_the_database_holds_a_registry() {
    let from = store_with_repository("transfer-repositories-export-from");
    let database = TestDatabase::new();
    imported(&from, &database).expect("imported");
    let into = Store::new("transfer-repositories-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  repositories: 1 rows"), "{report}");
    let written = std::fs::read(repositories_path(&into.paths.state_file)).expect("the file");
    assert_eq!(
        parse_file(&written).expect("the file reads"),
        loaded(&database).desired.repositories
    );
    assert_eq!(
        std::fs::read(repositories_path(&from.paths.state_file)).expect("the original"),
        written
    );

    let plain = Store::recorded("transfer-repositories-none-from");
    let empty = TestDatabase::new();
    imported(&plain, &empty).expect("imported");
    let out = Store::new("transfer-repositories-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    assert!(!repositories_path(&out.paths.state_file).exists());

    database
        .run("DELETE FROM repositories; UPDATE repository_generation SET generation = 2")
        .expect("a registry emptied by its last removal");
    let emptied = Store::new("transfer-repositories-emptied-into");
    export(&export_flags(&database, &emptied), &FileLock, &clock()).expect("exported");
    let text = std::fs::read(repositories_path(&emptied.paths.state_file)).expect("the file");
    assert_eq!(parse_file(&text).expect("reads").generation, 2);
}
