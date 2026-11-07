//! `import` and `export` against a database of each test's own: what moves, what the move
//! refuses, and that neither leaves a file or a table changed when it refuses.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::FileRecords;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_testkit::Scratch;

use super::{Export, Import, TransferError, export, import};
use crate::audit_file::{AuditLine, AuditLog, Detail, Rotation, UnixClock};
use crate::backup::{BackupError, StatePaths};
use crate::config::StartError;
use crate::enrollment::tests::support::{
    FixedClock, HostKey, NOW, Rig as Enrolling, enrolled, host_id,
};
use crate::enrollment::{StoreAt, os_random, setup};
use crate::lock::{FileLock, StateLock, lock_path};
use crate::ops::OpsStore;
use crate::store::Backend;
use crate::store::postgres::DatabaseError;
use crate::store::postgres::PgRecords;
use crate::store::postgres::tests::{AT, TestDatabase};
use crate::store::postgres::transfer::{MoveError, STATE_TABLES};
use crate::store::{Files, Loaded};
use crate::testutil::scratch;

const DESIRED_MAP: &[u8] = include_bytes!("../tests/fixtures/recorded/desired-map.json");
const DESIRED_SINGLE: &[u8] = include_bytes!("../tests/fixtures/recorded/desired-single.json");
const OPS: &[u8] = include_bytes!("../tests/fixtures/recorded/ops.json");
const HALTS: &[u8] = include_bytes!("../tests/fixtures/recorded/ops.json.halts");
const OPS_SCHEMA_2: &[u8] = include_bytes!("../tests/fixtures/recorded/ops-schema2.json");

/// The clock of every import and export: [`AT`].
fn clock() -> UnixClock {
    Arc::new(|| AT)
}

/// A file store in a directory of its own: `desired.json`, the ops file beside it by default,
/// and with `enrollment`, the enrollment store of a CA started in it.
struct Store {
    dir: Scratch,
    /// Where the connection-string files of the tests are, beside the store's own directory.
    secrets: Scratch,
    paths: StatePaths,
}

impl Store {
    fn new(name: &str) -> Store {
        let dir = scratch(name);
        let paths = StatePaths {
            state_file: dir.path().join("desired.json"),
            ops_file: None,
            enrollment_dir: None,
        };
        Store {
            dir,
            secrets: scratch(&format!("{name}-secrets")),
            paths,
        }
    }

    /// A store holding the recorded files of the release before: the desired state in its map
    /// form, the ops file and its halts file.
    fn recorded(name: &str) -> Store {
        let store = Store::new(name);
        store.put("desired.json", DESIRED_MAP);
        store.put("desired.json.ops", OPS);
        store.put("desired.json.ops.halts", HALTS);
        store
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        let path = self.dir.path().join(name);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        fs::write(path, bytes).expect("write a file of the store");
    }

    fn ops(&self) -> PathBuf {
        self.paths.ops_file()
    }

    /// Every file under the store's directory, with its bytes; the lock file, which holds no
    /// state and stays once made, left out.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(self.dir.path(), &mut out);
        out.remove(&lock_path(&self.paths.state_file));
        out
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).expect("list") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.insert(path.clone(), fs::read(&path).expect("read"));
        }
    }
}

/// The connection-string file of `database`, owner-only, in `dir`.
fn dsn_file(dir: &Path, database: &TestDatabase) -> PathBuf {
    let path = dir.join(format!("{}.dsn", database.name()));
    fs::write(&path, database.dsn_text()).expect("write the connection string");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("owner-only");
    path
}

/// The import of `store` into `database`, with `apply`.
fn import_flags(store: &Store, database: &TestDatabase, apply: bool) -> Import {
    Import {
        paths: store.paths.clone(),
        legacy_service: None,
        database_dsn_file: dsn_file(store.secrets.path(), database),
        apply,
    }
}

fn imported(store: &Store, database: &TestDatabase) -> Result<String, TransferError> {
    import(&import_flags(store, database, true), &FileLock, &clock()).map(|r| r.to_string())
}

fn export_flags(database: &TestDatabase, into: &Store) -> Export {
    let file = import_flags(into, database, false).database_dsn_file;
    Export {
        database_dsn_file: file,
        paths: into.paths.clone(),
        legacy_service: None,
        into_empty: true,
    }
}

/// The rows of `table` in `database`.
fn count(database: &TestDatabase, table: &str) -> usize {
    database.rows(&format!("SELECT 1 FROM {table}")).len()
}

/// Whether any migration ran on `database`.
fn migrated(database: &TestDatabase) -> bool {
    database.rows("SELECT to_regclass('hostkeeper.schema_version')") != [[None]]
}

/// What `database` holds, as a start loads it.
fn loaded(database: &TestDatabase) -> Loaded {
    let (store, loaded) = database.open("coordinator").expect("the store opens");
    store.stop();
    loaded
}

/// What the file store `store` holds, as a start loads it.
fn files_loaded(store: &Store, legacy: Option<&str>) -> Loaded {
    Files::default()
        .with_desired(&store.paths.state_file, legacy)
        .with_ops(&store.ops(), crate::audit_file::DEFAULT_ROTATION)
        .expect("the ops files")
        .load()
        .expect("the files load")
}

/// An audit line of the tests.
fn line(seq: u64, at_unix: u64) -> AuditLine {
    AuditLine {
        seq,
        at_unix,
        by: "alice".to_owned(),
        action: "exclude".to_owned(),
        detail: Detail {
            service: Some("kbf".to_owned()),
            node_id: Some(format!("s{seq}")),
            reason: Some("a reason".to_owned()),
        },
    }
}

/// Appends lines `1..=n` to the audit file of `store`, each `at + seq`, sealed into segments of
/// about two lines, and the oldest beyond `retained` deleted.
fn audit(store: &Store, n: u64, at: u64, retained: usize) -> Vec<AuditLine> {
    let log = AuditLog::open(
        crate::audit_file::path_of(&store.ops()),
        Rotation {
            max_bytes: std::num::NonZeroU64::new(300).expect("non-zero"),
            retained: std::num::NonZeroUsize::new(retained).expect("non-zero"),
        },
    )
    .expect("the audit file");
    let lines: Vec<AuditLine> = (1..=n).map(|seq| line(seq, at + seq)).collect();
    for line in &lines {
        log.append(line).expect("append");
    }
    let mut kept = Vec::new();
    log.each(&mut |line| kept.push(line)).expect("read back");
    kept
}

/// Proves: the recorded files of the release before (its desired state, its ops file and its
/// halts file) are imported, and a start on the database loads what a start on the files loads:
/// the same desired state, and the same operations state but for the ring, which the database
/// keeps as its audit rows. The ring's entries have no line in an audit file there, so they are
/// not copied, the report says so, and the import's own row is numbered past them.
///
/// Catches: a file of the release before that the import cannot read, a table it leaves out, or
/// an import's row that takes a number the ring already gave.
#[test]
fn import_reads_every_recorded_file_of_the_release_before() {
    let store = Store::recorded("transfer-recorded");
    let database = TestDatabase::new();
    let report = imported(&store, &database).expect("imported");
    let files = files_loaded(&store, None);
    let held = loaded(&database);
    assert_eq!(held.desired, files.desired);
    let ring = files.ops.audit.clone();
    assert!(!ring.is_empty(), "the recorded ops file holds a ring");
    let without_ring = |mut ops: crate::ops::OpsState| {
        ops.audit.clear();
        ops
    };
    assert_eq!(without_ring(held.ops.clone()), without_ring(files.ops));
    let newest = ring.iter().map(|entry| entry.seq).max().expect("a newest");
    assert_eq!(
        database.rows("SELECT seq, by_name, action FROM audit"),
        [[
            Some((newest + 1).to_string()),
            Some("coordinator".to_owned()),
            Some("state_imported".to_owned())
        ]]
    );
    assert!(
        report.contains(&format!(
            "{} entries of the ops file's audit ring are older than its audit file",
            ring.len()
        )),
        "{report}"
    );
    assert!(
        count(&database, "halts") > 0,
        "the halts file's halt is imported"
    );
}

/// Proves: a desired-state file in the single-state form is imported as the state of
/// `--legacy-service`, and refused without it, as `serve` refuses it.
///
/// Catches: an import that reads the single form as no service, or as a service of another name.
#[test]
fn import_reads_a_single_state_desired_file_as_the_legacy_service() {
    let store = Store::new("transfer-single");
    store.put("desired.json", DESIRED_SINGLE);
    let database = TestDatabase::new();
    let mut flags = import_flags(&store, &database, true);
    match import(&flags, &FileLock, &clock()) {
        Err(TransferError::Refused(StartError::Desired(why))) => {
            assert!(
                why.to_string()
                    .ends_with("the file holds one desired state and no legacy service is set"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(!migrated(&database), "nothing was migrated");
    flags.legacy_service = Some("kbf".to_owned());
    import(&flags, &FileLock, &clock()).expect("imported");
    let held = loaded(&database);
    assert_eq!(
        held.desired
            .desired
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["kbf"]
    );
    assert_eq!(held.desired, files_loaded(&store, Some("kbf")).desired);
}

/// Proves: the whole audit history is imported, every sealed segment and the active file, each
/// line with its own number and time, then the import's row by `coordinator` numbered one past
/// the newest; and the time of the oldest line, when older ones were deleted, is the import's
/// record of where the history begins.
///
/// Catches: an import that copies the active file alone, renumbers the lines, takes the time of
/// the import for them, or forgets that the history was pruned.
#[test]
fn import_copies_the_whole_audit_history_with_its_numbers() {
    let store = Store::new("transfer-audit");
    let lines = audit(&store, 9, 1_850_000_000, 2);
    assert!(lines.len() < 9, "the oldest segments were deleted");
    let database = TestDatabase::new();
    imported(&store, &database).expect("imported");
    let rows = database.rows("SELECT seq, at_unix, by_name, node_id FROM audit ORDER BY seq");
    let mut want: Vec<Vec<Option<String>>> = lines
        .iter()
        .map(|line| {
            vec![
                Some(line.seq.to_string()),
                Some(line.at_unix.to_string()),
                Some(line.by.clone()),
                line.detail.node_id.clone(),
            ]
        })
        .collect();
    want.push(vec![
        Some("10".to_owned()),
        Some(AT.to_string()),
        Some("coordinator".to_owned()),
        None,
    ]);
    assert_eq!(rows, want);
    assert_eq!(
        database.rows("SELECT audit_pruned_before, at_unix FROM imports"),
        [[Some(lines[0].at_unix.to_string()), Some(AT.to_string())]]
    );
    let manifest = database.rows("SELECT manifest->'files'->0->>'name' FROM imports");
    assert_eq!(manifest, [[Some("ops-audit".to_owned())]]);

    // A history whose first segment is still there begins with its first line.
    let whole = Store::new("transfer-audit-whole");
    audit(&whole, 3, 1_850_000_000, 8);
    let database = TestDatabase::new();
    imported(&whole, &database).expect("imported");
    assert_eq!(
        database.rows("SELECT audit_pruned_before FROM imports"),
        [[None]]
    );
}

/// Proves: an import into a database that holds state is refused, naming the table, and writes
/// nothing; so is one into a database whose serial counter was moved.
///
/// Catches: a second import over a first, which would mix two histories in one database.
#[test]
fn import_refuses_a_database_that_holds_state() {
    let store = Store::recorded("transfer-holds");
    for (table, sql) in [
        (
            "spent_tokens",
            "INSERT INTO spent_tokens (jti, until_unix) VALUES ('j', 1)",
        ),
        ("enrollment_serial", "UPDATE enrollment_serial SET next = 9"),
        (
            "imports",
            "INSERT INTO imports (id, at_unix, manifest) VALUES (1, 1, '{}')",
        ),
    ] {
        let database = TestDatabase::new();
        let (opened, _) = database.open("coordinator").expect("the store opens");
        opened.stop();
        database.run(sql).expect("the row is written");
        match imported(&store, &database) {
            Err(TransferError::Database(MoveError::HoldsState { table: named, rows })) => {
                assert_eq!((named, rows), (table, 1));
            }
            other => panic!("{table}: {other:?}"),
        }
        assert_eq!(count(&database, "desired"), 0);
        assert_eq!(count(&database, "audit"), 0);
    }
}

/// Proves: an import is refused while a file coordinator holds the lock of the state files, and
/// while a coordinator holds the database's lock; neither refusal writes to the database.
///
/// Catches: an import of files that a running coordinator is changing, or into a database a
/// coordinator serves on.
#[test]
fn import_refuses_while_a_file_coordinator_holds_the_lock() {
    let store = Store::recorded("transfer-locked");
    let database = TestDatabase::new();
    let held = FileLock
        .acquire(&lock_path(&store.paths.state_file))
        .expect("the lock");
    match imported(&store, &database) {
        Err(TransferError::Files(BackupError::Locked { path })) => {
            assert_eq!(path, lock_path(&store.paths.state_file));
        }
        other => panic!("{other:?}"),
    }
    drop(held);
    assert!(!migrated(&database));

    let (serving, _) = database.open("coordinator").expect("the store opens");
    assert!(matches!(
        imported(&store, &database),
        Err(TransferError::Database(MoveError::Database(
            DatabaseError::Locked
        )))
    ));
    serving.stop();
    assert_eq!(count(&database, "desired"), 0);
}

/// Proves: when what the transaction reads back is not what the files hold, the whole import is
/// rolled back, naming the first table that differs: no row of any table, no audit row and no
/// import's record is left.
///
/// Catches: an import that commits what it did not read back as written, or that leaves part of
/// itself behind when the comparison fails.
#[test]
fn import_rolls_back_when_the_read_back_differs() {
    let store = Store::recorded("transfer-differs");
    audit(&store, 2, 1_850_000_000, 8);
    let enrolling = Enrolling::new("transfer-differs-ca");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    let mut store = store;
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    // Each trigger changes what one table is given as it is written, as a database that does not
    // keep what it was given would.
    for (table, set, part) in [
        ("desired", "NEW.version := '9.9'", "desired"),
        ("audit", "NEW.at_unix := NEW.at_unix + 1", "audit"),
        (
            "enrollment_hosts",
            "NEW.role := 'other'",
            "enrollment_hosts",
        ),
    ] {
        let database = TestDatabase::new();
        let (opened, _) = database.open("coordinator").expect("the store opens");
        opened.stop();
        database
            .run(&format!(
                "CREATE FUNCTION skew() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN {set}; RETURN NEW; END $$; \
                 CREATE TRIGGER skewed BEFORE INSERT ON {table} \
                 FOR EACH ROW EXECUTE FUNCTION skew()"
            ))
            .expect("the trigger");
        match imported(&store, &database) {
            Err(TransferError::Database(MoveError::Differs { part: named })) => {
                assert_eq!(named, part);
            }
            other => panic!("{table}: {other:?}"),
        }
        for table in STATE_TABLES.iter().chain(&["imports"]) {
            assert_eq!(count(&database, table), 0, "{table} after the rollback");
        }
    }
}

/// Proves: an import without `--apply` reads the files and the database and writes nothing: no
/// file changes (a staged halts file is not settled, a file of spent ids with a past id is not
/// rewritten, the lock file is not made), no migration runs on a fresh database, and the report
/// says what each table would hold and whether the database holds state.
///
/// Catches: a dry run that settles what a start settles, takes a lock, or migrates the database.
#[test]
fn import_without_apply_writes_nothing() {
    let store = Store::recorded("transfer-plan");
    store.put(
        "desired.json.ops.halts.next",
        b"{\"ops\": \"no digest of the ops file\"}",
    );
    store.put("desired.json.jti", b"past 1\nlive 99999999999\n");
    let before = store.files();
    let database = TestDatabase::new();
    let report = import(&import_flags(&store, &database, false), &FileLock, &clock())
        .expect("the plan")
        .to_string();
    assert_eq!(store.files(), before);
    assert!(!migrated(&database));
    assert!(
        report.contains("would be imported into the database, nothing written"),
        "{report}"
    );
    assert!(report.contains("  spent_tokens: 1 rows"), "{report}");
    assert!(report.contains("the database holds no state"), "{report}");

    imported(&store, &database).expect("imported");
    let report = import(&import_flags(&store, &database, false), &FileLock, &clock())
        .expect("the plan")
        .to_string();
    assert!(
        report.contains("the database holds state (2 rows in desired): import --apply refuses it"),
        "{report}"
    );
}

/// Proves: a staged halts file that names the ops file's digest (a change committed before its
/// halts file was renamed in) is imported as the halts file a start renames it to; one that does
/// not name it is passed over, as a start removes it.
///
/// Catches: an import that reads the halts file a start would replace, or a staged file a start
/// would remove.
#[test]
fn import_reads_the_halts_file_a_start_would_settle_on() {
    let committed = Store::recorded("transfer-staged");
    let mut staged: serde_json::Value = serde_json::from_slice(HALTS).expect("the halts file");
    staged["services"]["kbf"]["halt"]["report"]["reason"] = "staged".into();
    committed.put(
        "desired.json.ops.halts.next",
        &serde_json::to_vec(&staged).expect("json"),
    );
    let database = TestDatabase::new();
    imported(&committed, &database).expect("imported");
    assert_eq!(
        database.rows("SELECT report->>'reason' FROM halts"),
        [[Some("staged".to_owned())]]
    );

    let discarded = Store::recorded("transfer-discarded");
    staged["ops"] = "0".repeat(64).into();
    discarded.put(
        "desired.json.ops.halts.next",
        &serde_json::to_vec(&staged).expect("json"),
    );
    let database = TestDatabase::new();
    imported(&discarded, &database).expect("imported");
    assert_eq!(
        database.rows("SELECT report->>'reason' FROM halts"),
        [[Some("disk_tainted".to_owned())]]
    );
}

/// Proves: an import refuses what `serve` refuses before it touches the database: the marker of
/// changes not yet written, the marker of a restore that stopped partway, an ops file of a newer
/// schema, a missing enrollment directory, and the MDM gate's device records, which no table
/// keeps yet.
///
/// Catches: an import that moves a state `serve` would not start on, or drops what the files hold.
#[test]
fn import_refuses_what_serve_refuses() {
    let cases: [(&str, &[u8]); 3] = [
        ("desired.json.ops.halts.unfinished", b"1\n"),
        ("desired.json.restore-unfinished", b""),
        ("desired.json.ops", OPS_SCHEMA_2),
    ];
    for (name, bytes) in cases {
        let store = Store::recorded("transfer-refused");
        store.put(name, bytes);
        let database = TestDatabase::new();
        let refused = imported(&store, &database);
        let named = match &refused {
            Err(TransferError::Unsettled { path })
            | Err(TransferError::Refused(StartError::RestoreUnfinished { path })) => path.clone(),
            Err(TransferError::Refused(StartError::Ops(why))) => {
                assert!(why.to_string().contains("schema"), "{why}");
                store.dir.path().join(name)
            }
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(named, store.dir.path().join(name));
        assert!(!migrated(&database), "{name}");
    }

    let mut store = Store::recorded("transfer-no-enrollment");
    store.paths.enrollment_dir = Some(store.dir.path().join("enrollment"));
    let database = TestDatabase::new();
    assert!(matches!(
        imported(&store, &database),
        Err(TransferError::Files(BackupError::NoEnrollmentDir { .. }))
    ));
    store.put("enrollment/mdm-devices/a.pb", b"a device");
    match imported(&store, &database) {
        Err(TransferError::MdmDevices { path }) => {
            assert_eq!(path, store.dir.path().join("enrollment/mdm-devices"));
        }
        other => panic!("{other:?}"),
    }
    store.put("enrollment/hosts/web-1.pb", b"no record");
    fs::remove_dir_all(store.dir.path().join("enrollment/mdm-devices")).expect("remove");
    assert!(matches!(
        imported(&store, &database),
        Err(TransferError::Records(_))
    ));
    assert!(!migrated(&database));
}

/// Proves: the enrollment store moves whole: a host enrolled on the files (its request, its
/// certificate, its host row, the serial counter) and a deny row are in the database after the
/// import as the files held them, and a store started on the database knows the host.
///
/// Catches: a record kind the import leaves out, or a serial counter that starts again from the
/// first serial, so a certificate would be issued twice with one serial.
#[test]
fn import_moves_the_enrollment_store_whole() {
    let enrolling = Enrolling::new("transfer-enrolled");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    enrolling
        .enrollment
        .store()
        .deny(&host_id("old-1"))
        .expect("denied");
    let mut store = Store::recorded("transfer-enrolled-files");
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    let database = TestDatabase::new();
    imported(&store, &database).expect("imported");
    let files =
        FileRecords::read_snapshot(Arc::new(RealFs), &enrolling.files.store).expect("the files");
    for (table, rows) in [
        ("enrollment_requests", files.requests.len()),
        ("enrollment_issued", files.issued.len()),
        ("enrollment_hosts", files.hosts.len()),
        ("enrollment_deny", files.deny.len()),
    ] {
        assert_eq!((table, count(&database, table)), (table, rows));
        assert!(rows > 0, "{table}");
    }
    let next = files.serial.expect("a serial was taken");
    assert_eq!(
        database.rows("SELECT next FROM enrollment_serial"),
        [[Some(next.to_string())]]
    );
}

/// Proves: an export writes into targets that hold no state, and refuses any that do (a state
/// file, an audit segment, an enrollment directory with anything in it), writing nothing then;
/// and is refused while a coordinator holds the database's lock.
///
/// Catches: an export that writes over a file of a file store, or reads a database a coordinator
/// is changing.
#[test]
fn export_refuses_targets_that_hold_state() {
    let database = TestDatabase::new();
    imported(&Store::recorded("transfer-export-from"), &database).expect("imported");
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
        Err(TransferError::Database(MoveError::Database(
            DatabaseError::Locked
        )))
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
    imported(&store, &database).expect("imported");
    let into = Store::new("transfer-export-no-dir");
    assert!(matches!(
        export(&export_flags(&database, &into), &FileLock, &clock()),
        Err(TransferError::EnrollmentDirNeeded)
    ));
    assert!(into.files().is_empty());
}

/// Proves: the files an export writes are the state the database holds: imported again into a
/// second database, they give back every row of every table, the audit history with its numbers
/// and the enrollment store; and a pruned history is written as a sealed segment past the first,
/// so the file store says the same time of its oldest line.
///
/// Catches: a part an export leaves out or writes in a form the file store does not read, or an
/// export that turns a pruned history into a whole one.
#[test]
fn an_export_imported_again_gives_back_every_row() {
    let enrolling = Enrolling::new("transfer-round-ca");
    enrolled(&enrolling.enrollment, &HostKey::new(), "m-1", "web-1");
    let mut store = Store::recorded("transfer-round");
    store.put("desired.json.jti", b"live 99999999999\n");
    audit(&store, 9, 1_850_000_000, 2);
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());
    let first = TestDatabase::new();
    imported(&store, &first).expect("imported");

    let mut out = Store::new("transfer-round-out");
    out.paths.enrollment_dir = Some(out.dir.path().join("enrollment"));
    export(&export_flags(&first, &out), &FileLock, &clock()).expect("exported");
    let active = crate::audit_file::path_of(&out.ops());
    assert!(!active.exists(), "the pruned history is sealed");
    assert!(crate::audit_file::segment_path(&active, 2).exists());

    let second = TestDatabase::new();
    // The export holds the first import's row: the second import's row is one past it.
    imported(&out, &second).expect("imported again");
    for table in STATE_TABLES {
        let order = match table {
            "audit" => " WHERE action <> 'state_imported'",
            _ => "",
        };
        let rows = |database: &TestDatabase| {
            let mut rows = database.rows(&format!("SELECT * FROM {table}{order}"));
            rows.sort();
            rows
        };
        assert_eq!(rows(&second), rows(&first), "{table}");
    }
    assert_eq!(
        second.rows("SELECT audit_pruned_before FROM imports"),
        first.rows("SELECT audit_pruned_before FROM imports")
    );
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
    let mut flags = import_flags(&single, &lone, true);
    flags.legacy_service = Some("kbf".to_owned());
    import(&flags, &FileLock, &clock()).expect("imported");
    let several = TestDatabase::new();
    imported(&Store::recorded("transfer-form-map"), &several).expect("imported");
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
    imported(&store, &database).expect("imported");
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

/// The audit rows `database` holds once a coordinator started on it has audited the enrollment
/// store against them (`audit_to`), as `serve` does at its start.
fn audited_at_start(database: &TestDatabase) -> usize {
    let (store, loaded) = database.open("coordinator").expect("the store opens");
    let ops = Arc::new(OpsStore::over(Arc::new(store.clone()), loaded.ops));
    let records = Arc::new(PgRecords::over(&store).expect("the records"));
    let ca = scratch("transfer-start-ca");
    let files = crate::enrollment::tests::support::Files::in_dir(&ca);
    let enrollment = setup(
        Arc::new(RealFs),
        &crate::enrollment::Setup {
            store: StoreAt::Records(records),
            ..files.setup(true)
        },
        FixedClock::at(NOW),
        os_random(),
    )
    .expect("enrollment starts over the database");
    enrollment.audit_to(Arc::clone(&ops));
    store.stop();
    count(database, "audit")
}

/// Proves: a history whose oldest segments were deleted keeps its beginning across the move: an
/// approval older than the oldest line the files held has no line, and the first start on the
/// database adds none for it, as the file coordinator added none; with the import's record of
/// where the history begins taken away, the same start adds the approval's line.
///
/// Catches: an import that forgets `audit_pruned_before`, so the first start on the database
/// writes an audit row for every approval older than the retained history.
#[test]
fn import_of_a_pruned_history_adds_no_audit_rows_at_first_start() {
    let enrolling = Enrolling::new("transfer-pruned-ca");
    let key = HostKey::new();
    enrolling
        .enrollment
        .enroll(crate::enrollment::tests::support::enroll_request(
            &key, "m-1",
        ))
        .expect("enrolled");
    enrolling
        .enrollment
        .store()
        .approve_recorded(
            &key.key_hash(),
            &key.key_hash(),
            &hostkeeper_enrollment::enroll::Decision {
                host_id: host_id("web-1"),
                role: hostkeeper_enrollment::names::Role::parse("storage").expect("a role"),
                services: Vec::new(),
                replace: false,
            },
            &hostkeeper_enrollment::enroll::ApprovalRecord {
                approver: "alice".to_owned(),
                approved_unix: NOW,
                reason: "racked".to_owned(),
                mac_nonce: None,
                mac_enrolled_since_unix: None,
            },
        )
        .expect("approved by alice");
    let mut store = Store::new("transfer-pruned");
    // Every line kept is newer than the approval, which was made at NOW.
    audit(&store, 9, NOW + 100, 2);
    store.paths.enrollment_dir = Some(enrolling.files.store.clone());

    let database = TestDatabase::new();
    imported(&store, &database).expect("imported");
    let rows = count(&database, "audit");
    assert_eq!(audited_at_start(&database), rows);

    let unpruned = TestDatabase::new();
    imported(&store, &unpruned).expect("imported");
    unpruned
        .run("UPDATE imports SET audit_pruned_before = NULL")
        .expect("the record changed");
    assert_eq!(audited_at_start(&unpruned), rows + 1);
}
