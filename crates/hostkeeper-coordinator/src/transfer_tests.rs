//! `export` against a database of each test's own, and what its tests share: file stores of the
//! tests' own, a database filled with what one of them holds ([`seeded`]), and the state a file
//! store holds read as a start reads it ([`stored`]).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::FileRecords;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_testkit::Scratch;

use hostkeeper_mdm_gate::{DeviceStore as _, FileDeviceStore};

use super::{Export, TransferError};
use crate::audit_file::{self, AuditLine, AuditLog, DEFAULT_ROTATION, Detail, Rotation, UnixClock};
use crate::backup::StatePaths;
use crate::jti_file::jti_path;
use crate::lock::{FileLock, lock_path};
use crate::mdm::DEVICES_DIR;
use crate::store::Backend;
use crate::store::postgres::DatabaseError;
use crate::store::postgres::tests::seed::seed;
use crate::store::postgres::tests::{AT, TestDatabase};
use crate::store::postgres::transfer::Moved;
use crate::store::{Files, Loaded};
use crate::testutil::scratch;
use crate::verdict_file::verdicts_path;

pub(super) const DESIRED_MAP: &[u8] = include_bytes!("../tests/fixtures/recorded/desired-map.json");
pub(super) const DESIRED_SINGLE: &[u8] =
    include_bytes!("../tests/fixtures/recorded/desired-single.json");
pub(super) const OPS: &[u8] = include_bytes!("../tests/fixtures/recorded/ops.json");
pub(super) const HALTS: &[u8] = include_bytes!("../tests/fixtures/recorded/ops.json.halts");

/// The clock of every export: [`AT`].
pub(super) fn clock() -> UnixClock {
    Arc::new(|| AT)
}

/// A file store in a directory of its own: `desired.json`, the ops file beside it by default,
/// and with `enrollment`, the enrollment store of a CA started in it.
pub(super) struct Store {
    pub(super) dir: Scratch,
    /// Where the connection-string files of the tests are, beside the store's own directory.
    secrets: Scratch,
    pub(super) paths: StatePaths,
}

impl Store {
    pub(super) fn new(name: &str) -> Store {
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
    pub(super) fn recorded(name: &str) -> Store {
        let store = Store::new(name);
        store.put("desired.json", DESIRED_MAP);
        store.put("desired.json.ops", OPS);
        store.put("desired.json.ops.halts", HALTS);
        store
    }

    pub(super) fn put(&self, name: &str, bytes: &[u8]) {
        let path = self.dir.path().join(name);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        fs::write(path, bytes).expect("write a file of the store");
    }

    pub(super) fn ops(&self) -> PathBuf {
        self.paths.ops_file()
    }

    /// Every file under the store's directory, with its bytes; the lock file, which holds no
    /// state and stays once made, left out.
    pub(super) fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
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
pub(super) fn dsn_file(dir: &Path, database: &TestDatabase) -> PathBuf {
    let path = dir.join(format!("{}.dsn", database.name()));
    fs::write(&path, database.dsn_text()).expect("write the connection string");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("owner-only");
    path
}

/// Everything the file store `store` holds, read by the readers a start uses, the single-state
/// desired file read as the state of `legacy` and CI's spent ids past at [`AT`] left out: the
/// desired state, the operations state, the verdicts and the spent ids; the whole audit history and
/// the time of its oldest line when older ones were deleted; and, with its enrollment directory,
/// the enrollment records and the MDM gate's device records in the order of their enrollment ids.
pub(super) fn stored(store: &Store, legacy: Option<&str>) -> Moved {
    let state = &store.paths.state_file;
    let ops = store.ops();
    let files = Files::default()
        .with_desired(state, legacy)
        .with_verdicts(&verdicts_path(state))
        .with_ops(&ops, DEFAULT_ROTATION)
        .expect("the ops files")
        .with_jti(&jti_path(state), Arc::new(|| AT))
        .expect("the spent ids");
    let loaded = files.load().expect("the files load");
    let mut audit = Vec::new();
    AuditLog::open(audit_file::path_of(&ops), DEFAULT_ROTATION)
        .expect("the audit file")
        .each(&mut |line| audit.push(line))
        .expect("the audit lines");
    let pruned_before = files.audit_pruned_before().expect("the oldest line");
    let (records, mut devices) = match &store.paths.enrollment_dir {
        Some(dir) => {
            let devices = dir.join(DEVICES_DIR);
            let held = if devices.is_dir() {
                FileDeviceStore::open(Arc::new(RealFs), &devices)
                    .and_then(|store| store.load())
                    .expect("the device records")
            } else {
                Vec::new()
            };
            (
                FileRecords::read_snapshot(Arc::new(RealFs), dir).expect("the enrollment store"),
                held,
            )
        }
        None => Default::default(),
    };
    devices.sort_by(|a, b| a.enrollment_id.as_str().cmp(b.enrollment_id.as_str()));
    Moved {
        loaded,
        audit,
        records,
        devices,
        pruned_before,
    }
}

/// `database` filled with what the file store `store` holds ([`stored`]), as a coordinator that
/// served on it would have left it.
pub(super) fn seeded(store: &Store, database: &TestDatabase) {
    seed(database, &stored(store, None));
}

pub(super) fn export_flags(database: &TestDatabase, into: &Store) -> Export {
    Export {
        database_dsn_file: dsn_file(into.secrets.path(), database),
        paths: into.paths.clone(),
        legacy_service: None,
        into_empty: true,
    }
}

/// The rows of `table` in `database`.
pub(super) fn count(database: &TestDatabase, table: &str) -> usize {
    database.rows(&format!("SELECT 1 FROM {table}")).len()
}

/// What `database` holds, as a start loads it.
pub(super) fn loaded(database: &TestDatabase) -> Loaded {
    let (store, loaded) = database.open("coordinator").expect("the store opens");
    store.stop();
    loaded
}

/// What the file store `store` holds, as a start loads it.
pub(super) fn files_loaded(store: &Store, legacy: Option<&str>) -> Loaded {
    Files::default()
        .with_desired(&store.paths.state_file, legacy)
        .with_ops(&store.ops(), crate::audit_file::DEFAULT_ROTATION)
        .expect("the ops files")
        .load()
        .expect("the files load")
}

/// An audit line of the tests.
pub(super) fn line(seq: u64, at_unix: u64) -> AuditLine {
    AuditLine {
        seq,
        at_unix,
        by: "alice".to_owned(),
        action: "exclude".to_owned(),
        detail: Detail {
            host_group: None,
            subject: None,
            source: None,
            service: Some("kbf".to_owned()),
            node_id: Some(format!("s{seq}")),
            reason: Some("a reason".to_owned()),
        },
    }
}

/// Appends lines `1..=n` to the audit file of `store`, each `at + seq`, sealed into segments of
/// about two lines, and the oldest beyond `retained` deleted.
pub(super) fn audit(store: &Store, n: u64, at: u64, retained: usize) -> Vec<AuditLine> {
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

/// Proves: an export refuses a database whose schema is newer than this build, as a start
/// refuses it, and writes nothing.
///
/// Catches: a release that reads out of tables whose newer parts it cannot keep.
#[test]
fn a_newer_schema_is_refused_by_export() {
    let database = TestDatabase::new();
    let (opened, _) = database.open("coordinator").expect("the store opens");
    opened.stop();
    database
        .run("INSERT INTO schema_version (version, name, older_releases) VALUES (999, 'later', 'refuse')")
        .expect("a newer migration");
    let into = Store::new("transfer-newer-out");
    match super::export(&export_flags(&database, &into), &FileLock, &clock()) {
        Err(TransferError::Database(DatabaseError::Schema(why))) => {
            assert!(why.contains("migration 999, later"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(into.files().is_empty());
    assert_eq!(count(&database, "desired"), 0);
}
