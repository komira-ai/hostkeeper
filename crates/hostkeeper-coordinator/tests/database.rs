//! `serve`'s start over the database of `--database-dsn-file` (`docs/design/coordinator-storage.md`,
//! sections 4.3 and 8): the file-store flags the database refuses beside it, the flags that
//! go together, a start that holds the database's lock, and the marker of changes not yet
//! committed, settled against the audit table.
//!
//! Each test that needs the server makes a database of its own on the server
//! `HOSTKEEPER_TEST_POSTGRES_URL` names, and fails, saying how to start one, when it is not set;
//! it never skips.

mod support;

use std::ffi::OsString;
use std::process::ExitCode;

use hostkeeper_coordinator::config::{Parts, StartError};
use support::OPERATOR;
use support::database::Database;
use support::serve::{Files, build_given, exit_code, flags_of, refusal, without};

/// The arguments of a `serve` on `database`: the recorded flags without `--state-file`, the
/// connection string in a file of mode 0600 and the marker beside it.
fn database_args(files: &Files, database: &Database) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    let mut args = without(&files.args(), "--state-file");
    args.extend(flags(&[
        ("--database-dsn-file", &path(files, "database.dsn")),
        ("--halt-marker-file", &path(files, "halt.marker")),
    ]));
    args
}

fn path(files: &Files, file: &str) -> String {
    files.path(file).display().to_string()
}

fn flags(pairs: &[(&str, &str)]) -> Vec<OsString> {
    pairs
        .iter()
        .flat_map(|(flag, value)| [OsString::from(flag), OsString::from(value)])
        .collect()
}

/// Proves: `--database-dsn-file` is refused beside each flag of the file store (`--state-file`,
/// `--ops-file`, `--enrollment-dir`, `--audit-max-bytes`, `--audit-retained-segments`), with the
/// exact text that names the flag and exit 1; either of `--database-dsn-file` and
/// `--halt-marker-file` without the other, and no store flag at all, are usage errors (exit 2);
/// and `--ca-key-file` with neither `--enrollment-dir` nor the database is refused for want of a
/// store for the enrollment records.
///
/// Catches: a file flag silently ignored beside the database (a coordinator that keeps some
/// state in files and some in the database), a refusal that does not name the flag, or a flag
/// that needs its partner taken alone.
#[test]
fn the_database_flag_refuses_the_file_flags() {
    let files = Files::new("database-flags");
    files.put("database.dsn", "host=127.0.0.1 dbname=unused", 0o600);
    let base = {
        let mut args = without(&files.args(), "--state-file");
        args.extend(flags(&[
            ("--database-dsn-file", &path(&files, "database.dsn")),
            ("--halt-marker-file", &path(&files, "halt.marker")),
        ]));
        args
    };
    let enrollment = [
        ("--enrollment-dir", path(&files, "enrollment")),
        ("--ca-key-file", path(&files, "ca.key")),
        ("--server-cert-file", path(&files, "server.pem")),
        ("--server-name", "localhost".to_owned()),
    ];
    let beside: [(&str, Vec<(&str, String)>); 5] = [
        (
            "--state-file",
            vec![("--state-file", path(&files, "desired.json"))],
        ),
        (
            "--ops-file",
            vec![("--ops-file", path(&files, "desired.json.ops"))],
        ),
        ("--enrollment-dir", enrollment.to_vec()),
        (
            "--audit-max-bytes",
            vec![("--audit-max-bytes", "1048576".to_owned())],
        ),
        (
            "--audit-retained-segments",
            vec![("--audit-retained-segments", "3".to_owned())],
        ),
    ];
    for (flag, given) in beside {
        let mut args = base.clone();
        for (name, value) in &given {
            args.extend(flags(&[(name, value)]));
        }
        let why = refusal(args.clone());
        assert!(
            matches!(&why, StartError::DatabaseAndFileFlag(named) if *named == flag),
            "{flag}: {why}"
        );
        assert_eq!(
            why.to_string(),
            format!(
                "--database-dsn-file and {flag} are both given: {flag} names a file of the file \
                 store, and the database holds what it held"
            )
        );
        assert_eq!(exit_code(args), ExitCode::from(1), "{flag}");
    }

    for lacking in ["--halt-marker-file", "--database-dsn-file"] {
        assert_eq!(
            exit_code(without(&base, lacking)),
            ExitCode::from(2),
            "without {lacking}"
        );
    }
    assert_eq!(
        exit_code(without(&files.args(), "--state-file")),
        ExitCode::from(2),
        "neither --state-file nor --database-dsn-file"
    );

    let mut keyed = files.args();
    keyed.extend(flags(&[
        ("--ca-key-file", &path(&files, "ca.key")),
        ("--server-cert-file", &path(&files, "server.pem")),
        ("--server-name", "localhost"),
    ]));
    let why = refusal(keyed);
    assert!(matches!(why, StartError::NoEnrollmentStore), "{why}");
}

/// Builds the serve that `args` name over the database, as a start does.
fn start(args: Vec<OsString>) -> Parts {
    build_given(&flags_of(args)).expect("the start")
}

/// Proves: a `serve` with the database's connection string in a file of mode 0600 starts on a
/// database that has nothing in it: it makes the schema, takes the coordinator's lock (a second
/// start on the same database is refused while the first runs, and is not once it has stopped),
/// and a change an operator makes is held by the audit table.
///
/// Catches: a start that does not reach the database, one that leaves it unlocked so a second
/// coordinator could write beside the first, or an operator's change that never reaches the
/// table.
#[test]
fn a_serve_starts_on_the_database_and_holds_its_lock() {
    let files = Files::new("database-start");
    let database = Database::new();
    let args = database_args(&files, &database);

    let parts = start(args.clone());
    let why = refusal(args.clone());
    assert!(
        matches!(&why, StartError::Database(_)),
        "a second coordinator is refused: {why}"
    );
    let done = parts
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"bad disk"}"#,
        )
        .expect("an operator's change");
    assert_eq!(
        database.audit_numbers(),
        [i64::try_from(done.seq).expect("a number")]
    );

    drop(parts);
    database.lock_is_free();
    let again = start(args);
    drop(again);
}

/// Proves: a start on the database refuses while the marker lists a change the audit table does
/// not hold, naming that change and leaving the marker, and a marker that cannot be read is
/// refused without touching the database.
///
/// Catches: a start that goes on over changes that were in memory and never reached the database
/// (their loss unnoticed), one that removes the marker the operator needs, or a refusal that
/// does not name the change.
#[test]
fn serve_refuses_to_start_on_the_database_while_a_marker_lists_a_change_it_does_not_hold() {
    let files = Files::new("database-marker-refused");
    let database = Database::new();
    let args = database_args(&files, &database);
    let marker = files.path("halt.marker");
    files.put("halt.marker", "{\"unwritten\":[7]}\n", 0o600);

    let why = refusal(args.clone());
    assert!(
        matches!(
            &why,
            StartError::ChangesUnwritten { path, numbers } if *path == marker && *numbers == [7]
        ),
        "{why}"
    );
    assert!(marker.exists(), "the marker stays for the operator");

    std::fs::remove_file(&marker).expect("removed by the operator");
    std::fs::create_dir(&marker).expect("a marker that cannot be read");
    database.lock_is_free();
    let why = refusal(args);
    assert!(
        matches!(&why, StartError::HaltWriteUnfinished { path } if *path == marker),
        "{why}"
    );
}

/// Proves: a marker written at an epoch another start has raised since is refused naming every
/// number it lists, though the audit table holds rows of those numbers; one written at the epoch
/// the database is still at is settled by its rows and removed.
///
/// Catches: the changes of a coordinator that did not lead last taken as landed because another
/// coordinator's audit rows carry the same numbers.
#[test]
fn a_marker_of_an_epoch_another_coordinator_led_after_refuses_the_start() {
    let files = Files::new("database-marker-epoch");
    let database = Database::new();
    let args = database_args(&files, &database);
    let marker = files.path("halt.marker");

    // Epoch 1: a change that lands.
    let parts = start(args.clone());
    let done = parts
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"bad disk"}"#,
        )
        .expect("an operator's change");
    drop(parts);
    database.lock_is_free();
    // Epoch 2: another start leads the database, and stops.
    drop(start(args.clone()));
    database.lock_is_free();

    files.put(
        "halt.marker",
        &format!("{{\"unwritten\":[{}],\"epoch\":1}}\n", done.seq),
        0o600,
    );
    let why = refusal(args.clone());
    assert!(
        matches!(&why, StartError::ChangesUnwritten { path, numbers } if *path == marker && *numbers == [done.seq]),
        "{why}"
    );
    assert!(marker.exists(), "the marker stays for the operator");
    database.lock_is_free();

    // The refused start raised the epoch to 3; a marker of it is settled by its rows.
    files.put(
        "halt.marker",
        &format!("{{\"unwritten\":[{}],\"epoch\":3}}\n", done.seq),
        0o600,
    );
    drop(start(args));
    assert!(
        !marker.exists(),
        "the marker of the epoch the database held is removed"
    );
}

/// Proves: a marker whose every change the audit table holds is removed at the start, which goes
/// on, and the changes stay in the database.
///
/// Catches: a marker whose changes all landed left to refuse the next start, or removed with a
/// change it listed missing.
#[test]
fn a_marker_whose_changes_all_landed_is_removed_at_start_on_the_database() {
    let files = Files::new("database-marker-landed");
    let database = Database::new();
    let args = database_args(&files, &database);
    let marker = files.path("halt.marker");

    let parts = start(args.clone());
    let done = parts
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"bad disk"}"#,
        )
        .expect("an operator's change");
    drop(parts);
    database.lock_is_free();
    files.put(
        "halt.marker",
        &format!("{{\"unwritten\":[{}]}}\n", done.seq),
        0o600,
    );

    let parts = start(args);
    assert!(!marker.exists(), "the marker is removed");
    drop(parts);
    assert_eq!(
        database.audit_numbers(),
        [i64::try_from(done.seq).expect("a number")]
    );
}
