//! `serve` with the MDM policy gate on the database of `--database-dsn-file`
//! (`docs/design/coordinator-storage.md`, section 8): the gate's flags take the database in place
//! of `--enrollment-dir`, the gate keeps its device records in the table `mdm_devices` and reads
//! them back at the next start, a row the gate's reader refuses stops the start naming it, and no
//! file of the file store is made.
//!
//! Each test makes a database of its own on the server `HOSTKEEPER_TEST_POSTGRES_URL` names, and
//! fails, saying how to start one, when it is not set; it never skips.

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use hostkeeper_coordinator::config::{Parts, StartError};
use support::database::{Database, client};
use support::serve::{Files, build_given, exit_code, flags_of, refusal, without};

fn path(files: &Files, file: &str) -> String {
    files.path(file).display().to_string()
}

fn flags(pairs: &[(&str, String)]) -> Vec<OsString> {
    pairs
        .iter()
        .flat_map(|(flag, value)| [OsString::from(flag), OsString::from(value)])
        .collect()
}

/// The arguments of a `serve` on `database` with enrollment (its CA made at start), and the
/// gate's flags when `gate`: the recorded flags without `--state-file`, the connection string in
/// a file of mode 0600, the marker beside it.
fn args(files: &Files, database: &Database, gate: bool) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    files.put("mdm.key", "a-nanohub-api-key-for-the-tests\n", 0o600);
    let mut args = without(&files.args(), "--state-file");
    args.extend(flags(&[
        ("--database-dsn-file", path(files, "database.dsn")),
        ("--halt-marker-file", path(files, "halt.marker")),
        ("--ca-key-file", path(files, "ca.key")),
        ("--server-cert-file", path(files, "server.pem")),
        ("--server-name", "localhost".to_owned()),
    ]));
    args.push("--ca-init".into());
    if gate {
        args.extend(gate_flags(files));
    }
    args
}

/// The gate's three flags: NanoHUB's API on a loopback port nothing listens on (no event of
/// these tests makes the gate call it), its key file, and the webhook's address.
fn gate_flags(files: &Files) -> Vec<OsString> {
    flags(&[
        ("--mdm-api-url", "http://127.0.0.1:9004".to_owned()),
        ("--mdm-api-key-file", path(files, "mdm.key")),
        ("--mdm-webhook-listen", "127.0.0.1:9100".to_owned()),
    ])
}

fn start(args: Vec<OsString>) -> Result<Parts, StartError> {
    build_given(&flags_of(args))
}

/// The bytes of NanoMDM's recorded `Authenticate` event, which the gate's own tests post too.
fn authenticate_event() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-mdm-gate/tests/fixtures/nanohub/Authenticate.2.json");
    std::fs::read(path).expect("the recorded event")
}

/// The enrollment ids of the rows of `mdm_devices`.
fn rows(database: &Database) -> Vec<String> {
    client(Some(database.name()))
        .query(
            "SELECT enrollment_id FROM hostkeeper.mdm_devices ORDER BY enrollment_id",
            &[],
        )
        .expect("the table reads")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// Proves: with `--database-dsn-file` the gate's flags start the gate, whose records are the rows
/// of `mdm_devices`: an event the webhook takes is a row when the gate answers, the next start's
/// gate reads it back as the record the first one kept, and nothing is made in the directory of
/// the flags' files (no `mdm-devices`, no enrollment directory). Without the gate's flags the
/// database start runs no gate.
///
/// Catches: a gate on the database that keeps its records in memory only, or in files beside the
/// database, so a restart forgets every device; and a database start that runs no gate though
/// its flags are given.
#[test]
fn the_gate_on_the_database_keeps_its_records_in_mdm_devices() {
    let files = Files::new("mdm-database");
    let database = Database::new();
    let parts = start(args(&files, &database, true)).expect("the start");
    let mdm = parts.coordinator.mdm().expect("the gate").gate.clone();
    assert_eq!(rows(&database), Vec::<String>::new());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime
        .block_on(mdm.webhook(&authenticate_event()))
        .expect("the event is taken");
    let kept = mdm.records();
    assert_eq!(kept.len(), 1);
    assert_eq!(rows(&database), vec![kept[0].enrollment_id.to_string()]);
    drop(mdm);
    drop(parts);
    database.lock_is_free();

    let parts = start(args(&files, &database, true)).expect("the start again");
    assert_eq!(
        parts.coordinator.mdm().expect("the gate").gate.records(),
        kept
    );
    for made in ["mdm-devices", "enrollment"] {
        assert!(!files.path(made).exists(), "{made}");
    }
    drop(parts);
    database.lock_is_free();

    let parts = start(args(&files, &database, false)).expect("the start without the gate");
    assert!(parts.coordinator.mdm().is_none());
    assert!(parts.coordinator.enrollment().is_some());
}

/// Proves: a row of `mdm_devices` the gate's reader refuses stops the start with the gate's own
/// reason, naming the row (`database:mdm_devices/<enrollment id>`) and not the connection string;
/// a start without the gate does not read the table, and one with it starts again once the row is
/// gone.
///
/// Catches: a gate on the database that reads a torn row as an enrolled device, or skips it and
/// forgets the device.
#[test]
fn a_device_row_the_gate_cannot_read_stops_the_start() {
    let files = Files::new("mdm-database-torn");
    let database = Database::new();
    drop(start(args(&files, &database, true)).expect("the start"));
    database.lock_is_free();
    let mut admin = client(Some(database.name()));
    admin
        .batch_execute("INSERT INTO hostkeeper.mdm_devices VALUES ('AAAA-1', '\\xffff')")
        .expect("a torn row");
    match start(args(&files, &database, true)) {
        Err(StartError::Mdm(why)) => assert_eq!(
            why.to_string(),
            "the MDM device records: database:mdm_devices/AAAA-1: the file is not a record; the \
             gate does not guess what a device's record said, so fix or remove the file to start"
        ),
        Err(other) => panic!("refused otherwise: {other}"),
        Ok(_) => panic!("started over a torn row"),
    }
    database.lock_is_free();
    // The start without the gate does not read the gate's records.
    drop(start(args(&files, &database, false)).expect("the start without the gate"));
    database.lock_is_free();
    admin
        .batch_execute("DELETE FROM hostkeeper.mdm_devices")
        .expect("the row removed");
    drop(start(args(&files, &database, true)).expect("the start once the row is gone"));
}

/// Proves: on the database the gate's flags need enrollment, `--ca-key-file`, as with the file
/// store: without it they are a usage error (exit 2) rather than a gate that silently does not
/// run; and `--enrollment-dir` beside the database is refused naming it, with the gate's flags as
/// without them (exit 1).
///
/// Catches: a database start that takes the gate's flags and runs no gate, and a gate whose
/// records would be split between a directory and the database.
#[test]
fn on_the_database_the_gate_needs_enrollment_and_refuses_the_directory() {
    let files = Files::new("mdm-database-flags");
    files.put("database.dsn", "host=127.0.0.1 dbname=unused", 0o600);
    files.put("mdm.key", "a-nanohub-api-key-for-the-tests\n", 0o600);
    let mut bare = without(&files.args(), "--state-file");
    bare.extend(flags(&[
        ("--database-dsn-file", path(&files, "database.dsn")),
        ("--halt-marker-file", path(&files, "halt.marker")),
    ]));
    bare.extend(gate_flags(&files));
    assert_eq!(exit_code(bare.clone()), ExitCode::from(2));

    let mut beside = bare;
    beside.extend(flags(&[
        ("--enrollment-dir", path(&files, "enrollment")),
        ("--ca-key-file", path(&files, "ca.key")),
        ("--server-cert-file", path(&files, "server.pem")),
        ("--server-name", "localhost".to_owned()),
    ]));
    let why = refusal(beside.clone());
    assert!(
        matches!(&why, StartError::DatabaseAndFileFlag("--enrollment-dir")),
        "{why}"
    );
    assert_eq!(exit_code(beside), ExitCode::from(1));
    assert!(!files.path("enrollment").exists());
}
