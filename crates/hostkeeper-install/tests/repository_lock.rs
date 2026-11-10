//! The repository installer's host-wide lock: `try_hold` takes it when it is free and never
//! waits when it is not.

use std::path::Path;

use hostkeeper_install::repository::InstallerLock;
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// Proves `try_hold` answers `None` while another holder has the lock, at once, and takes it
/// once the holder lets go.
#[test]
fn try_hold_never_waits_for_a_held_lock() {
    let dir = scratch("repository-lock");
    let file = dir.path().join("installer.lock");
    let first = InstallerLock::new(&file);
    let second = InstallerLock::new(&file);
    let held = first.hold().expect("the lock");
    assert!(second.try_hold().expect("an answer").is_none());
    drop(held);
    assert!(second.try_hold().expect("an answer").is_some());
}

/// Proves a lock file that cannot be opened is an error, not an answer of `None`.
#[test]
fn try_hold_on_a_file_that_cannot_be_opened_is_an_error() {
    let dir = scratch("repository-lock-open");
    let lock = InstallerLock::new(&dir.path().join("missing").join("installer.lock"));
    assert!(lock.try_hold().is_err());
}
