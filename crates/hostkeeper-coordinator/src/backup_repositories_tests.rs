//! `backup` and `restore` over the repositories file a coordinator keeps beside its state file
//! (`<state-file>.repositories`, docs/design/packages-repositories.md section 3).

use std::path::Path;

use crate::backup::{StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::desired::DesiredStore;
use crate::lock::FileLock;
use crate::repositories::repositories_path;
use crate::repositories_support::komira;
use crate::store::Commit;
use crate::store::conformance_tests::desired_at;
use crate::testutil::scratch;
use crate::write_record::WriteAsk;

/// Puts `komira` into the registry of the file store at `state`, committed.
pub(crate) fn put_komira(state: &Path) {
    let store = DesiredStore::open(state, None).expect("a store");
    let staged = store
        .stage_repositories(|held| held.put(komira(1)))
        .expect("staged");
    let part = staged.part().expect("a change");
    staged
        .backend()
        .commit(&Commit {
            desired: Some(part),
            ..Commit::default()
        })
        .expect("committed");
    staged.finish();
}

/// Proves: a backup of a state that holds a repositories file copies it into the snapshot as
/// `state-repositories`, byte for byte, and a restore into empty targets puts it back beside the
/// restored state file, where a store opened over it holds the same registry.
///
/// Catches: a registry left out of a backup, so a restored coordinator would send its hosts no
/// repository until an operator put every one again.
#[test]
fn a_backup_copies_the_repositories_file_and_a_restore_puts_it_back() {
    let dir = scratch("backup-repositories");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    put_komira(&state);
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let written = std::fs::read(repositories_path(&state)).expect("the repositories file");

    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(
        files_under(&snapshot).get("state-repositories"),
        Some(&written)
    );

    let target = scratch("backup-repositories-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(
        std::fs::read(repositories_path(&restored.state_file)).expect("the restored file"),
        written
    );
    let again = DesiredStore::open(&restored.state_file, None).expect("a store");
    let held = again.with_repositories(Clone::clone);
    assert_eq!(held.generation, 1);
    assert_eq!(
        held.get("komira").map(|k| k.repository.clone()),
        Some(komira(1))
    );
}

/// Proves: a state that holds no registry is backed up and restored without a repositories file.
///
/// Catches: an empty repositories file invented by a backup or a restore.
#[test]
fn a_state_without_a_registry_is_backed_up_without_a_repositories_file() {
    let dir = scratch("backup-no-repositories");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    let held = desired_at("2.0", 'c');
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
    let paths = StatePaths {
        state_file: state,
        ops_file: None,
        enrollment_dir: None,
    };
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let held = files_under(&snapshot);
    assert!(held.contains_key("state"));
    assert!(!held.contains_key("state-repositories"));
}
