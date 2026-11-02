//! A scratch directory a test owns: a directory of its own under a base, removed when it drops.
//!
//! The name holds the process id and a number this process gives each directory, so two tests,
//! or two runs sharing one target directory, never get the same directory and never remove one
//! another's.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TestDir(PathBuf);

impl TestDir {
    /// A new, empty directory `base/<name>-<pid>-<n>`.
    pub fn under(base: &Path, name: &str) -> TestDir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(base).expect("create the scratch base");
        // `create_dir`, not `create_dir_all`: a directory that already exists is an error, so
        // no test ever shares one.
        std::fs::create_dir(&path).expect("create the scratch directory");
        TestDir(path)
    }
}

impl Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // A directory that cannot be removed is left behind; no later test uses its name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches: two fixtures of the same name sharing a directory, so that one dropping (or one
    /// run starting) removes the other's files.
    #[test]
    fn two_scratch_directories_of_one_name_are_separate_and_drop_alone() {
        let exe = std::env::current_exe().expect("test binary");
        let base = exe.parent().expect("deps").join("testdir-unit");
        let a = TestDir::under(&base, "same");
        let b = TestDir::under(&base, "same");
        assert_ne!(a.as_ref(), b.as_ref());
        std::fs::write(b.join("kept"), b"x").expect("write");
        let gone = a.to_path_buf();
        drop(a);
        assert!(!gone.exists());
        assert_eq!(std::fs::read(b.join("kept")).expect("read"), b"x");
    }
}
