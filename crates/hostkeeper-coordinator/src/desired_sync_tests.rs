//! A desired state whose file is in place but whose directory could not be synced, and one
//! whose file could not be replaced at all: what the caller is told, what memory holds, what
//! the file holds, and what a store reopened from the file holds.

use std::fs;
use std::io;
use std::path::Path;

use crate::desired::{DesiredStore, StoreError};
use crate::testutil::{LEGACY, desired, scratch};

fn failing_sync(_: &Path) -> io::Result<()> {
    Err(io::Error::other("sync failed"))
}

/// The version the file at `path` holds, read by a store opened from it.
fn reopened(path: &Path) -> Option<String> {
    DesiredStore::open(path, Some(LEGACY))
        .expect("a store")
        .get(LEGACY)
        .map(|d| d.version.to_string())
}

/// Catches: a set whose directory sync failed after the rename that kept the old state in
/// memory while the file held the new one. The caller is told the state was written but not
/// synced, memory and file both hold the new state, a store reopened from the file holds it, and
/// setting the same state again succeeds.
#[test]
fn a_sync_failure_after_the_rename_updates_memory_and_reports_not_synced() {
    let dir = scratch("desired-sync-after");
    let path = dir.path().join("desired.json");
    DesiredStore::open(&path, Some(LEGACY))
        .expect("a store")
        .set(LEGACY, desired("1.0", 0, 0))
        .expect("the first state");
    let store = DesiredStore::open(&path, Some(LEGACY))
        .expect("a store")
        .with_sync_dir(failing_sync);
    let err = store
        .set(LEGACY, desired("2.0", 0, 0))
        .expect_err("not synced");
    assert!(matches!(err, StoreError::NotSynced { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!(
            "{}: written, but the directory was not synced: sync failed",
            dir.path().display()
        )
    );
    let in_memory = store.get(LEGACY).expect("a state");
    assert_eq!(in_memory.version.to_string(), "2.0");
    let on_disk = fs::read_to_string(&path).expect("the file");
    assert!(on_disk.contains("\"2.0\""), "{on_disk}");
    assert_eq!(reopened(&path).as_deref(), Some("2.0"));
    assert!(!dir.path().join("desired.json.new").exists());
    DesiredStore::open(&path, Some(LEGACY))
        .expect("a store")
        .set(LEGACY, desired("2.0", 0, 0))
        .expect("the same state again");
    assert_eq!(reopened(&path).as_deref(), Some("2.0"));
}

/// Catches: a set that failed before the rename changing memory or the file. The caller is
/// told which file failed, memory and the file keep the old state, and a store reopened from
/// the file holds it.
#[test]
fn a_failure_before_the_rename_keeps_the_old_state_in_memory_and_on_disk() {
    let dir = scratch("desired-sync-before");
    let path = dir.path().join("desired.json");
    let store = DesiredStore::open(&path, Some(LEGACY)).expect("a store");
    store
        .set(LEGACY, desired("1.0", 0, 0))
        .expect("the first state");
    let before = fs::read(&path).expect("the file");
    // A directory where the temporary file goes: it can be neither removed as a file nor
    // created.
    let temp = dir.path().join("desired.json.new");
    fs::create_dir(&temp).expect("a directory");
    let err = store
        .set(LEGACY, desired("2.0", 0, 0))
        .expect_err("not written");
    assert!(matches!(err, StoreError::Io { .. }), "{err:?}");
    assert!(
        err.to_string().starts_with(&temp.display().to_string()),
        "{err}"
    );
    let in_memory = store.get(LEGACY).expect("a state");
    assert_eq!(in_memory.version.to_string(), "1.0");
    assert_eq!(fs::read(&path).expect("the file"), before);
    assert_eq!(reopened(&path).as_deref(), Some("1.0"));
}
