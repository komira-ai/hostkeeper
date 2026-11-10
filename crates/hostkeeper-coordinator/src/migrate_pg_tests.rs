//! `host-groups migrate` over a database: the plan it prints, the one transaction it writes, and
//! what it leaves when a statement fails or the read back differs.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::audit_file::{AuditLine, Detail};
use crate::migrate::{Migrate, MigrateError};
use crate::migrate_support::NOW;
use crate::migrate_tests::{Disk, ROLLOUT, run};
use crate::store::postgres::DatabaseError;
use crate::store::postgres::tests::TestDatabase;
use crate::store::postgres::transfer::{MoveError, Rewrite, import, read_only, rewrite};
use crate::transfer::read_files;

/// A database holding what the files of a [`Disk`] hold, as an import leaves it, and the flags
/// of a migration of it.
pub(crate) struct Served {
    pub(crate) disk: Disk,
    pub(crate) database: TestDatabase,
    pub(crate) dsn: PathBuf,
}

impl Served {
    pub(crate) fn new(name: &str) -> Served {
        let disk = Disk::new(name);
        let database = TestDatabase::new();
        let moved = read_files(&disk.paths(), None, NOW).expect("the files read");
        let imported = AuditLine {
            seq: 1,
            at_unix: NOW,
            by: "coordinator".to_owned(),
            action: "state_imported".to_owned(),
            detail: Detail::default(),
        };
        import(
            &database.dsn(),
            &moved,
            &serde_json::json!({ "files": [] }),
            &imported,
            NOW,
        )
        .expect("imported");
        let dsn = disk.dir.path().join("database.dsn");
        fs::write(&dsn, database.dsn_text()).expect("write");
        fs::set_permissions(&dsn, fs::Permissions::from_mode(0o600)).expect("mode");
        Served {
            disk,
            database,
            dsn,
        }
    }

    pub(crate) fn flags(&self, apply: bool) -> Migrate {
        Migrate {
            state_file: None,
            enrollment_dir: None,
            database_dsn_file: Some(self.dsn.clone()),
            ..self.disk.flags(apply)
        }
    }

    fn count(&self, table: &str) -> usize {
        self.database.rows(&format!("SELECT 1 FROM {table}")).len()
    }

    /// The groups of the hosts, `host_id=group` each, as the database holds them.
    fn hosts(&self) -> Vec<String> {
        self.database
            .rows("SELECT host_id, coalesce(host_group, '') FROM enrollment_hosts ORDER BY host_id")
            .into_iter()
            .map(|row| {
                format!(
                    "{}={}",
                    row[0].clone().unwrap_or_default(),
                    row[1].clone().unwrap_or_default()
                )
            })
            .collect()
    }
}

/// Proves: without `--apply` the migration of a database prints the plan of the files (the same
/// lines, the header naming the database) and writes nothing: no host group, no record's group
/// and no audit row is added.
///
/// Catches: a dry run that changes the database it is run on.
#[test]
fn a_dry_run_on_a_database_prints_the_plan_and_writes_nothing() {
    let served = Served::new("migrate-pg-dry");
    let report = run(&served.flags(false)).expect("a plan");
    assert!(
        report.starts_with(&format!(
            "hostkeeper-coordinator host-groups migrate: the database of {} (dry run)\n\
             group web (from role web): 2 hosts; packages hostkeeper, kbf, kbf-config\n",
            served.dsn.display()
        )),
        "{report}"
    );
    assert!(
        report.ends_with("no errors\nnothing written; --apply writes it in one commit\n"),
        "{report}"
    );
    assert_eq!(served.count("host_groups"), 0);
    assert_eq!(served.count("audit"), 1);
    assert_eq!(served.hosts(), ["w1=", "w2=", "w3="]);
}

/// Proves: `--apply` on a database writes the groups, their packages, desired versions and marks,
/// the hosts' groups, the repository and the audit rows (numbered after the import's) in one
/// transaction; a start on the database loads what the plan said, the report names the audit
/// numbers, and a second migration is refused as already done and changes nothing.
///
/// Catches: a transaction that commits the groups without the hosts' records, or numbers its
/// audit rows over the import's.
#[test]
fn apply_on_a_database_writes_one_transaction_that_reads_back() {
    let served = Served::new("migrate-pg-apply");
    let report = run(&served.flags(true)).expect("applied");
    assert!(
        report.ends_with("no errors\nwritten in one transaction (database); audit entries 2..6\n"),
        "{report}"
    );
    assert_eq!(served.count("host_groups"), 2);
    assert_eq!(served.count("host_group_packages"), 5);
    assert_eq!(served.count("group_desired"), 2);
    assert_eq!(served.count("group_min_versions"), 2);
    assert_eq!(served.hosts(), ["w1=web", "w2=web", "w3=web-2"]);
    assert_eq!(
        served
            .database
            .rows("SELECT repository FROM services WHERE repository IS NOT NULL"),
        [[Some("private".to_owned())]]
    );
    assert_eq!(
        served
            .database
            .rows("SELECT seq, by_name, action FROM audit ORDER BY seq")[1..],
        [
            [
                Some("2".to_owned()),
                Some("migration".to_owned()),
                Some("host_group_put".to_owned())
            ],
            [
                Some("3".to_owned()),
                Some("migration".to_owned()),
                Some("set_min_version".to_owned())
            ],
            [
                Some("4".to_owned()),
                Some("migration".to_owned()),
                Some("host_group_put".to_owned())
            ],
            [
                Some("5".to_owned()),
                Some("migration".to_owned()),
                Some("set_min_version".to_owned())
            ],
            [
                Some("6".to_owned()),
                Some("migration".to_owned()),
                Some("set_repository".to_owned())
            ],
        ]
    );
    let (store, loaded) = served
        .database
        .open("coordinator")
        .expect("the store opens");
    store.stop();
    assert_eq!(loaded.desired.host_groups.0.len(), 2);
    assert_eq!(
        loaded.ops.services["kbf-config"].repository.as_deref(),
        Some("private")
    );
    assert!(matches!(
        run(&served.flags(true)),
        Err(MigrateError::AlreadyMigrated)
    ));
    assert!(matches!(
        run(&served.flags(false)),
        Err(MigrateError::AlreadyMigrated)
    ));
    assert_eq!(served.count("host_groups"), 2);
}

/// Proves: a plan that holds an error is printed and `--apply` on a database writes nothing.
///
/// Catches: a transaction committed over a plan whose errors the operator was shown.
#[test]
fn apply_on_a_database_refuses_a_plan_with_an_error() {
    let served = Served::new("migrate-pg-error");
    served
        .disk
        .put_rollout(&ROLLOUT.replace("\"private\"", "\"missing\""));
    for apply in [false, true] {
        assert!(matches!(
            run(&served.flags(apply)),
            Err(MigrateError::Refused(_))
        ));
    }
    assert_eq!(served.count("host_groups"), 0);
    assert_eq!(served.count("audit"), 1);
    assert_eq!(served.hosts(), ["w1=", "w2=", "w3="]);
}

/// Proves: a connection-string file that cannot be read is refused as the database's own refusal,
/// before anything is read or written.
///
/// Catches: a migration that starts without knowing which database it is to change.
#[test]
fn a_connection_string_that_cannot_be_read_is_refused() {
    let disk = Disk::new("migrate-pg-dsn");
    let flags = Migrate {
        state_file: None,
        enrollment_dir: None,
        database_dsn_file: Some(disk.dir.path().join("none.dsn")),
        ..disk.flags(false)
    };
    assert!(matches!(
        run(&flags),
        Err(MigrateError::Database(MoveError::Database(_)))
    ));
}

/// Proves: a coordinator serving on the database refuses `--apply`, naming the lock, while a dry
/// run, which takes none, prints the plan; and nothing is written.
///
/// Catches: a migration under a coordinator that is writing the same rows.
#[test]
fn a_serving_coordinator_refuses_apply() {
    let served = Served::new("migrate-pg-locked");
    let (store, _) = served
        .database
        .open("coordinator")
        .expect("the store opens");
    assert!(matches!(
        run(&served.flags(true)),
        Err(MigrateError::Database(MoveError::Database(
            DatabaseError::Locked
        )))
    ));
    assert!(run(&served.flags(false)).is_ok());
    store.stop();
    assert_eq!(served.count("host_groups"), 0);
}

/// Proves: a transaction whose statement fails (an audit number the database holds), whose
/// read back differs (a changed host record the rewrite does not write), or whose maker refuses,
/// is rolled back whole: the database holds no group, no record's group and no audit row more
/// than it did, and the difference names the first table.
///
/// Catches: a partial migration left in the database after a failure, which no backup of the
/// files would undo.
#[test]
fn a_failed_statement_a_difference_or_a_refusal_leaves_the_database_as_it_was() {
    let served = Served::new("migrate-pg-rollback");
    let dsn = served.database.dsn();
    let moved = read_only(&dsn, NOW).expect("a read");
    let plan = served_plan(&served, &moved);
    let rewritten = |lines: Vec<AuditLine>, drop_hosts: bool| Rewrite {
        after: plan.after.clone(),
        changed: if drop_hosts {
            hostkeeper_enrollment::enroll::records::Snapshot::default()
        } else {
            plan.changed.clone()
        },
        lines,
    };
    let unchanged = |served: &Served| {
        assert_eq!(served.count("host_groups"), 0);
        assert_eq!(served.count("audit"), 1);
        assert_eq!(served.hosts(), ["w1=", "w2=", "w3="]);
    };
    // A statement fails: the first line takes the number the import's row has.
    let mut taken = plan.audit.clone();
    taken[0].seq = 1;
    let result: Result<(), MigrateError> =
        rewrite(&dsn, NOW, |_| Ok((rewritten(taken, false), ())));
    assert!(
        matches!(
            result,
            Err(MigrateError::Database(MoveError::Database(
                DatabaseError::Refused(_)
            )))
        ),
        "{result:?}"
    );
    unchanged(&served);
    // The read back differs.
    let result: Result<(), MigrateError> =
        rewrite(&dsn, NOW, |_| Ok((rewritten(plan.audit.clone(), true), ())));
    assert!(
        matches!(
            result,
            Err(MigrateError::Database(MoveError::Rewritten {
                part: "enrollment_hosts"
            }))
        ),
        "{result:?}"
    );
    unchanged(&served);
    // The maker refuses.
    let result: Result<(), MigrateError> =
        rewrite(&dsn, NOW, |_| Err(MigrateError::AlreadyMigrated));
    assert!(matches!(result, Err(MigrateError::AlreadyMigrated)));
    unchanged(&served);
    // And the same rewrite, whole, commits.
    let result: Result<(), MigrateError> = rewrite(&dsn, NOW, |_| {
        Ok((rewritten(plan.audit.clone(), false), ()))
    });
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(served.count("host_groups"), 2);
}

/// The plan of the migration of the store `moved` read from `served`.
fn served_plan(
    served: &Served,
    moved: &crate::store::postgres::transfer::Moved,
) -> crate::migrate_plan::Plan {
    let rollout = crate::services::Services::from_file(&served.disk.rollout).expect("a file");
    crate::migrate_plan::plan(&crate::migrate_plan::Inputs {
        moved,
        rollout: &rollout,
        allowlist: None,
        live_rollouts: 1,
        now: NOW,
    })
}
