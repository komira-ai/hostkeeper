//! `backup` and `restore` over the layout file a coordinator keeps beside its state file
//! (`<state-file>.layout`).

use std::path::Path;

use crate::backup::{StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::layout::{LayoutStore, layout_path};
use crate::lock::FileLock;
use crate::store::conformance_tests::rich_layout;
use crate::testutil::scratch;

/// Puts the rich layout into the layout file beside the state file `state`, committed.
pub(crate) fn put_layout(state: &Path) {
    let held = rich_layout();
    LayoutStore::open(&layout_path(state))
        .expect("a store")
        .change(|_| Ok((held, ())))
        .expect("committed");
}

/// Proves: a backup of a state that holds a layout file copies it into the snapshot as
/// `state-layout`, byte for byte, a restore into empty targets puts it back beside the restored
/// state file where a store opened over it holds the same layout.
///
/// Catches: a lab layout left out of a backup, so a restored coordinator would forget where every
/// host stands.
#[test]
fn a_backup_copies_the_layout_file_and_a_restore_puts_it_back() {
    let dir = scratch("backup-layout");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    put_layout(&state);
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let written = std::fs::read(layout_path(&state)).expect("the layout file");

    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(files_under(&snapshot).get("state-layout"), Some(&written));

    let target = scratch("backup-layout-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(
        std::fs::read(layout_path(&restored.state_file)).expect("the restored file"),
        written
    );
    let again = LayoutStore::open(&layout_path(&restored.state_file)).expect("a store");
    assert_eq!(again.snapshot(), rich_layout());
}
