//! Shared by the tests: a scratch directory and the uid of the account the tests run as.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub mod package;

/// A directory under the target directory, unique to the test, removed when it is dropped.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let dir = hostkeeper_testkit::Scratch::under(base, &format!("hostkeeper-config-{name}"))
            .expect("create the scratch directory");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("set the mode");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// A file of this mode, whatever the umask, with its directories.
    pub fn file(&self, name: &str, text: &str, mode: u32) -> PathBuf {
        let path = self.0.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the directories");
        }
        let _ = fs::remove_file(&path);
        fs::write(&path, text).expect("write the file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set the mode");
        path
    }

    /// A directory of this mode, whatever the umask.
    pub fn dir(&self, name: &str, mode: u32) -> PathBuf {
        let path = self.0.path().join(name);
        fs::create_dir_all(&path).expect("create the directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set the mode");
        path
    }
}

/// The uid the tests run as: the owner of what they create.
pub fn my_uid() -> u32 {
    fs::metadata(env!("CARGO_TARGET_TMPDIR"))
        .expect("the target directory")
        .uid()
}

/// Makes a named pipe at `path` with the system's `mkfifo` program (no shell).
pub fn fifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo {}", path.display());
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("set the mode");
}
