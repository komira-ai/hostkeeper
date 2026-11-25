//! Tests of the lock of `placed.json` that need its file: what a copy of its descriptor does.

use hostkeeper_testkit::Scratch;

use super::*;
use crate::testing::{FakeUnitControl, FaultFs, Log};

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch() -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), "placer-lock").expect("a scratch directory")
}

/// Proves a dropped `PlacedLock` lets the next holder in while another descriptor of its open
/// file still lives, as a child process spawned meanwhile holds a copy of every descriptor until
/// its exec.
///
/// Catches: a lock released only by closing its descriptor, which a concurrent spawn keeps held
/// for the moment the child takes to exec, so the next `lock` fails `Locked` with no holder.
#[test]
fn a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch();
    let roots = Roots::linux();
    let state = roots.state_dir();
    let host_state = dir.path().join(state.strip_prefix("/").unwrap_or(state));
    std::fs::create_dir_all(host_state).expect("the state directory");
    let log = Log::new();
    let fs = FaultFs::new(dir.path(), log.clone());
    let units = FakeUnitControl::new(log);
    let placer = Placer::new(roots, &fs, &units);

    let lock = placer.lock().expect("the lock is free");
    let copy = lock._file.try_clone().expect("a copy of the descriptor");
    drop(lock);
    assert!(placer.lock().is_ok(), "the dropped lock is free");
    drop(copy);
}
