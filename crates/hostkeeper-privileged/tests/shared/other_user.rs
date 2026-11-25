//! Reading a file as another user than root, for the tests that prove what hostkeeper keeps of
//! a package is root's alone: the user is the owner of the build directory (the runner's own
//! user, never root), and the read is `/bin/cat` run as that user, its groups dropped.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The user and group another user's read runs as: the owner of the build directory.
pub fn other_user() -> (u32, u32) {
    let meta = fs::metadata(env!("CARGO_TARGET_TMPDIR")).expect("the build directory");
    assert_ne!(
        meta.uid(),
        0,
        "the build directory is root's: the tests are built as the runner's user and run as root"
    );
    (meta.uid(), meta.gid())
}

/// Whether [`other_user`] can read the bytes of `path`.
pub fn other_user_reads(path: &Path) -> bool {
    let (uid, gid) = other_user();
    Command::new("/bin/cat")
        .arg(path)
        .uid(uid)
        .gid(gid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("cat {} as {uid}: {e}", path.display()))
        .success()
}

/// Every regular file at and below `dir`, links not followed.
pub fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => found.extend(files_below(&path)),
            Ok(meta) if meta.is_file() => found.push(path),
            _ => {}
        }
    }
    found.sort();
    found
}

/// Proves no file below each of `dirs` can be read by [`other_user`], and that this is the
/// directories' doing: a file the test puts beside them, mode 0644, can. Answers how many files
/// were tried, so that a test can see there were some.
pub fn assert_root_only(beside: &Path, dirs: &[&Path]) -> usize {
    let control = beside.join("readable-by-others");
    fs::write(&control, b"x").expect("the control file");
    fs::set_permissions(&control, fs::Permissions::from_mode(0o644)).expect("its mode");
    assert!(
        other_user_reads(&control),
        "{} cannot be read by the other user: the check would prove nothing",
        control.display()
    );
    fs::remove_file(&control).expect("the control file goes");
    let mut tried = 0;
    for dir in dirs {
        for file in files_below(dir) {
            tried += 1;
            assert!(
                !other_user_reads(&file),
                "{} can be read by another user than root",
                file.display()
            );
        }
    }
    tried
}
