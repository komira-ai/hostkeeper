//! `export` held to the strict readers of kbf v0.0.64, the release before the coordinator: the
//! mirror of them the integration tests keep (`tests/support/readers.rs`), which refuses a key
//! they do not know and a schema newer than their own.

use std::path::Path;

use super::export;
use super::tests::{DESIRED_MAP, DESIRED_SINGLE, HALTS, OPS, Store, clock, export_flags, stored};
use crate::lock::FileLock;
use crate::store::postgres::tests::TestDatabase;
use crate::store::postgres::tests::seed::seed;

#[path = "../tests/support/readers.rs"]
#[allow(
    dead_code,
    reason = "the integration tests' mirror of the readers, of which this test uses the checks of the desired-state and ops files"
)]
mod readers;

/// The text of the recorded fixture `name`, as the integration tests' `support::recorded` reads
/// it: what the mirror of the readers reads its table from.
fn recorded(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recorded")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|why| panic!("the fixture {name}: {why}"))
}

/// Proves: the files an export writes are read by the strict readers of the release before: a
/// database holding the recorded files of that release, in both forms of the desired-state file,
/// exports a desired-state file, an ops file and a halts file each of its readers takes whole, the
/// halts file naming the digest of the ops file beside it.
///
/// Catches: an export that writes a key the release before refuses, a newer schema, a halts file
/// that does not go with its ops file, or the single form where that release reads the map.
#[test]
fn exported_files_are_read_by_the_strict_readers_of_the_release_before() {
    let readers = readers::Readers::recorded();
    for (desired, legacy, reader) in [
        (DESIRED_MAP, None, "desired-map"),
        (DESIRED_SINGLE, Some("kbf"), "desired-single"),
    ] {
        let from = Store::new("transfer-readers-from");
        from.put("desired.json", desired);
        from.put("desired.json.ops", OPS);
        from.put("desired.json.ops.halts", HALTS);
        let database = TestDatabase::new();
        seed(&database, &stored(&from, legacy));

        let into = Store::new("transfer-readers-into");
        let mut flags = export_flags(&database, &into);
        flags.legacy_service = legacy.map(str::to_owned);
        export(&flags, &FileLock, &clock()).expect("exported");
        readers.check_desired_in(into.dir.path(), reader);
        readers.check_ops_in(into.dir.path());
        assert!(
            into.dir.path().join("desired.json.ops.halts").exists(),
            "{reader}: the halt the recorded halts file holds is written back"
        );
    }
}
