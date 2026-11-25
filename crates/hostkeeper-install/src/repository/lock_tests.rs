//! Tests of the installer lock of the repository installer.

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch() -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), "repository-lock").unwrap()
}

/// Proves a dropped `Held` lets the next holder in while another descriptor of its open file
/// still lives, as a child process spawned meanwhile holds a copy of every descriptor until its
/// exec.
///
/// Catches: a lock released only by closing its descriptor, which a concurrent spawn keeps held
/// for the moment the child takes to exec, so the next `try_hold` finds it held with no holder.
#[test]
fn a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch();
    let lock = InstallerLock::new(&dir.path().join("installer.lock"));
    let held = lock.hold().unwrap();
    let copy = held._file.try_clone().unwrap();
    drop(held);
    assert!(
        lock.try_hold().unwrap().is_some(),
        "the dropped lock is free"
    );
    drop(copy);
}
