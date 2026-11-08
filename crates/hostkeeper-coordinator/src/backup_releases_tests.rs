//! `backup` and `restore` over the releases file a coordinator keeps beside its state file
//! (`<state-file>.releases`, docs/design/packages.md section 3.6).

use std::path::Path;

use crate::backup::{StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::desired::DesiredStore;
use crate::lock::FileLock;
use crate::releases::releases_path;
use crate::store::conformance_tests::desired_with_release;
use crate::testutil::scratch;
use crate::write_record::WriteAsk;

/// Sets the version `2.0` of `kbf` with a release in the file store at `state`.
fn set_with_release(state: &Path) {
    let held = desired_with_release("2.0", 'c');
    let release = held.releases["kbf"]["2.0"].clone();
    let store = DesiredStore::open(state, None).expect("a store");
    store
        .record_as(
            "kbf",
            held.desired["kbf"].clone(),
            |_| false,
            &WriteAsk::default(),
            Some(release),
        )
        .expect("the set commits");
}

/// Proves: a backup of a state that holds a releases file copies it into the snapshot as
/// `state-releases`, byte for byte, and a restore into empty targets puts it back beside the
/// restored state file, where a store opened over it holds the same release.
///
/// Catches: a release left out of a backup, so a restored coordinator would answer
/// `release_unknown` for a version it was set with.
#[test]
fn a_backup_copies_the_releases_file_and_a_restore_puts_it_back() {
    let dir = scratch("backup-releases");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    set_with_release(&state);
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let written = std::fs::read(releases_path(&state)).expect("the releases file");

    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let held = files_under(&snapshot);
    assert_eq!(held.get("state-releases"), Some(&written));
    assert!(held.contains_key("state"));

    let target = scratch("backup-releases-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(
        std::fs::read(releases_path(&restored.state_file)).expect("the restored file"),
        written
    );
    let again = DesiredStore::open(&restored.state_file, None).expect("a store");
    let want = desired_with_release("2.0", 'c');
    assert_eq!(
        *again.release("kbf", "2.0").expect("the release"),
        *want.releases["kbf"]["2.0"]
    );
}

/// Proves: a state with no releases file backs up and restores without one: the snapshot holds no
/// `state-releases` and the restored targets hold no releases file.
///
/// Catches: an empty releases file invented by a backup or a restore.
#[test]
fn a_state_without_releases_is_backed_up_without_a_releases_file() {
    let dir = scratch("backup-no-releases");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    let held = desired_with_release("2.0", 'c');
    DesiredStore::open(&state, None)
        .expect("a store")
        .record_as(
            "kbf",
            held.desired["kbf"].clone(),
            |_| false,
            &WriteAsk::default(),
            None,
        )
        .expect("a plain set");
    assert!(!releases_path(&state).exists());
    let paths = StatePaths {
        state_file: state,
        ops_file: None,
        enrollment_dir: None,
    };
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert!(!files_under(&snapshot).contains_key("state-releases"));
    let target = scratch("backup-no-releases-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert!(!releases_path(&restored.state_file).exists());
}
