//! What the tests of this crate share: a scratch directory a version tree can be removed from.

#![allow(dead_code, reason = "each test binary uses a part of the support")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A scratch directory whose read-only version trees are opened up before it is removed.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        Scratch(
            hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
                .expect("a scratch directory"),
        )
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        open_up(self.0.path());
    }
}

/// Gives every directory below `path` mode 0700 so the tree can be removed.
fn open_up(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            open_up(&entry.path());
        }
    }
}
