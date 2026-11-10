//! `import` and `export` of the host groups: the state file's and the ops file's `host_groups` keys
//! and an inventory record's `host_group` of a file store, and the tables of migration `0008` of a
//! database (docs/design/hostgroups.md section 8).

use std::collections::BTreeMap;

use super::export;
use super::tests::{Store, clock, count, export_flags, files_loaded, imported, loaded};
use crate::host_groups::tests::{every_mark, web};
use crate::host_groups::{GroupMarks, HostGroup, HostGroups};
use crate::lock::FileLock;
use crate::store::conformance_tests::{desired_at, rich_ops};
use crate::store::postgres::tests::TestDatabase;

/// A file store holding two groups, `web` with a desired version, the marks of `tierstore` in
/// `web` (and of `hostkeeper`, at their default), and an inventory record of `web`.
fn store_with_groups(name: &str) -> Store {
    let store = Store::new(name);
    let mut held = desired_at("2.0", 'a');
    let canary = HostGroup {
        packages: vec!["hostkeeper".to_owned()],
        generation: 1,
        desired: BTreeMap::new(),
        ..web()
    };
    held.host_groups = HostGroups(BTreeMap::from([
        ("web".parse().expect("a name"), web()),
        ("web-canary".parse().expect("a name"), canary),
    ]));
    let text = crate::desired::encode(&held, None).expect("encodes");
    store.put("desired.json", &text);
    let mut ops = rich_ops();
    ops.audit.clear();
    ops.inventory[0].host_group = "web".to_owned();
    ops.host_groups.0.insert(
        "web".parse().expect("a name"),
        BTreeMap::from([
            ("tierstore".to_owned(), every_mark()),
            ("hostkeeper".to_owned(), GroupMarks::default()),
        ]),
    );
    let (text, halts) = crate::ops_file::texts(&store.ops(), &ops).expect("the ops texts");
    store.put("desired.json.ops", &text);
    if let Some(halts) = halts {
        store.put("desired.json.ops.halts", &halts);
    }
    store
}

/// Proves: importing a file store that holds groups writes them as the rows of `host_groups`,
/// `host_group_packages` and `group_desired`, the marks as the rows of `group_min_versions`,
/// `group_services`, `group_rollouts` and `group_rollout_done` (the pair at its default as none),
/// and the record's group in `inventory` (the report counts the rows); a start on the database
/// loads the groups, their marks and the record a start on the files loads.
///
/// Catches: an import that leaves the groups behind, or reads them back differently and does not
/// roll back, so the database would place hosts by other groups than the files did.
#[test]
fn import_writes_the_groups_and_their_marks_as_rows() {
    let store = store_with_groups("transfer-host-groups-import");
    let database = TestDatabase::new();
    let report = imported(&store, &database).expect("imported");
    for (table, rows) in [
        ("host_groups", 2),
        ("host_group_packages", 3),
        ("group_desired", 1),
        ("group_min_versions", 1),
        ("group_services", 1),
        ("group_rollouts", 1),
        ("group_rollout_done", 1),
    ] {
        assert!(
            report.contains(&format!("  {table}: {rows} rows\n")),
            "{table}: {report}"
        );
        assert_eq!(count(&database, table), rows, "{table}");
    }
    let held = loaded(&database);
    let files = files_loaded(&store, None);
    assert_eq!(held.desired.host_groups, files.desired.host_groups);
    assert_eq!(
        held.ops.host_groups,
        files.ops.host_groups.clone().without_defaults()
    );
    assert_eq!(held.ops.inventory, files.ops.inventory);
    assert_eq!(held.ops.inventory[0].host_group, "web");
}

/// Proves: exporting the database an import of groups wrote gives back the state file the import
/// read, byte for byte, and the ops file with the same groups' marks and records; a database that holds no group exports files that name
/// none.
///
/// Catches: an export that drops a group, a desired version or a mark, or writes the key for a
/// database that holds no group (the release before would then refuse the files).
#[test]
fn export_gives_back_the_files_of_the_groups_byte_for_byte() {
    let from = store_with_groups("transfer-host-groups-export-from");
    let database = TestDatabase::new();
    imported(&from, &database).expect("imported");
    let into = Store::new("transfer-host-groups-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  host_groups: 2 rows\n"), "{report}");
    let read = |store: &Store, file: &str| std::fs::read(store.dir.path().join(file)).expect(file);
    assert_eq!(read(&into, "desired.json"), read(&from, "desired.json"));
    // The ops file is written without the entries at their default the import read and with the
    // import's own audit line; its groups' marks and records are what the import read.
    let ops = |store: &Store| -> serde_json::Value {
        serde_json::from_slice(&read(store, "desired.json.ops")).expect("JSON")
    };
    for key in ["host_groups", "inventory"] {
        assert_eq!(ops(&into)[key], ops(&from)[key], "{key}");
    }

    let plain = Store::recorded("transfer-host-groups-none-from");
    let empty = TestDatabase::new();
    imported(&plain, &empty).expect("imported");
    let out = Store::new("transfer-host-groups-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    for file in ["desired.json", "desired.json.ops"] {
        let text = std::fs::read_to_string(out.dir.path().join(file)).expect("exported");
        assert!(!text.contains("host_group"), "{file}: {text}");
    }
}
