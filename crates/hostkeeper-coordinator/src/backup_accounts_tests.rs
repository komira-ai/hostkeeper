//! `backup` and `restore` over the accounts file a coordinator keeps beside its state file
//! (`<state-file>.accounts`, docs/design/operator-login.md section 9).

use std::path::Path;

use crate::accounts::{AccountStore, accounts_path};
use crate::backup::{StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::desired::DesiredStore;
use crate::lock::FileLock;
use crate::store::conformance_tests::{desired_at, rich_accounts};
use crate::testutil::scratch;
use crate::write_record::WriteAsk;

/// Puts the rich accounts into the accounts file beside the state file `state`, committed.
pub(crate) fn put_accounts(state: &Path) {
    let held = rich_accounts();
    AccountStore::open(&accounts_path(state))
        .expect("a store")
        .change(|_| Ok(held))
        .expect("committed");
}

/// Proves: a backup of a state that holds an accounts file copies it into the snapshot as
/// `state-accounts`, byte for byte, and a restore into empty targets puts it back beside the
/// restored state file, where a store opened over it holds the same users and tokens.
///
/// Catches: users and API tokens left out of a backup, so a restored coordinator would have
/// nobody to sign in until an admin ran the deploy's break-glass again.
#[test]
fn a_backup_copies_the_accounts_file_and_a_restore_puts_it_back() {
    let dir = scratch("backup-accounts");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    put_accounts(&state);
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let written = std::fs::read(accounts_path(&state)).expect("the accounts file");

    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(files_under(&snapshot).get("state-accounts"), Some(&written));

    let target = scratch("backup-accounts-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(
        std::fs::read(accounts_path(&restored.state_file)).expect("the restored file"),
        written
    );
    let again = AccountStore::open(&accounts_path(&restored.state_file)).expect("a store");
    assert_eq!(again.snapshot(), rich_accounts());
}

/// Proves: a state that holds no accounts is backed up and restored without an accounts file.
///
/// Catches: an empty accounts file invented by a backup or a restore.
#[test]
fn a_state_without_accounts_is_backed_up_without_an_accounts_file() {
    let dir = scratch("backup-no-accounts");
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
    assert!(!held.contains_key("state-accounts"));
}
