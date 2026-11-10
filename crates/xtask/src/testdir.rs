//! A scratch directory a test owns: a directory of its own under a base, removed when it drops.
//!
//! The name holds the process id and a number this process gives each directory. The directory
//! is created exclusively: when one of that name is already there (a run before this one, with
//! the same process id, left it in a target directory the next run reuses), the next number is
//! tried, so two tests, or two runs sharing one target directory, never get the same directory
//! and never remove one another's.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// How many numbers one directory tries before the test fails: far more than the stale
/// directories of one name a reused target directory holds, and few enough that a fault which
/// makes every try look taken fails the test at once instead of hanging it.
const ATTEMPTS: u64 = 1024;

pub struct TestDir(PathBuf);

impl TestDir {
    /// A new, empty directory `base/<name>-<pid>-<n>`, with the first number `n` whose
    /// directory is not there yet.
    pub fn under(base: &Path, name: &str) -> TestDir {
        unique(
            base,
            &format!("{name}-{}", std::process::id()),
            &NEXT,
            ATTEMPTS,
        )
    }
}

/// The new directory `base/<stem>-<n>`, `n` taken from `next`, a fresh number for each try: a
/// directory that already exists is passed over, never shared or emptied; any other error, or
/// `attempts` numbers that were all taken, fails the test.
fn unique(base: &Path, stem: &str, next: &AtomicU64, attempts: u64) -> TestDir {
    std::fs::create_dir_all(base).expect("create the scratch base");
    for _ in 0..attempts {
        let path = base.join(format!("{stem}-{}", next.fetch_add(1, Ordering::Relaxed)));
        // `create_dir`, not `create_dir_all`: a directory that already exists is an error, so
        // no test ever shares one.
        match std::fs::create_dir(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            made => {
                made.expect("create the scratch directory");
                return TestDir(path);
            }
        }
    }
    panic!(
        "no free scratch directory {}-<n> in {attempts} numbers",
        base.join(stem).display()
    );
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

    /// Catches: a stale directory of the same name (left by a run before this one with the same
    /// process id) failing the new one, or being shared or emptied.
    #[test]
    fn a_stale_directory_of_the_same_name_is_passed_over_and_left_alone() {
        let exe = std::env::current_exe().expect("test binary");
        let parent = TestDir::under(&exe.parent().expect("deps").join("testdir-unit"), "stale");
        for stale in ["left-0", "left-1"] {
            std::fs::create_dir(parent.join(stale)).expect("a stale directory");
            std::fs::write(parent.join(stale).join("kept"), stale).expect("write");
        }
        let next = AtomicU64::new(0);
        let fresh = unique(&parent, "left", &next, ATTEMPTS);
        assert_eq!(fresh.as_ref(), parent.join("left-2"));
        assert_eq!(next.load(Ordering::Relaxed), 3);
        for stale in ["left-0", "left-1"] {
            let kept = std::fs::read(parent.join(stale).join("kept")).expect("read");
            assert_eq!(kept, stale.as_bytes());
        }
    }

    /// Catches: parallel callers of one name sharing a directory, or failing because another
    /// made the directory first.
    #[test]
    fn parallel_callers_of_one_name_each_get_their_own_directory() {
        const CALLERS: usize = 16;
        let exe = std::env::current_exe().expect("test binary");
        let parent = TestDir::under(&exe.parent().expect("deps").join("testdir-unit"), "par");
        std::fs::create_dir(parent.join("raced-0")).expect("a stale directory");
        let next = AtomicU64::new(0);
        let made: Vec<TestDir> = std::thread::scope(|scope| {
            let callers: Vec<_> = (0..CALLERS)
                .map(|_| {
                    let (parent, next) = (&*parent, &next);
                    scope.spawn(move || unique(parent, "raced", next, ATTEMPTS))
                })
                .collect();
            callers
                .into_iter()
                .map(|c| c.join().expect("the caller"))
                .collect()
        });
        let paths: std::collections::BTreeSet<&Path> = made.iter().map(AsRef::as_ref).collect();
        assert_eq!(paths.len(), CALLERS, "{paths:?}");
        assert!(!paths.contains(parent.join("raced-0").as_path()));
        assert!(paths.iter().all(|p| p.is_dir()));
    }

    /// Catches: a search that runs without end when every number it tries is taken; it fails
    /// after its attempts, having taken exactly that many numbers.
    #[test]
    fn a_search_whose_every_number_is_taken_fails_after_its_attempts() {
        let exe = std::env::current_exe().expect("test binary");
        let parent = TestDir::under(&exe.parent().expect("deps").join("testdir-unit"), "full");
        for taken in ["full-0", "full-1", "full-2"] {
            std::fs::create_dir(parent.join(taken)).expect("a taken directory");
        }
        let next = AtomicU64::new(0);
        let failed = std::panic::catch_unwind(|| unique(&parent, "full", &next, 3));
        // `err().expect`, as `TestDir` has no `Debug` for `expect_err` to print.
        let said = failed
            .err()
            .expect("no directory is made when every number is taken");
        let said = said.downcast_ref::<String>().cloned().unwrap_or_default();
        assert_eq!(
            said,
            format!(
                "no free scratch directory {}-<n> in 3 numbers",
                parent.join("full").display()
            )
        );
        assert_eq!(next.load(Ordering::Relaxed), 3);
    }

    /// Catches: an error other than an existing directory being retried with the next number
    /// (a search that never ends) or reported as made.
    #[test]
    fn an_error_other_than_an_existing_directory_fails_at_the_first_number() {
        let exe = std::env::current_exe().expect("test binary");
        let parent = TestDir::under(&exe.parent().expect("deps").join("testdir-unit"), "long");
        let next = AtomicU64::new(0);
        let name = "n".repeat(300);
        let failed = std::panic::catch_unwind(|| unique(&parent, &name, &next, ATTEMPTS));
        assert!(failed.is_err());
        assert_eq!(next.load(Ordering::Relaxed), 1);
    }
}
