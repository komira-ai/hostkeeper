//! `export` against a database of each test's own: what it writes back, in which form, what it
//! counts, and the targets and databases it refuses. The helpers are `transfer_tests`'.

use std::fs;
use std::path::PathBuf;

use super::TransferError;
use super::export;
use super::tests::{
    DESIRED_SINGLE, Store, audit, clock, count, export_flags, files_loaded, loaded, seeded, stored,
};
use crate::backup::BackupError;
use crate::enrollment::tests::support::{HostKey, Rig as Enrolling, enrolled};
use crate::lock::FileLock;
use crate::store::postgres::DatabaseError;
use crate::store::postgres::tests::TestDatabase;
use crate::store::postgres::tests::seed::seed;
use crate::store::postgres::transfer::STATE_TABLES;
use crate::store::{Backend, Files, Loaded};

/// Proves: an export writes into targets that hold no state, and refuses any that do (a state
/// file, an audit segment, an enrollment directory with anything in it), writing nothing then;
/// and is refused while a coordinator holds the database's lock.
///
/// Catches: an export that writes over a file of a file store, or reads a database a coordinator
/// is changing.
#[test]
fn export_refuses_targets_that_hold_state() {
    let database = TestDatabase::new();
    seeded(&Store::recorded("transfer-export-from"), &database);
    for name in [
        "desired.json",
        "desired.json.ops.audit.3",
        "enrollment/stray",
    ] {
        let mut into = Store::new("transfer-export-full");
        into.paths.enrollment_dir = Some(into.dir.path().join("enrollment"));
        into.put(name, b"held");
        let before = into.files();
        match export(&export_flags(&database, &into), &FileLock, &clock()) {
            Err(TransferError::Files(BackupError::NotEmpty { path })) => {
                assert_eq!(path, into.dir.path().join(name));
            }
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(into.files(), before, "{name}");
    }
    let (serving, _) = database.open("coordinator").expect("the store opens");
    let into = Store::new("transfer-export-locked");
    assert!(matches!(
        export(&export_flags(&database, &into), &FileLock, &clock()),
        Err(TransferError::Database(DatabaseError::Locked))
    ));
    serving.stop();
    assert!(into.files().is_empty());
}

/// Proves: an export of a database that holds the enrollment store is refused without
/// `--enrollment-dir`, and writes nothing.
///
/// Catches: an export that leaves the enrolled hosts behind, so every host would enroll again on
/// the file store.
#[test]
fn export_refuses_to_leave_the_enrollment_store_behind() {
    let enrolling = Enrolling::new("transfer-export-enrolled");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    let mut store = Store::recorded("transfer-export-enrolled-files");
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    let database = TestDatabase::new();
    seeded(&store, &database);
    let into = Store::new("transfer-export-no-dir");
    assert!(matches!(
        export(&export_flags(&database, &into), &FileLock, &clock()),
        Err(TransferError::EnrollmentDirNeeded)
    ));
    assert!(into.files().is_empty());
}

/// Notes that the daemon of `host` runs `web` 1.0 from `deb`, in `enrolling`'s store.
fn note_running(enrolling: &Enrolling, host: &str) {
    let running = hostkeeper_enrollment::enroll::HostRunning {
        agent_name: "hostkeeper".to_owned(),
        agent_version: "0.2.0".to_owned(),
        services: vec![hostkeeper_enrollment::enroll::RunningService {
            service: "web".to_owned(),
            version: "1.0".to_owned(),
            installer: "deb".to_owned(),
        }],
    };
    enrolling
        .enrollment
        .store()
        .note_running(
            &hostkeeper_enrollment::names::HostId::parse(host).expect("a host id"),
            &running,
            crate::enrollment::tests::support::NOW + 60,
        )
        .expect("noted");
}

/// Proves: the files an export writes are the state the database holds: read back by the readers
/// a start uses and written into a second database, they give back every row of every table, the
/// audit history with its numbers and the enrollment store (a host's record of what it runs among
/// it); and a pruned history is written as a sealed segment past the first, so the file store says
/// the same time of its oldest line.
///
/// Catches: a part an export leaves out or writes in a form the file store does not read, or an
/// export that turns a pruned history into a whole one.
#[test]
fn an_export_read_back_gives_back_every_row() {
    let enrolling = Enrolling::new("transfer-round-ca");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    note_running(&enrolling, "web-1");
    let mut store = Store::recorded("transfer-round");
    store.put("desired.json.jti", b"live 99999999999\n");
    audit(&store, 9, 1_850_000_000, 2);
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    let first = TestDatabase::new();
    seeded(&store, &first);
    assert_eq!(count(&first, "enrollment_host_running"), 1);
    assert_eq!(count(&first, "spent_tokens"), 1);

    let mut out = Store::new("transfer-round-out");
    out.paths.enrollment_dir = Some(out.dir.path().join("enrollment"));
    export(&export_flags(&first, &out), &FileLock, &clock()).expect("exported");
    let active = crate::audit_file::path_of(&out.ops());
    assert!(!active.exists(), "the pruned history is sealed");
    assert!(crate::audit_file::segment_path(&active, 2).exists());

    let second = TestDatabase::new();
    seed(&second, &stored(&out, None));
    for table in STATE_TABLES {
        let rows = |database: &TestDatabase| {
            let mut rows = database.rows(&format!("SELECT * FROM {table}"));
            rows.sort();
            rows
        };
        assert_eq!(rows(&second), rows(&first), "{table}");
    }
    let files = Files::default()
        .with_ops(&out.ops(), crate::audit_file::DEFAULT_ROTATION)
        .expect("the exported ops files");
    let pruned = files.audit_pruned_before().expect("read").expect("pruned");
    assert_eq!(
        first.rows("SELECT audit_pruned_before FROM imports"),
        [[Some(pruned.to_string())]]
    );
}

/// Proves: the desired-state file an export writes is in the single-state form exactly when the
/// legacy service is the only service, and in the map form otherwise (another service beside it,
/// or no legacy service).
///
/// Catches: an export that writes the single form for a farm of several services, which the file
/// store would read as one, or the map form for a lone legacy service.
#[test]
fn export_writes_the_single_state_form_only_for_a_lone_legacy_service() {
    let single = Store::new("transfer-form-single");
    single.put("desired.json", DESIRED_SINGLE);
    let lone = TestDatabase::new();
    seed(&lone, &stored(&single, Some("kbf")));
    let several = TestDatabase::new();
    seeded(&Store::recorded("transfer-form-map"), &several);
    let services = |database: &TestDatabase| count(database, "desired");
    assert_eq!(services(&lone), 1);
    assert!(services(&several) > 1);

    for (database, legacy, single_form) in [
        (&lone, Some("kbf"), true),
        (&lone, None, false),
        (&lone, Some("web"), false),
        (&several, Some("kbf"), false),
    ] {
        let into = Store::new("transfer-form-out");
        let mut flags = export_flags(database, &into);
        flags.legacy_service = legacy.map(str::to_owned);
        export(&flags, &FileLock, &clock()).expect("exported");
        let text: serde_json::Value =
            serde_json::from_slice(&fs::read(&into.paths.state_file).expect("the state file"))
                .expect("json");
        assert_eq!(
            text.get("services").is_none(),
            single_form,
            "{legacy:?}: {text}"
        );
    }
}

/// Proves: an export prints the rows of every state table, each as the database counts it, and
/// the lines of the audit file it wrote.
///
/// Catches: a count the drill would compare that is not the table's, or a table left out of it.
#[test]
fn export_prints_the_count_of_each_table() {
    let mut store = Store::recorded("transfer-counts");
    audit(&store, 3, 1_850_000_000, 8);
    let enrolling = Enrolling::new("transfer-counts-ca");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    let database = TestDatabase::new();
    seeded(&store, &database);
    let mut into = Store::new("transfer-counts-out");
    into.paths.enrollment_dir = Some(into.dir.path().join("enrollment"));
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    for table in STATE_TABLES {
        let line = format!("  {table}: {} rows\n", count(&database, table));
        assert!(report.contains(&line), "{line}{report}");
    }
    assert!(
        report.contains(&format!(
            "the audit file holds {} lines\n",
            count(&database, "audit")
        )),
        "{report}"
    );
}

/// Proves: a database that a coordinator started on and that holds nothing, so with no audit
/// row and no record, exports as a file store that holds nothing: the desired-state file and the
/// ops file of no state, no audit file, no enrollment record; every table counted at 0.
///
/// Catches: an export that needs an audit history or an import record to be there.
#[test]
fn an_empty_database_exports_an_empty_file_store() {
    let database = TestDatabase::new();
    let (opened, _) = database.open("coordinator").expect("the store opens");
    opened.stop();
    let mut into = Store::new("transfer-export-empty");
    into.paths.enrollment_dir = Some(into.dir.path().join("enrollment"));
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    let written: Vec<PathBuf> = into.files().into_keys().collect();
    assert_eq!(written, [into.paths.state_file.clone(), into.ops()]);
    assert_eq!(files_loaded(&into, None), Loaded::default());
    for table in STATE_TABLES {
        assert!(report.contains(&format!("  {table}: 0 rows\n")), "{report}");
    }
}

/// Proves: the verdicts a file store holds (the verdict each host was last acknowledged, and the
/// ones counted toward an automatic rollback) in a database are exported back as they were.
///
/// Catches: an export that leaves the verdicts behind, so a host would be
/// acknowledged a verdict again, or a rollback counted twice.
#[test]
fn the_verdicts_move_both_ways() {
    let store = Store::recorded("transfer-verdicts");
    let report = r#"{"target":"2.0","outcome":"failed","phase":"halt","reason":"disk_tainted"}"#;
    store.put(
        "desired.json.verdicts",
        format!(r#"{{"services":{{"kbf":{{"standing":{{"s1":{report}}}}}}}}}"#).as_bytes(),
    );
    let files = Files::default()
        .with_verdicts(&crate::verdict_file::verdicts_path(&store.paths.state_file))
        .load_verdicts()
        .expect("the verdicts");
    assert_ne!(files, crate::verdict_file::Held::default());
    let database = TestDatabase::new();
    seeded(&store, &database);
    assert_eq!(count(&database, "verdict_standing"), 1);
    assert_eq!(loaded(&database).verdicts, files);

    let into = Store::new("transfer-verdicts-out");
    export(&export_flags(&database, &into), &FileLock, &clock()).expect("exported");
    let back = Files::default()
        .with_verdicts(&crate::verdict_file::verdicts_path(&into.paths.state_file))
        .load_verdicts()
        .expect("the exported verdicts");
    assert_eq!(back, files);
}
