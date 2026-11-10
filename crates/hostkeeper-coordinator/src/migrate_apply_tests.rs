//! The failures of `host-groups migrate` that only a write meets (the files and the database are
//! read as they are before anything is written), and the requests of the store that a plan
//! reads without an approval.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use hostkeeper_enrollment::enroll::records::pb;

use crate::backup::migrate_marker;
use crate::lock::lock_path;
use crate::migrate::{Migrate, MigrateError};
use crate::migrate_apply::apply_files;
use crate::migrate_plan::{Inputs, Plan, plan};
use crate::migrate_support::{NOW, Store};
use crate::migrate_tests::{Disk, run};
use crate::services::Services;
use crate::store::postgres::tests::TestDatabase;
use crate::store::postgres::transfer::{MoveError, Moved};
use crate::transfer::read_files;

/// The store of `disk` as it is, and the plan of its migration.
fn planned(disk: &Disk) -> (Moved, Plan) {
    let moved = read_files(&disk.paths(), None, NOW).expect("it reads");
    let rollout = Services::from_file(&disk.rollout).expect("a rollout file");
    let made = plan(&Inputs {
        moved: &moved,
        rollout: &rollout,
        allowlist: None,
        live_rollouts: 1,
        now: NOW,
    });
    assert_eq!(made.errors, Vec::<String>::new());
    (moved, made)
}

/// Proves: `--apply` through the verb, over files it reads whole and a plan without an error,
/// whose marker cannot be made (the directory of the state file takes no new file) is refused
/// with the marker's path and the system's reason, and writes nothing: no file of the store
/// changes. The test needs a user the directory's mode binds: it fails, and says so, for root.
///
/// Catches: a write error of the files that the verb takes for success, or one that leaves part
/// of the migration behind.
#[test]
fn a_write_that_fails_through_the_verb_is_its_refusal_and_changes_nothing() {
    let disk = Disk::new("apply-verb-write");
    let marker = migrate_marker(&disk.state);
    // The lock file is there, so that taking the lock needs no new file.
    fs::File::create(lock_path(&disk.state)).expect("the lock file");
    let before = disk.files();
    let dir = disk.dir.path();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o500)).expect("mode");
    let bound = fs::File::create(dir.join("probe")).is_err();
    let refused = run(&disk.flags(true));
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("mode");

    assert!(
        bound,
        "the directory's mode does not bind this user (root?)"
    );
    let refused = refused.expect_err("refused");
    assert!(
        matches!(&refused, MigrateError::Io { path, .. } if *path == marker),
        "{refused:?}"
    );
    assert!(
        refused
            .to_string()
            .starts_with(&format!("{}: ", marker.display()))
    );
    assert_eq!(disk.files(), before);
}

/// Proves: a host record the enrollment store cannot write (its file is a directory) stops the
/// write with the store's own error, the marker stays, and neither the ops file nor the state
/// file is touched; an audit file that is a directory stops it with the marker left and the state
/// file untouched.
///
/// Catches: a stop after the records that goes on to commit the ops and state files, which
/// would leave a store that looks migrated over records half written.
#[test]
fn a_record_or_an_ops_file_that_cannot_be_written_stops_before_the_state_file() {
    let disk = Disk::new("apply-record-write");
    let (moved, made) = planned(&disk);
    let state_text = fs::read(&disk.state).expect("read");
    let ops_text = fs::read(disk.paths().ops_file()).expect("read");
    let host = disk.enrollment.join("hosts").join("w1.pb");
    fs::remove_file(&host).expect("remove");
    fs::create_dir_all(host.join("in")).expect("a directory in its place");

    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);

    assert!(
        matches!(result, Err(MigrateError::Records(_))),
        "{result:?}"
    );
    assert!(migrate_marker(&disk.state).exists());
    assert_eq!(fs::read(&disk.state).expect("read"), state_text);
    assert_eq!(fs::read(disk.paths().ops_file()).expect("read"), ops_text);

    let disk = Disk::new("apply-ops-garbage");
    let (moved, made) = planned(&disk);
    let state_text = fs::read(&disk.state).expect("read");
    let audit = crate::audit_file::path_of(&disk.paths().ops_file());
    fs::remove_file(&audit).ok();
    fs::create_dir(&audit).expect("a directory in its place");

    let result = apply_files(&disk.paths(), &disk.enrollment, None, &moved.loaded, &made);

    assert!(result.is_err());
    assert!(migrate_marker(&disk.state).exists());
    assert_eq!(fs::read(&disk.state).expect("read"), state_text);
}

/// Proves: a dry run over a database that holds no coordinator schema is refused as the
/// database's own refusal, and writes nothing.
///
/// Catches: a dry run that prints a plan over a database it could not read.
#[test]
fn a_dry_run_over_a_database_that_cannot_be_read_is_refused() {
    let disk = Disk::new("apply-pg-unread");
    let database = TestDatabase::new();
    let dsn = disk.dir.path().join("database.dsn");
    fs::write(&dsn, database.dsn_text()).expect("write");
    fs::set_permissions(&dsn, fs::Permissions::from_mode(0o600)).expect("mode");
    let flags = Migrate {
        state_file: None,
        enrollment_dir: None,
        database_dsn_file: Some(dsn),
        ..disk.flags(false)
    };

    let refused = run(&flags);

    assert!(
        matches!(refused, Err(MigrateError::Database(MoveError::Database(_)))),
        "{refused:?}"
    );
}

/// Proves: a request still waiting for its approval names no role, so the plan reads it as no
/// approval to place: it is left as it is, and the plan holds no error.
///
/// Catches: a pending request taken for an approved one, whose missing approval would stop the
/// plan or be given a group.
#[test]
fn a_request_without_an_approval_is_left_as_it_is() {
    let mut store = Store::of(&[("w1", "web")]);
    store.moved.records.requests = vec![pb::Request {
        request_id: vec![9; 32],
        state: pb::RequestState::Pending as i32,
        approved: None,
        ..pb::Request::default()
    }];

    let made = store.plan(ROLLOUT_ONE);

    assert_eq!(made.errors, Vec::<String>::new());
    assert!(made.changed.requests.is_empty());
    assert_eq!(made.after.records.requests, store.moved.records.requests);
}

/// A rollout file of one service, placed on the role `web`.
const ROLLOUT_ONE: &str = "[[service]]\nname = \"kbf\"\nroles = [\"web\"]\n";
