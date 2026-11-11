//! `hostkeeper-coordinator export` as the binary runs it, through `config::main`: its exit code
//! when it writes and when it is refused. What an export writes, checks and refuses, and the files
//! it writes held to the strict readers of the release before, are in the crate's
//! `transfer_tests` and `transfer_readers_tests`.

mod support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use hostkeeper_coordinator::config::main;
use support::database::Database;
use support::scratch;

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

/// Proves: an export of a database into targets that hold no state exits 0 and writes the
/// desired-state file and the ops file; an export over the files it wrote is refused, exit 1.
///
/// Catches: a refused export whose exit code says it was made, which an operator's script (the
/// restore drill) would go on from, or one that exits 0 having written nothing.
#[test]
fn an_export_exits_0_and_a_refused_export_exits_1() {
    let dir = scratch("transfer-exit");
    let state = dir.path().join("desired.json");
    let database = Database::new();
    let dsn = dir.path().join("database.dsn");
    dsn_file(&dsn, &database);
    let export = [
        "export",
        "--database-dsn-file",
        text(&dsn),
        "--state-file",
        text(&state),
        "--into-empty",
    ];
    assert_eq!(main(args(&export)), ExitCode::SUCCESS);
    assert!(state.exists(), "the desired-state file");
    assert!(dir.path().join("desired.json.ops").exists(), "the ops file");
    assert_eq!(
        main(args(&export)),
        ExitCode::from(1),
        "the targets hold state"
    );
}
