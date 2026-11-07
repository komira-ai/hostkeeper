//! `hostkeeper-coordinator import` and `export` as the binary runs them, through `config::main`:
//! the recorded files of the release before moved into a database and written back out, and the
//! files the export writes held to the strict readers of the release before
//! (`support::readers`), which refuse a key they do not know and a schema newer than their own.
//! What the two verbs move, check and refuse is in the crate's `transfer_tests`.

mod support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use hostkeeper_coordinator::config::main;
use support::database::Database;
use support::readers::Readers;
use support::{recorded_bytes, scratch};

fn args(words: &[&str]) -> Vec<OsString> {
    std::iter::once("hostkeeper-coordinator")
        .chain(words.iter().copied())
        .map(OsString::from)
        .collect()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

/// Writes the connection string of `database` into `path`, owner-only.
fn dsn_file(path: &Path, database: &Database) {
    std::fs::write(path, database.dsn_text()).expect("write the connection string");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("owner-only");
}

/// Proves: the files an export writes are read by the strict readers of the release before: the
/// recorded files of that release, in both forms of the desired-state file, imported into a
/// database and exported again, give a desired-state file, an ops file and a halts file each of
/// its readers takes whole, the halts file naming the digest of the ops file beside it.
///
/// Catches: an export that writes a key the release before refuses, a newer schema, a halts file
/// that does not go with its ops file, or the single form where that release reads the map.
#[test]
fn exported_files_are_read_by_the_strict_readers_of_the_release_before() {
    let readers = Readers::recorded();
    for (desired, legacy, reader) in [
        ("desired-map.json", None, "desired-map"),
        ("desired-single.json", Some("kbf"), "desired-single"),
    ] {
        let from = scratch("transfer-readers-from");
        let state = from.path().join("desired.json");
        std::fs::write(&state, recorded_bytes(desired)).expect("the desired state");
        std::fs::write(
            from.path().join("desired.json.ops"),
            recorded_bytes("ops.json"),
        )
        .expect("the ops file");
        std::fs::write(
            from.path().join("desired.json.ops.halts"),
            recorded_bytes("ops.json.halts"),
        )
        .expect("the halts file");
        let database = Database::new();
        let dsn = from.path().join("database.dsn");
        dsn_file(&dsn, &database);
        let legacy_flags: Vec<&str> = legacy
            .map(|name| vec!["--legacy-service", name])
            .unwrap_or_default();

        let mut import = vec![
            "import",
            "--state-file",
            text(&state),
            "--database-dsn-file",
            text(&dsn),
        ];
        import.extend(&legacy_flags);
        assert_eq!(
            main(args(&import)),
            ExitCode::SUCCESS,
            "{desired}: the plan"
        );
        import.push("--apply");
        assert_eq!(
            main(args(&import)),
            ExitCode::SUCCESS,
            "{desired}: the import"
        );

        let into = scratch("transfer-readers-into");
        let target = into.path().join("desired.json");
        let mut export = vec![
            "export",
            "--database-dsn-file",
            text(&dsn),
            "--state-file",
            text(&target),
            "--into-empty",
        ];
        export.extend(&legacy_flags);
        assert_eq!(
            main(args(&export)),
            ExitCode::SUCCESS,
            "{desired}: the export"
        );
        readers.check_desired_in(into.path(), reader);
        readers.check_ops_in(into.path());
        assert!(
            into.path().join("desired.json.ops.halts").exists(),
            "{desired}: the halt the recorded halts file holds is written back"
        );
    }
}

/// Proves: an import or an export that is refused exits 1: a second import into the database it
/// filled, and an export over the files it wrote.
///
/// Catches: a refused move whose exit code says it was made, which an operator's script would
/// go on from.
#[test]
fn a_refused_import_or_export_exits_1() {
    let from = scratch("transfer-exit");
    let state = from.path().join("desired.json");
    std::fs::write(&state, recorded_bytes("desired-map.json")).expect("the desired state");
    let database = Database::new();
    let dsn = from.path().join("database.dsn");
    dsn_file(&dsn, &database);
    let import = [
        "import",
        "--state-file",
        text(&state),
        "--database-dsn-file",
        text(&dsn),
        "--apply",
    ];
    assert_eq!(main(args(&import)), ExitCode::SUCCESS);
    assert_eq!(
        main(args(&import)),
        ExitCode::from(1),
        "the database holds state"
    );
    let export = [
        "export",
        "--database-dsn-file",
        text(&dsn),
        "--state-file",
        text(&state),
        "--into-empty",
    ];
    assert_eq!(
        main(args(&export)),
        ExitCode::from(1),
        "the targets hold state"
    );
}
