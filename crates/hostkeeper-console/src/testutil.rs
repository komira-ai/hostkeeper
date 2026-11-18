//! What the tests of this crate share: a scratch directory of each test's own, and a file in it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_testkit::Scratch;

/// A directory the test owns, under the unit-test base beside the test binary.
pub(crate) fn scratch(name: &str) -> Scratch {
    let base = std::env::current_exe()
        .expect("the test binary")
        .parent()
        .expect("its directory")
        .join("hostkeeper-console-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

/// The file `name` in `dir` holding `bytes`, of mode `mode`.
pub(crate) fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("its mode");
    path
}
