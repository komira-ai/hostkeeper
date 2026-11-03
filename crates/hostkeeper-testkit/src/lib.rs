//! The test helpers the crates of this workspace share. A dev-dependency only.
//!
//! - [`Scratch`]: a directory a test owns, under a base the test names (its
//!   `CARGO_TARGET_TMPDIR`), removed when it is dropped.
//!
//! No moved test opens a port, so there is no port helper.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A new, empty directory that one test owns, removed when it is dropped.
///
/// Its name is `<name>-<pid>-<n>`, with the process id and a number this process gives each
/// directory, so two tests, or two runs sharing one target directory, never get the same
/// directory and never remove one another's.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    /// Creates `base/<name>-<pid>-<n>`, and `base` when it is missing. A directory that already
    /// exists at that path is an error, never shared.
    pub fn under(base: &Path, name: &str) -> io::Result<Scratch> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(base)?;
        // `create_dir`, not `create_dir_all`: a directory that already exists is an error.
        std::fs::create_dir(&path)?;
        Ok(Scratch(path))
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A directory that cannot be removed is left behind; no later test uses its name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base of these tests: a directory beside the test binary, inside the target directory.
    fn base(name: &str) -> PathBuf {
        let exe = std::env::current_exe().expect("the test binary");
        exe.parent().expect("its directory").join(name)
    }

    /// Catches two scratch directories of one name sharing a directory, so that one dropping
    /// (or one run starting) removes the other's files.
    #[test]
    fn two_scratch_directories_of_one_name_are_separate_and_drop_alone() {
        let base = base("testkit-unit");
        let a = Scratch::under(&base, "same").expect("first");
        let b = Scratch::under(&base, "same").expect("second");
        assert_ne!(a.path(), b.path());
        assert!(a.path().starts_with(&base));
        assert!(a.path().is_dir());
        std::fs::write(b.path().join("kept"), b"x").expect("write");
        let gone = a.path().to_path_buf();
        drop(a);
        assert!(!gone.exists());
        assert_eq!(std::fs::read(b.path().join("kept")).expect("read"), b"x");
    }

    /// Catches a scratch directory made inside something that is not a directory being
    /// reported as made.
    #[test]
    fn a_base_that_is_a_file_is_an_error() {
        let parent = Scratch::under(&base("testkit-unit"), "file-base").expect("parent");
        let file = parent.path().join("a-file");
        std::fs::write(&file, b"x").expect("write");
        let refused = Scratch::under(&file, "inside").expect_err("a file is no base");
        assert_ne!(refused.kind(), io::ErrorKind::NotFound, "{refused}");
    }
}
