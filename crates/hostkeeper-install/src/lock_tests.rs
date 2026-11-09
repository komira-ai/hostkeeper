//! Tests of the installer lock.

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch() -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), "lock").unwrap()
}

/// Catches: a second holder let in while the first still holds the lock.
#[test]
fn a_second_lock_on_the_same_file_cannot_be_taken_while_held() {
    let dir = scratch();
    let file = dir.path().join("installer.lock");
    let first = InstallerLock::new(&file);
    let second = InstallerLock::new(&file);
    let held = first.hold().unwrap();
    assert!(second.try_hold().unwrap().is_none());
    drop(held);
    assert!(second.try_hold().unwrap().is_some());
}

#[test]
fn an_unheld_lock_is_taken_and_the_file_is_created() {
    let dir = scratch();
    let file = dir.path().join("installer.lock");
    assert!(!file.exists());
    let lock = InstallerLock::new(&file);
    let held = lock.try_hold().unwrap();
    assert!(held.is_some());
    assert!(file.is_file());
}

#[test]
fn a_lock_whose_file_cannot_be_opened_is_an_error() {
    let dir = scratch();
    let lock = InstallerLock::new(&dir.path().join("missing").join("installer.lock"));
    assert!(lock.hold().is_err());
    assert!(lock.try_hold().is_err());
}
