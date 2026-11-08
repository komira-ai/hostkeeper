//! The releases file of the file store (`<state-file>.releases`): written whole before the state
//! file and only when a commit changes a release, read at load, and refused by name when it is
//! bad. The directory sync is injected where a test needs it to fail; no clock, no timing.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use super::conformance_tests::desired_at;
use super::{Backend, BackendError, Commit, Files};
use crate::desired::{self, StoreError};
use crate::releases::{Book, Release, Targets};
use crate::releases_tests::{digest, entry, list};
use crate::testutil::{LEGACY, scratch};

/// A state of `kbf` at `2.0` that holds a release of one `tarball` entry for the version.
fn held_with_release() -> desired::Held {
    let mut held = desired_at("2.0", 'a');
    let targets = Targets::from([(
        "linux-x86_64".to_owned(),
        list(vec![entry("kbf", "tarball", 'c', 3_000)]),
    )]);
    let release = Arc::new(Release::new(targets, Some(digest('9'))));
    held.releases
        .insert(LEGACY.to_owned(), Book::from([("2.0".to_owned(), release)]));
    held
}

fn commit_of(
    files: &Files,
    before: &desired::Held,
    after: &desired::Held,
) -> Result<(), BackendError> {
    files.commit(&Commit {
        desired: Some((before, after)),
        ..Commit::default()
    })
}

thread_local! {
    /// How many directory syncs `fails_first` has been asked for in this test's thread.
    static SYNCS: Cell<usize> = const { Cell::new(0) };
}

/// A directory sync that fails on its first call in the thread and succeeds on the next ones.
fn fails_first(_: &Path) -> io::Result<()> {
    let calls = SYNCS.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    if calls == 1 {
        Err(io::Error::other("sync failed"))
    } else {
        Ok(())
    }
}

/// A directory sync that always fails.
fn always_fails(_: &Path) -> io::Result<()> {
    Err(io::Error::other("sync failed"))
}

/// The files over `dir`'s `desired.json`.
fn files_over(dir: &Path) -> Files {
    Files::default().with_desired(&dir.join("desired.json"), Some(LEGACY))
}

fn loaded(dir: &Path) -> Result<desired::Held, StoreError> {
    Files::default()
        .with_desired(&dir.join("desired.json"), Some(LEGACY))
        .load_desired()
}

/// Proves: a commit that adds a release writes the releases file beside the state file, both
/// whole, and a load of the two gives the state and the release back.
#[test]
fn a_commit_writes_the_releases_file_beside_the_state_file_and_load_reads_it() {
    let dir = scratch("files-releases-written");
    let files = files_over(dir.path());
    let after = held_with_release();
    commit_of(&files, &desired::Held::default(), &after).expect("committed");
    assert!(dir.path().join("desired.json").exists());
    let text = fs::read_to_string(dir.path().join("desired.json.releases")).expect("the file");
    assert!(text.starts_with("{\n  \"services\": {\n    \"kbf\": {\n      \"2.0\": {"));
    let state = fs::read_to_string(dir.path().join("desired.json")).expect("the file");
    assert!(
        !state.contains("releases") && !state.contains("tarball"),
        "{state}"
    );
    let held = loaded(dir.path()).expect("loads");
    assert_eq!(held.releases, after.releases);
    assert_eq!(held.desired, after.desired);
}

/// Proves: with no releases file a load holds no releases, and a load of a releases file with no
/// state file holds the releases and no state.
#[test]
fn a_load_without_the_releases_file_holds_no_releases() {
    let dir = scratch("files-releases-absent");
    assert_eq!(loaded(dir.path()).expect("loads"), desired::Held::default());
    let files = files_over(dir.path());
    commit_of(&files, &desired::Held::default(), &desired_at("2.0", 'a')).expect("committed");
    assert!(!dir.path().join("desired.json.releases").exists());
    assert!(loaded(dir.path()).expect("loads").releases.is_empty());

    let only = scratch("files-releases-alone");
    let text = crate::releases::encode_file(&held_with_release().releases).expect("encodes");
    fs::write(only.path().join("desired.json.releases"), text).expect("written");
    let held = loaded(only.path()).expect("loads");
    assert!(held.desired.is_empty());
    assert_eq!(held.releases, held_with_release().releases);
}

/// Proves: a releases file that is bad is refused at load by the file's name and the reason, a
/// releases file that cannot be read is an I/O error at its path, and a bad state file is still
/// refused as before.
#[test]
fn a_bad_releases_file_is_refused_naming_the_file() {
    let dir = scratch("files-releases-bad");
    let path = dir.path().join("desired.json.releases");
    fs::write(&path, b"{\"services\": 5}").expect("written");
    let refused = loaded(dir.path()).expect_err("refused");
    assert!(matches!(refused, StoreError::Invalid { .. }), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: the desired state is malformed: the releases: no services",
            path.display()
        )
    );

    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory where the file goes");
    let unread = loaded(dir.path()).expect_err("unread");
    assert!(matches!(unread, StoreError::Io { .. }), "{unread:?}");
    assert!(
        unread.to_string().starts_with(&path.display().to_string()),
        "{unread}"
    );

    let bad_state = scratch("files-releases-bad-state");
    fs::write(bad_state.path().join("desired.json"), b"[]").expect("written");
    let refused = loaded(bad_state.path()).expect_err("refused");
    assert!(refused.to_string().contains("desired.json"), "{refused}");
}

/// Proves: a commit that changes only the limits of the state does not write the releases file
/// again (the file removed after the first write stays removed), and one that changes a release
/// writes it again.
#[test]
fn a_commit_that_changes_no_release_leaves_the_releases_file_alone() {
    let dir = scratch("files-releases-unchanged");
    let files = files_over(dir.path());
    let first = held_with_release();
    commit_of(&files, &desired::Held::default(), &first).expect("committed");
    let releases = dir.path().join("desired.json.releases");
    fs::remove_file(&releases).expect("removed");

    let mut limits = first.clone();
    limits.desired.insert(
        LEGACY.to_owned(),
        desired_at("2.0", 'a').desired[LEGACY].clone(),
    );
    limits.digests.clear();
    commit_of(&files, &first, &limits).expect("committed");
    assert!(!releases.exists(), "not written again");
    assert!(dir.path().join("desired.json").exists());

    let mut changed = limits.clone();
    changed.releases.clear();
    commit_of(&files, &limits, &changed).expect("committed");
    assert!(releases.exists(), "a dropped release is a change");
    let held = loaded(dir.path()).expect("loads");
    assert!(held.releases.is_empty());
}

/// Proves: the releases file is written before the state file: when the releases file cannot be
/// written (an I/O error) the state file is not written and the error is the releases file's; when
/// the state file cannot be written the releases file is in place already.
#[test]
fn the_releases_file_is_written_before_the_state_file() {
    let blocked = scratch("files-releases-io-first");
    let files = files_over(blocked.path());
    // A directory where the releases file's temporary file goes.
    let mut temp =
        crate::releases::releases_path(&blocked.path().join("desired.json")).into_os_string();
    temp.push(".new");
    let temp = std::path::PathBuf::from(temp);
    fs::create_dir(&temp).expect("a directory");
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_release()).expect_err("io");
    assert!(
        matches!(&refused, BackendError::Desired(StoreError::Io { path, .. }) if *path == temp),
        "{refused:?}"
    );
    assert!(!blocked.path().join("desired.json").exists());
    assert!(!blocked.path().join("desired.json.releases").exists());

    let second = scratch("files-releases-io-second");
    let files = files_over(second.path());
    fs::create_dir(second.path().join("desired.json.new")).expect("a directory");
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_release()).expect_err("io");
    assert!(
        matches!(&refused, BackendError::Desired(StoreError::Io { .. })),
        "{refused:?}"
    );
    assert!(!second.path().join("desired.json").exists());
    assert!(
        second.path().join("desired.json.releases").exists(),
        "the release is in place before the state"
    );
}

/// Proves: when the directory sync after the releases file fails, the state file is written all
/// the same and the commit answers `NotSynced` (both files in place); when the sync fails after
/// both, the state file's `NotSynced` is the answer.
#[test]
fn an_unsynced_releases_file_still_writes_the_state_file_and_answers_not_synced() {
    let dir = scratch("files-releases-unsynced");
    SYNCS.with(|n| n.set(0));
    let files = files_over(dir.path()).with_sync_dir(fails_first);
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_release()).expect_err("not synced");
    assert!(
        matches!(
            &refused,
            BackendError::Desired(StoreError::NotSynced { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: written, but the directory was not synced: sync failed",
            dir.path().display()
        )
    );
    assert!(dir.path().join("desired.json").exists());
    assert!(dir.path().join("desired.json.releases").exists());
    let held = loaded(dir.path()).expect("loads");
    assert_eq!(held.releases, held_with_release().releases);
    assert_eq!(held.desired, held_with_release().desired);

    let both = scratch("files-releases-unsynced-both");
    let files = files_over(both.path()).with_sync_dir(always_fails);
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_release()).expect_err("not synced");
    assert!(
        matches!(
            &refused,
            BackendError::Desired(StoreError::NotSynced { .. })
        ),
        "{refused:?}"
    );
    assert!(both.path().join("desired.json").exists());
    assert!(both.path().join("desired.json.releases").exists());
}
