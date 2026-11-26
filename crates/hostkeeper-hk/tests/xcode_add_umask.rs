//! `hk xcode add` under a umask of 000. It is a binary of its own because the test changes the
//! umask of the whole process, and no other test may create a file while it is changed.
//! The rig is the `cli` binary's, included by path; this binary uses only part of it.
#![allow(
    dead_code,
    reason = "the rig is shared with the `cli` binary, which uses all of it; this one a part"
)]

#[path = "cli/add_rig.rs"]
mod add_rig;
#[path = "cli/rig.rs"]
mod rig;
#[path = "cli/verify_rig.rs"]
mod verify_rig;

use std::os::unix::fs::PermissionsExt as _;

use add_rig::{Add, Store, archive_bytes, sha256};

/// Proves: under a umask of 000 the first `hk xcode add` (flags only) makes `~/.hostkeeper` and
/// `~/.hostkeeper/xcode` with mode 0700 and its lock with mode 0600 (the lock as the store sees it
/// while the run holds it, and as it stays afterwards), and no more than that is made there but the
/// record, 0600.
#[test]
fn under_a_umask_of_000_what_it_makes_is_private() {
    let add = Add::new("umask");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let (server, watched) = Store::default().serve_watching(&add.rig, &add.kept(".lock"));
    let ran = {
        let before = rustix::process::umask(rustix::fs::Mode::empty());
        let ran = add.add(&server, &[&path]);
        rustix::process::umask(before);
        ran
    };
    assert_eq!(ran.code, 0, "{ran:?}");
    let mode = |path: std::path::PathBuf| {
        std::fs::metadata(path).expect("made").permissions().mode() & 0o777
    };
    assert_eq!(mode(add.rig.path("home/.hostkeeper")), 0o700);
    assert_eq!(mode(add.rig.path("home/.hostkeeper/xcode")), 0o700);
    assert_eq!(
        mode(add.kept(&format!("{}.json", sha256(&archive_bytes())))),
        0o600
    );
    let modes: Vec<Option<u32>> = watched
        .lock()
        .expect("the record")
        .iter()
        .map(|held| held.as_ref().map(|(_, mode)| *mode))
        .collect();
    assert_eq!(modes, [Some(0o600); 5]);
    // The lock file stays, as private as it was while held.
    assert_eq!(mode(add.kept(".lock")), 0o600);
}
