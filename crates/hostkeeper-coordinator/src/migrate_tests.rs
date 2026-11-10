//! `host-groups migrate` over a file store on disk: what a dry run prints and leaves alone, what
//! `--apply` writes, what refuses it, and what a stop partway leaves behind.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::{FileRecords, Record, Records as _};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_testkit::Scratch;

use crate::assignment::Entry;
use crate::audit_file::UnixClock;
use crate::backup::{StatePaths, migrate_marker};
use crate::desired::{self, Held};
use crate::lock::{FileLock, lock_path};
use crate::migrate::{Migrate, MigrateError, migrate};
use crate::migrate_support::{NOW, desired as desired_at, host};
use crate::ops::OpsState;
use crate::ops_file;
use crate::repositories::{self, Registry, repositories_path};
use crate::testutil::{scratch, version};
use crate::transfer::read_files;

/// The rollout file of the tests: `kbf`, and its config package from a private repository.
pub(super) const ROLLOUT: &str = "\
[[service]]
name = \"kbf\"
roles = [\"web\"]
bake_beats = 2
min_version = \"1.0.0\"

[[service]]
name = \"kbf-config\"
roles = [\"web\"]
repository = \"private\"
";

/// The clock of every migration: [`NOW`].
pub(super) fn clock() -> UnixClock {
    Arc::new(|| NOW)
}

/// A file store in a directory of its own: three web hosts (the third without the config
/// package), the desired version of `kbf`, a registry holding `private`, and the rollout file.
pub(super) struct Disk {
    pub(super) dir: Scratch,
    pub(super) state: PathBuf,
    pub(super) enrollment: PathBuf,
    pub(super) rollout: PathBuf,
}

impl Disk {
    pub(super) fn new(name: &str) -> Disk {
        let dir = scratch(name);
        let disk = Disk {
            state: dir.path().join("desired.json"),
            enrollment: dir.path().join("enrollment"),
            rollout: dir.path().join("rollout.toml"),
            dir,
        };
        let mut held = Held::default();
        held.desired.insert("kbf".to_owned(), desired_at("2.1.0"));
        held.repositories = Registry::default()
            .put(crate::repositories_support::repository(
                "private",
                "https://private.example.com/apt",
                &["testing"],
                2,
            ))
            .expect("a registry");
        fs::write(&disk.state, desired::encode(&held, None).expect("encodes")).expect("write");
        fs::write(
            repositories_path(&disk.state),
            repositories::encode_file(&held.repositories).expect("encodes"),
        )
        .expect("write");
        let mut ops = OpsState::default();
        ops.assignments
            .entry("kbf-config".to_owned())
            .or_default()
            .hosts
            .insert("w3".to_owned(), Entry::Exclude);
        disk.put_ops(&ops);
        let records = FileRecords::open(Arc::new(RealFs), &disk.enrollment).expect("a store");
        let hosts: Vec<Record> = ["w1", "w2", "w3"]
            .iter()
            .map(|id| Record::Host(host(id, "web")))
            .collect();
        records.commit(&hosts).expect("hosts");
        disk.put_rollout(ROLLOUT);
        disk
    }

    /// Writes `ops` as the ops file.
    pub(super) fn put_ops(&self, ops: &OpsState) {
        let path = self.paths().ops_file();
        let (text, halts) = ops_file::texts(&path, ops).expect("the ops texts");
        fs::write(&path, text).expect("write");
        if let Some(halts) = halts {
            fs::write(ops_file::halts_path(&path), halts).expect("write");
        }
    }

    /// Writes `text` as the rollout file, readable by its owner alone.
    pub(super) fn put_rollout(&self, text: &str) {
        fs::write(&self.rollout, text).expect("write");
        fs::set_permissions(&self.rollout, fs::Permissions::from_mode(0o600)).expect("mode");
    }

    pub(super) fn paths(&self) -> StatePaths {
        StatePaths {
            state_file: self.state.clone(),
            ops_file: None,
            enrollment_dir: Some(self.enrollment.clone()),
        }
    }

    /// The flags of the migration of this store.
    pub(super) fn flags(&self, apply: bool) -> Migrate {
        Migrate {
            rollout_file: self.rollout.clone(),
            state_file: Some(self.state.clone()),
            ops_file: None,
            enrollment_dir: Some(self.enrollment.clone()),
            database_dsn_file: None,
            legacy_service: None,
            live_rollouts: 2,
            mac_serial_allowlist: None,
            apply,
        }
    }

    /// Every file under the store's directory with its bytes, the lock's left out.
    pub(super) fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(self.dir.path(), &mut out);
        out.remove(&lock_path(&self.state));
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

/// The report of the migration `flags`, or its refusal.
pub(super) fn run(flags: &Migrate) -> Result<String, MigrateError> {
    migrate(flags, &FileLock, &clock()).map(|report| report.to_string())
}

/// The dry run's report of the store's files.
const DRY_RUN: &str = "\
hostkeeper-coordinator host-groups migrate: the file store of <state> (dry run)
group web (from role web): 2 hosts; packages hostkeeper, kbf, kbf-config
  policy: bake_beats 2
  bake_beats 2: the highest of kbf 2, kbf-config 0
  desired kbf 2.1.0 (generation 1)
  minimum kbf 1.0.0 (from the rollout file)
group web-2 (from role web): 1 hosts; packages hostkeeper, kbf
  policy: bake_beats 2
  desired kbf 2.1.0 (generation 1)
  minimum kbf 1.0.0 (from the rollout file)
hosts:
  web: w1, w2
  web-2: w3
repositories:
  kbf-config: private
records:
live_rollouts: 2 groups may each roll 2 packages at once where the fleet rolled 2
no errors
nothing written; --apply writes it in one commit
";

/// Proves: without `--apply` the migration prints its plan (the groups with their hosts, packages
/// and policy, the desired versions, minimum versions, repositories and the live-rollouts note,
/// every line exact) and writes nothing: no file of the store changes, and no lock file or marker
/// is made.
///
/// Catches: a dry run that changes the store it is meant to be run on before the window, or a
/// report an operator reads differently from the one the tests hold.
#[test]
fn a_dry_run_prints_the_plan_and_writes_nothing() {
    let disk = Disk::new("migrate-dry-run");
    let before = disk.files();
    let report = run(&disk.flags(false)).expect("a plan");
    assert_eq!(
        report,
        DRY_RUN.replace("<state>", &disk.state.display().to_string())
    );
    assert_eq!(disk.files(), before);
    assert!(!migrate_marker(&disk.state).exists());
    assert!(!lock_path(&disk.state).exists());
}

/// Proves: `--apply` writes the groups, the marks, the minimum versions, the repository, the
/// host group of every host and the audit lines, one commit: a read of the store afterwards holds
/// each, the marker is gone, the report says the audit numbers, and a second migration is
/// refused as already done.
///
/// Catches: a write that leaves the hosts without their group, the groups without their hosts, or
/// the audit without its entries.
#[test]
fn apply_writes_the_groups_the_records_and_the_audit() {
    let disk = Disk::new("migrate-apply");
    let report = run(&disk.flags(true)).expect("applied");
    let expect = DRY_RUN
        .replace("<state>", &disk.state.display().to_string())
        .replace("(dry run)", "(apply)")
        .replace(
            "nothing written; --apply writes it in one commit",
            "written in one commit (files); audit entries 1..5",
        );
    assert_eq!(report, expect);
    assert!(!migrate_marker(&disk.state).exists());
    let moved = read_files(&disk.paths(), None, NOW).expect("the store reads");
    let groups = &moved.loaded.desired.host_groups.0;
    assert_eq!(
        groups.keys().map(ToString::to_string).collect::<Vec<_>>(),
        ["web", "web-2"]
    );
    assert_eq!(groups[&"web".parse().expect("a name")].by, "migration");
    let of: BTreeMap<&str, &str> = moved
        .records
        .hosts
        .iter()
        .map(|h| (h.host_id.as_str(), h.host_group.as_str()))
        .collect();
    assert_eq!(of, [("w1", "web"), ("w2", "web"), ("w3", "web-2")].into());
    let web = &moved.loaded.ops.host_groups.0[&"web".parse().expect("a name")];
    assert_eq!(web["kbf"].min_version, Some(version("1.0.0")));
    assert_eq!(
        moved.loaded.ops.services["kbf-config"]
            .repository
            .as_deref(),
        Some("private")
    );
    let audit: Vec<(u64, &str, &str)> = moved
        .audit
        .iter()
        .map(|l| (l.seq, l.by.as_str(), l.action.as_str()))
        .collect();
    assert_eq!(
        audit,
        [
            (1, "migration", "host_group_put"),
            (2, "migration", "set_min_version"),
            (3, "migration", "host_group_put"),
            (4, "migration", "set_min_version"),
            (5, "migration", "set_repository"),
        ][..]
    );
    assert!(matches!(
        run(&disk.flags(true)),
        Err(MigrateError::AlreadyMigrated)
    ));
    assert!(matches!(
        run(&disk.flags(false)),
        Err(MigrateError::AlreadyMigrated)
    ));
}
