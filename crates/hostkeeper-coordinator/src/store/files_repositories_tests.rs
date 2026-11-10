//! The repositories file of the file store (`<state-file>.repositories`): written whole before the
//! state file and only when a commit changes the registry, read at load, and refused by name when
//! it is bad. The directory sync is injected where a test needs it to fail; no clock, no timing.

use std::fs;
use std::io;
use std::path::Path;

use super::conformance_tests::desired_at;
use super::{Backend, BackendError, Commit, Files};
use crate::desired::{self, StoreError};
use crate::repositories::Registry;
use crate::repositories_support::komira;
use crate::testutil::{LEGACY, scratch};

/// A state of `kbf` at `2.0` beside a registry that holds `komira`.
fn held_with_repository() -> desired::Held {
    let mut held = desired_at("2.0", 'a');
    held.repositories = Registry::default().put(komira(1)).expect("put");
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

fn files_over(dir: &Path) -> Files {
    Files::default().with_desired(&dir.join("desired.json"), Some(LEGACY))
}

fn loaded(dir: &Path) -> Result<desired::Held, StoreError> {
    files_over(dir).load_desired()
}

fn always_fails(_: &Path) -> io::Result<()> {
    Err(io::Error::other("sync failed"))
}

/// Proves: a commit that changes the registry writes the repositories file beside the state file,
/// both whole, and a load of the two gives the state and the registry back, its generation among
/// it; the state file names no repository.
///
/// Catches: a registry written into the state file, which a coordinator before would refuse.
#[test]
fn a_commit_writes_the_repositories_file_beside_the_state_file_and_load_reads_it() {
    let dir = scratch("files-repositories-written");
    let files = files_over(dir.path());
    let after = held_with_repository();
    commit_of(&files, &desired::Held::default(), &after).expect("committed");
    let text = fs::read_to_string(dir.path().join("desired.json.repositories")).expect("the file");
    assert!(text.starts_with("{\n  \"generation\": 1,\n  \"repositories\": {\n    \"komira\": {"));
    let state = fs::read_to_string(dir.path().join("desired.json")).expect("the file");
    assert!(!state.contains("komira"), "{state}");
    // The state file is what a state without a registry writes, byte for byte: the coordinator
    // before reads it as it always did.
    let plain = scratch("files-repositories-plain");
    commit_of(
        &files_over(plain.path()),
        &desired::Held::default(),
        &desired_at("2.0", 'a'),
    )
    .expect("committed");
    assert_eq!(
        fs::read(plain.path().join("desired.json")).expect("the file"),
        state.as_bytes()
    );
    let held = loaded(dir.path()).expect("loads");
    assert_eq!(held.repositories, after.repositories);
    assert_eq!(held.desired, after.desired);
}

/// Proves: with no repositories file a load holds the empty registry, and a repositories file
/// with no state file gives the registry and no state; a state file that an older coordinator
/// wrote (no repositories file beside it) starts this reader with an empty registry.
///
/// Catches: a start that needs the new file, which the coordinator before never wrote.
#[test]
fn a_load_without_the_repositories_file_holds_the_empty_registry() {
    let dir = scratch("files-repositories-absent");
    assert_eq!(loaded(dir.path()).expect("loads"), desired::Held::default());
    let files = files_over(dir.path());
    commit_of(&files, &desired::Held::default(), &desired_at("2.0", 'a')).expect("committed");
    assert!(!dir.path().join("desired.json.repositories").exists());
    assert_eq!(
        loaded(dir.path()).expect("loads").repositories,
        Registry::default()
    );

    let only = scratch("files-repositories-alone");
    let text =
        crate::repositories::encode_file(&held_with_repository().repositories).expect("encodes");
    fs::write(only.path().join("desired.json.repositories"), text).expect("written");
    let held = loaded(only.path()).expect("loads");
    assert!(held.desired.is_empty());
    assert_eq!(held.repositories, held_with_repository().repositories);
}

/// Proves: a repositories file that is bad is refused at load by the file's name and the reason,
/// and one that cannot be read is an I/O error at its path.
///
/// Catches: a bad file loaded as no registry, which the next put would overwrite.
#[test]
fn a_bad_repositories_file_is_refused_naming_the_file() {
    let dir = scratch("files-repositories-bad");
    let path = dir.path().join("desired.json.repositories");
    fs::write(&path, b"{\"generation\": 1}").expect("written");
    let refused = loaded(dir.path()).expect_err("refused");
    assert!(matches!(refused, StoreError::Invalid { .. }), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: the desired state is malformed: the repositories: no repositories",
            path.display()
        )
    );
    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory where the file goes");
    let unread = loaded(dir.path()).expect_err("unread");
    assert!(matches!(unread, StoreError::Io { .. }), "{unread:?}");
}

/// Proves: a commit that changes nothing of the registry does not write the repositories file
/// again (the file removed after the first write stays removed), and one that changes it writes
/// it again.
///
/// Catches: a file rewritten at every commit of a version.
#[test]
fn a_commit_that_changes_no_repository_leaves_the_repositories_file_alone() {
    let dir = scratch("files-repositories-unchanged");
    let files = files_over(dir.path());
    let first = held_with_repository();
    commit_of(&files, &desired::Held::default(), &first).expect("committed");
    let file = dir.path().join("desired.json.repositories");
    fs::remove_file(&file).expect("removed");

    let mut next = first.clone();
    next.desired.clear();
    commit_of(&files, &first, &next).expect("committed");
    assert!(!file.exists(), "not written again");

    let mut removed = next.clone();
    removed.repositories = next.repositories.remove("komira").expect("removed");
    commit_of(&files, &next, &removed).expect("committed");
    assert!(file.exists());
    assert_eq!(
        loaded(dir.path()).expect("loads").repositories.generation,
        2
    );
}

/// Proves: the repositories file is written before the state file: when it cannot be written the
/// state file is not written and the error is its own; when the state file cannot be written the
/// repositories file is in place already.
///
/// Catches: a state file that is written first, which a crash would leave beside the old registry.
#[test]
fn the_repositories_file_is_written_before_the_state_file() {
    let blocked = scratch("files-repositories-io-first");
    let files = files_over(blocked.path());
    let mut temp = crate::repositories::repositories_path(&blocked.path().join("desired.json"))
        .into_os_string();
    temp.push(".new");
    let temp = std::path::PathBuf::from(temp);
    fs::create_dir(&temp).expect("a directory");
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_repository()).expect_err("io");
    assert!(
        matches!(&refused, BackendError::Desired(StoreError::Io { path, .. }) if *path == temp),
        "{refused:?}"
    );
    assert!(!blocked.path().join("desired.json").exists());

    let second = scratch("files-repositories-io-second");
    let files = files_over(second.path());
    fs::create_dir(second.path().join("desired.json.new")).expect("a directory");
    let refused =
        commit_of(&files, &desired::Held::default(), &held_with_repository()).expect_err("io");
    assert!(
        matches!(&refused, BackendError::Desired(StoreError::Io { .. })),
        "{refused:?}"
    );
    assert!(!second.path().join("desired.json").exists());
    assert!(second.path().join("desired.json.repositories").exists());
}

/// Proves: when the directory sync after the repositories file fails, the state file is written
/// all the same and the commit answers `NotSynced`, every file in place.
///
/// Catches: a state file left unwritten beside a repositories file that is in place.
#[test]
fn an_unsynced_repositories_file_still_writes_the_state_file_and_answers_not_synced() {
    let dir = scratch("files-repositories-unsynced");
    let files = files_over(dir.path()).with_sync_dir(always_fails);
    let refused = commit_of(&files, &desired::Held::default(), &held_with_repository())
        .expect_err("not synced");
    assert!(
        matches!(
            &refused,
            BackendError::Desired(StoreError::NotSynced { .. })
        ),
        "{refused:?}"
    );
    assert!(dir.path().join("desired.json").exists());
    assert!(dir.path().join("desired.json.repositories").exists());
    let held = loaded(dir.path()).expect("loads");
    assert_eq!(held.repositories, held_with_repository().repositories);
    assert_eq!(held.desired, held_with_repository().desired);
}
