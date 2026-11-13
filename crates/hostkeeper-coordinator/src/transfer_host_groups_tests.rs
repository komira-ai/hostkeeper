//! `export` of the host groups: the tables of migration `0008` of a database written as the state
//! file's and the ops file's `host_groups` keys and an inventory record's `host_group` of a file
//! store (docs/design/hostgroups.md section 8).

use std::collections::BTreeMap;

use super::export;
use super::tests::{Store, clock, export_flags, seeded};
use crate::host_groups::tests::{every_mark, web};
use crate::host_groups::{GroupMarks, HostGroup, HostGroups};
use crate::lock::FileLock;
use crate::store::conformance_tests::{desired_at, rich_ops};
use crate::store::postgres::tests::TestDatabase;

/// A file store holding three groups, the built-in `all` and two others, `web` with a desired version, the marks of `tierstore` in
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
    ]))
    .with_all();
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

/// Proves: exporting a database that holds the groups of a file store gives back that store's
/// state file, byte for byte, and its ops file with the same groups' marks and records; a database
/// that holds no group exports files that name none.
///
/// Catches: an export that drops a group, a desired version or a mark, or writes the key for a
/// database that holds no group (the release before would then refuse the files).
#[test]
fn export_gives_back_the_files_of_the_groups_byte_for_byte() {
    let from = store_with_groups("transfer-host-groups-export-from");
    let database = TestDatabase::new();
    seeded(&from, &database);
    let into = Store::new("transfer-host-groups-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  host_groups: 3 rows\n"), "{report}");
    let read = |store: &Store, file: &str| std::fs::read(store.dir.path().join(file)).expect(file);
    assert_eq!(read(&into, "desired.json"), read(&from, "desired.json"));
    // The ops file is written without the entries at their default the store's file holds; its
    // groups' marks and records are the store's.
    let ops = |store: &Store| -> serde_json::Value {
        serde_json::from_slice(&read(store, "desired.json.ops")).expect("JSON")
    };
    for key in ["host_groups", "inventory"] {
        assert_eq!(ops(&into)[key], ops(&from)[key], "{key}");
    }

    let plain = Store::recorded("transfer-host-groups-none-from");
    let empty = TestDatabase::new();
    seeded(&plain, &empty);
    let out = Store::new("transfer-host-groups-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    for file in ["desired.json", "desired.json.ops"] {
        let text = std::fs::read_to_string(out.dir.path().join(file)).expect("exported");
        assert!(!text.contains("host_group"), "{file}: {text}");
    }
}
