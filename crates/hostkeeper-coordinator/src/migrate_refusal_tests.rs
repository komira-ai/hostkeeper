//! What refuses `host-groups migrate`, and what a stop partway of `--apply` leaves: the plan that
//! holds an error, the flags that name no store, the markers and locks of the files, and the
//! writes that fail.

use std::fs;

use crate::backup::{Backup, BackupError, Restore, StatePaths, backup, migrate_marker, restore};
use crate::config::{StartError, refuse_unfinished_migrate};
use crate::lock::{FileLock, StateLock, lock_path};
use crate::migrate::MigrateError;
use crate::migrate_apply::apply_files;
use crate::migrate_plan::{Inputs, Plan, plan};
use crate::migrate_tests::{Disk, ROLLOUT, run};
use crate::services::Services;
use crate::store::Backend as _;
use crate::store::Files;
use crate::store::postgres::transfer::Moved;
use crate::testutil::scratch;
use crate::transfer::{TransferError, read_files};

/// The text a marker refuses with.
fn stopped(disk: &Disk) -> String {
    format!(
        "{}: a host-groups migrate --apply stopped partway; restore the backup taken before it and run it again",
        migrate_marker(&disk.state).display()
    )
}

/// The store of `disk` as it is, and the plan of its migration.
fn planned(disk: &Disk) -> (Moved, Plan) {
    let moved = read_files(&disk.paths(), None, crate::migrate_support::NOW).expect("it reads");
    let rollout = Services::from_file(&disk.rollout).expect("a rollout file");
    let made = plan(&Inputs {
        moved: &moved,
        rollout: &rollout,
        allowlist: None,
        live_rollouts: 1,
        now: crate::migrate_support::NOW,
    });
    assert_eq!(made.errors, Vec::<String>::new());
    (moved, made)
}

/// Proves: a plan that holds an error is printed whole, with the error under `errors:`, by a dry
/// run (an error, exit 1) and by `--apply`, and neither writes anything: no file of the store
/// changes and no marker is made.
///
/// Catches: an `--apply` that writes the part of a plan that has no error, leaving a store the
/// operator did not read the plan of.
#[test]
fn a_plan_with_an_error_is_printed_and_apply_writes_nothing() {
    let disk = Disk::new("migrate-error");
    disk.put_rollout(&ROLLOUT.replace("\"private\"", "\"missing\""));
    let before = disk.files();
    for apply in [false, true] {
        let Err(MigrateError::Refused(text)) = run(&disk.flags(apply)) else {
            panic!("the plan holds an error");
        };
        let mode = if apply { "apply" } else { "dry run" };
        assert!(
            text.starts_with(&format!(
                "hostkeeper-coordinator host-groups migrate: the file store of {} ({mode})\n",
                disk.state.display()
            )),
            "{text}"
        );
        assert!(
            text.ends_with(
                "errors:\n  service kbf-config: its repository missing is not in the registry\n\
                 nothing written; --apply refuses while there is an error"
            ),
            "{text}"
        );
    }
    assert_eq!(disk.files(), before);
    assert!(!migrate_marker(&disk.state).exists());
}

/// Proves: `--state-file` with no enrollment store is refused naming `--enrollment-dir`; a
/// database beside any file flag is refused naming the flag, as `serve` refuses it; a rollout
/// file that cannot be read, and an allowlist that cannot be, are refused naming their flags.
///
/// Catches: a migration run over the wrong store, or over a store with no hosts, which would
/// write groups with nobody in them.
#[test]
fn the_flags_must_name_one_store_with_hosts_and_readable_files() {
    let disk = Disk::new("migrate-flags");
    let mut flags = disk.flags(false);
    flags.enrollment_dir = None;
    assert_eq!(
        run(&flags).expect_err("refused").to_string(),
        "--enrollment-dir is not given: without the enrolled hosts there is no host to put in a group"
    );
    for (flag, edit) in [
        ("--state-file", 0),
        ("--ops-file", 1),
        ("--enrollment-dir", 2),
    ] {
        let mut flags = disk.flags(false);
        flags.database_dsn_file = Some(disk.dir.path().join("dsn"));
        flags.state_file = (edit == 0).then(|| disk.state.clone());
        flags.ops_file = (edit == 1).then(|| disk.state.clone());
        flags.enrollment_dir = (edit == 2).then(|| disk.enrollment.clone());
        assert_eq!(
            run(&flags).expect_err("refused").to_string(),
            format!(
                "--database-dsn-file and {flag} are both given: {flag} names a file of the file store, and the database holds what it held"
            )
        );
    }
    let mut flags = disk.flags(false);
    flags.rollout_file = disk.dir.path().join("none.toml");
    assert!(
        run(&flags)
            .expect_err("refused")
            .to_string()
            .starts_with("--rollout-file: "),
    );
    let mut flags = disk.flags(false);
    flags.mac_serial_allowlist = Some(disk.dir.path().join("none.toml"));
    assert!(
        run(&flags)
            .expect_err("refused")
            .to_string()
            .starts_with("--mac-serial-allowlist ")
    );
}

/// Proves: the Mac allowlist is read and the edits each entry needs are in the report; one whose
/// services its group does not list is an error naming the serial.
///
/// Catches: an allowlist that is not read, so that its entries are found wanting only at the
/// new coordinator's start.
#[test]
fn the_allowlist_is_read_and_its_edits_are_printed() {
    let disk = Disk::new("migrate-allowlist");
    let list = disk.dir.path().join("macs.toml");
    fs::write(
        &list,
        "[[mac]]\nserial = \"C02AAA\"\nhost_id_prefix = \"mac-\"\nrole = \"web\"\nservices = [\"kbf\"]\n",
    )
    .expect("write");
    let mut flags = disk.flags(false);
    flags.mac_serial_allowlist = Some(list.clone());
    let report = run(&flags).expect("a plan");
    assert!(
        report
            .contains("  allowlist entry C02AAA: host_group = \"web\", and its services kbf go\n"),
        "{report}"
    );
    fs::write(
        &list,
        "[[mac]]\nserial = \"C02AAA\"\nhost_id_prefix = \"mac-\"\nrole = \"web\"\nservices = [\"other\"]\n",
    )
    .expect("write");
    let Err(MigrateError::Refused(text)) = run(&flags) else {
        panic!("an error");
    };
    assert!(
        text.contains("allowlist entry C02AAA: its services other are not packages of group web"),
        "{text}"
    );
}

/// Proves: `--apply` takes the lock of the state files, so a running coordinator refuses it
/// naming the lock and nothing is written, while a dry run needs no lock.
///
/// Catches: a migration over files a coordinator is changing.
#[test]
fn apply_is_refused_while_a_coordinator_holds_the_lock() {
    let disk = Disk::new("migrate-locked");
    let held = FileLock.acquire(&lock_path(&disk.state)).expect("the lock");
    let before = disk.files();
    match run(&disk.flags(true)) {
        Err(MigrateError::Files(BackupError::Locked { path })) => {
            assert_eq!(path, lock_path(&disk.state));
        }
        other => panic!("{other:?}"),
    }
    assert!(run(&disk.flags(false)).is_ok());
    assert_eq!(disk.files(), before);
    drop(held);
    assert!(run(&disk.flags(true)).is_ok());
}

/// Proves: the marker of an `--apply` that stopped partway refuses, with the text that says to
/// restore the backup, `serve`'s start, `import`'s and `migrate`'s read of the files and `backup`;
/// and `restore` and `export` take it for state in the target and refuse it as such.
///
/// Catches: a coordinator started over files that hold part of a migration.
#[test]
fn the_marker_of_a_stopped_apply_refuses_every_verb_that_reads_the_files() {
    let disk = Disk::new("migrate-marker");
    let out = scratch("migrate-marker-out");
    let snapshot = out.path().join("snapshot");
    backup(
        &Backup {
            paths: disk.paths(),
            out: snapshot.clone(),
        },
        &FileLock,
    )
    .expect("a backup before the marker");
    fs::write(migrate_marker(&disk.state), b"").expect("the marker");
    let text = stopped(&disk);
    assert_eq!(
        refuse_unfinished_migrate(&disk.state)
            .expect_err("refused")
            .to_string(),
        text
    );
    assert!(matches!(
        refuse_unfinished_migrate(&disk.state),
        Err(StartError::MigrateUnfinished { .. })
    ));
    assert_eq!(
        read_files(&disk.paths(), None, 0)
            .expect_err("refused")
            .to_string(),
        text
    );
    assert!(matches!(
        read_files(&disk.paths(), None, 0),
        Err(TransferError::Files(BackupError::MigrateUnfinished { .. }))
    ));
    assert_eq!(
        run(&disk.flags(false)).expect_err("refused").to_string(),
        text
    );
    assert_eq!(
        run(&disk.flags(true)).expect_err("refused").to_string(),
        text
    );
    let again = Backup {
        paths: disk.paths(),
        out: out.path().join("again"),
    };
    assert_eq!(
        backup(&again, &FileLock).expect_err("refused").to_string(),
        text
    );
    let target = scratch("migrate-marker-target");
    let into = StatePaths {
        state_file: target.path().join("desired.json"),
        ops_file: None,
        enrollment_dir: Some(target.path().join("enrollment")),
    };
    fs::write(migrate_marker(&into.state_file), b"").expect("a marker in the target");
    let flags = Restore {
        from: snapshot,
        paths: into.clone(),
        into_empty: true,
    };
    assert!(matches!(
        restore(&flags, &FileLock),
        Err(BackupError::NotEmpty { path }) if path == migrate_marker(&into.state_file)
    ));
}

/// Proves: a write of `--apply` that fails leaves the marker and returns the error, and the files
/// after the failed one are as they were: the enrollment store's directory unusable leaves the
/// ops and state files untouched; the ops file unusable leaves the state file untouched but
/// the records written; the state file unusable leaves the marker with everything else written;
/// and a marker that cannot be made stops before any write. A migration run after any of them is
/// refused by the marker.
///
/// Catches: a stop partway that leaves the store looking migrated, or the unmigrated files with
/// no marker to say they are not.
#[test]
fn a_write_that_fails_leaves_the_marker_and_the_later_files_as_they_were() {
    // The enrollment store cannot be opened.
    let disk = Disk::new("migrate-fails-records");
    let (moved, made) = planned(&disk);
    let (state, ops) = (disk.state.clone(), disk.paths().ops_file());
    let (state_text, ops_text) = (
        fs::read(&state).expect("read"),
        fs::read(&ops).expect("read"),
    );
    fs::remove_dir_all(disk.enrollment.join("hosts")).expect("remove");
    fs::write(disk.enrollment.join("hosts"), b"").expect("a file in its place");
    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);
    assert!(
        matches!(result, Err(MigrateError::Records(_))),
        "{result:?}"
    );
    assert!(migrate_marker(&state).exists());
    assert_eq!(
        (
            fs::read(&state).expect("read"),
            fs::read(&ops).expect("read")
        ),
        (state_text, ops_text)
    );
    assert_eq!(
        run(&disk.flags(true)).expect_err("refused").to_string(),
        stopped(&disk)
    );

    // The ops file is a directory.
    let disk = Disk::new("migrate-fails-ops");
    let (moved, made) = planned(&disk);
    let state_text = fs::read(&disk.state).expect("read");
    let ops = disk.paths().ops_file();
    fs::remove_file(&ops).expect("remove");
    fs::create_dir(&ops).expect("a directory in its place");
    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);
    assert!(result.is_err());
    assert!(migrate_marker(&disk.state).exists());
    assert_eq!(fs::read(&disk.state).expect("read"), state_text);
    let hosts = hostkeeper_enrollment::enroll::records::FileRecords::read_snapshot(
        std::sync::Arc::new(hostkeeper_enrollment::fs::RealFs),
        &disk.enrollment,
    )
    .expect("the records read");
    assert!(hosts.hosts.iter().all(|h| !h.host_group.is_empty()));

    // The state file is a directory: the ops file is written, the marker stays.
    let disk = Disk::new("migrate-fails-state");
    let (moved, made) = planned(&disk);
    fs::remove_file(&disk.state).expect("remove");
    fs::create_dir(&disk.state).expect("a directory in its place");
    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);
    assert!(result.is_err());
    assert!(migrate_marker(&disk.state).exists());
    let loaded = Files::default()
        .with_ops(
            &disk.paths().ops_file(),
            crate::audit_file::DEFAULT_ROTATION,
        )
        .expect("the ops file")
        .load()
        .expect("it loads");
    assert!(!loaded.ops.host_groups.is_empty());

    // The marker cannot be made.
    let disk = Disk::new("migrate-fails-marker");
    let (moved, made) = planned(&disk);
    let before = disk.files();
    fs::create_dir(migrate_marker(&disk.state)).expect("a directory in its place");
    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);
    assert!(matches!(result, Err(MigrateError::Io { .. })), "{result:?}");
    fs::remove_dir(migrate_marker(&disk.state)).expect("remove");
    assert_eq!(disk.files(), before);
}

/// Proves: the files of a migrated store load as `serve` loads them: the groups are held, so the
/// coordinator is in group mode, the marks and the repository are in the ops state, and the
/// hosts' records name their groups; a service that names no repository other than `komira` has
/// none recorded.
///
/// Catches: a store the new coordinator reads as no groups, or without the repository of a
/// private config package, which would then deploy from `komira`.
#[test]
fn the_migrated_files_load_as_a_start_loads_them() {
    let disk = Disk::new("migrate-loads");
    run(&disk.flags(true)).expect("applied");
    let loaded = Files::default()
        .with_desired(&disk.state, None)
        .with_ops(
            &disk.paths().ops_file(),
            crate::audit_file::DEFAULT_ROTATION,
        )
        .expect("the ops file")
        .load()
        .expect("it loads");
    assert!(!loaded.desired.host_groups.is_empty());
    assert_eq!(loaded.desired.desired["kbf"].version.to_string(), "2.1.0");
    assert_eq!(
        loaded.ops.services["kbf-config"].repository.as_deref(),
        Some("private")
    );
    assert!(!loaded.ops.services.contains_key("kbf"));
    let state = fs::read_to_string(&disk.state).expect("read");
    assert!(state.contains("\"host_groups\""), "{state}");
}

/// Proves: each refusal of the verb says what it is, in words that name the flag or file at
/// fault: the unsettled changes, the rollout file, the allowlist, the database, a flag, an
/// enrollment store, the ops file, a state file and a file that could not be made.
///
/// Catches: a refusal that prints its type's name, which an operator cannot act on.
#[test]
fn every_refusal_says_what_it_is() {
    use crate::enrollment::AllowlistError;
    use crate::ops::OpsError;
    use crate::services::ServicesError;
    use crate::store::BackendError;
    use crate::store::postgres::transfer::MoveError;
    use hostkeeper_enrollment::enroll::StoreError;
    let path = std::path::PathBuf::from("/state/desired.json.ops");
    let said = |why: MigrateError| why.to_string();
    assert_eq!(
        said(MigrateError::Transfer(TransferError::Unsettled {
            path: path.clone()
        })),
        format!(
            "{}: changes stood in memory and were not yet written when the file coordinator stopped; start it once on these files, so that it settles them, stop it, and import again",
            path.display()
        )
    );
    assert_eq!(
        said(MigrateError::Rollout(ServicesError::Read(
            "gone".to_owned()
        ))),
        "--rollout-file: read gone"
    );
    assert_eq!(
        said(MigrateError::Allowlist(AllowlistError::Invalid(
            "bad".to_owned()
        ))),
        "--mac-serial-allowlist: bad"
    );
    assert_eq!(
        said(MigrateError::AlreadyMigrated),
        "the store already holds a host group: it is migrated already"
    );
    assert_eq!(
        said(MigrateError::Database(MoveError::HoldsState {
            table: "audit",
            rows: 2
        })),
        "the database holds state (2 rows in audit): import writes only into a database that holds none; drop it and create it again before another import"
    );
    assert_eq!(
        said(MigrateError::Flag(StartError::DatabaseAndFileFlag(
            "--ops-file"
        ))),
        "--database-dsn-file and --ops-file are both given: --ops-file names a file of the file store, and the database holds what it held"
    );
    assert_eq!(
        said(MigrateError::Records(StoreError::Unkeyed("host"))),
        format!("--enrollment-dir: {}", StoreError::Unkeyed("host"))
    );
    let full = || OpsError::AuditUnavailable(std::io::Error::other("full"));
    assert_eq!(said(MigrateError::Ops(full())), full().to_string());
    assert_eq!(
        said(MigrateError::Backend(BackendError::Spent)),
        "the token id is spent"
    );
    let io = std::io::Error::other("no space");
    assert_eq!(
        said(MigrateError::Io {
            path: path.clone(),
            source: io
        }),
        format!("{}: no space", path.display())
    );
}
