//! The test helpers the crates of this workspace share. A dev-dependency only.
//!
//! - [`Scratch`]: a directory a test owns, under a base the test names (its
//!   `CARGO_TARGET_TMPDIR`), removed when it is dropped.
//!
//! - [`HookMode`]: the exit code and last line a fixture hook ends with, for each behaviour a
//!   test can pick.
//!
//! - [`fifo`]: a named pipe at a path, on Linux and on macOS alike: the file that is neither a
//!   regular file, a directory nor a link, which the tests of what refuses such a file make.
//!
//! - [`postgres_url`]: the URL of the Postgres server the coordinator's store tests run against,
//!   from the one variable CI's job sets: the one read of the environment in the tests, and none
//!   in any product code.
//!
//! No moved test opens a port, so there is no port helper.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The variable that names the tests' Postgres server, as `postgres://<user>@<host>:<port>/<db>`.
pub const POSTGRES_URL_VAR: &str = "HOSTKEEPER_TEST_POSTGRES_URL";

/// The URL of the tests' Postgres server ([`POSTGRES_URL_VAR`]).
///
/// # Panics
/// When the variable is not set or empty: a test that needs the server fails, saying how to
/// start one; it never skips.
#[must_use]
pub fn postgres_url() -> String {
    postgres_url_of(postgres_url_var())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the tests' Postgres server is named by CI's job in one variable, read here and \
              nowhere else; no product code reads the environment (AGENTS.md S2), and a test \
              binary takes no flags of its own to name it by"
)]
fn postgres_url_var() -> Option<String> {
    std::env::var(POSTGRES_URL_VAR).ok()
}

/// The URL `value` names: what [`postgres_url`] answers for the variable's value.
///
/// # Panics
/// When `value` is none or blank.
#[must_use]
pub fn postgres_url_of(value: Option<String>) -> String {
    match value {
        Some(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "{POSTGRES_URL_VAR} names no Postgres server: start one (for example `docker run \
             --rm -p 5432:5432 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16`) and set it to its \
             URL, as `postgres://postgres@127.0.0.1:5432/postgres`"
        ),
    }
}

/// Makes a named pipe at `path` with the permission bits `mode` (as the umask leaves them). An
/// error when `path` exists or its directory does not. rustix's `mknodat` and `mkfifoat` are not
/// built for macOS; this is the one way the workspace's tests make a FIFO on both.
pub fn fifo(path: &Path, mode: u16) -> io::Result<()> {
    let mode = nix::sys::stat::Mode::from_bits_truncate(nix::sys::stat::mode_t::from(mode));
    nix::unistd::mkfifo(path, mode).map_err(io::Error::from)
}

/// The fixture hook's behaviours a test can pick.
pub enum HookMode {
    /// `prepare` defers because the host holds live leases it will not cut: exit `75`, class
    /// `environment`, reason `leases_live`, a retry in a minute.
    DeferLeasesLive,
}

impl HookMode {
    /// The exit code the hook ends with.
    pub fn exit_code(&self) -> i32 {
        match self {
            HookMode::DeferLeasesLive => 75,
        }
    }

    /// The last line the hook writes to standard output.
    pub fn last_line(&self) -> &'static str {
        match self {
            HookMode::DeferLeasesLive => {
                r#"{"class":"environment","reason":"leases_live","retry_after_secs":60}"#
            }
        }
    }
}

/// A new, empty directory that one test owns, removed when it is dropped.
///
/// Its name is `<name>-<pid>-<n>`, with the process id and a number this process gives each
/// directory, both in base 36 to keep paths short (a unix socket's path must fit in 108 bytes),
/// so two tests, or two runs sharing one target directory, never get the same directory and
/// never remove one another's.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    /// Creates `base/<name>-<pid>-<n>`, and `base` when it is missing. A directory that already
    /// exists at that path is an error, never shared. `name` must stay one plain component of
    /// a path inside `base`: an empty name, or one holding a `/` (an absolute path, `..`
    /// above it, or a deeper path), is refused with `InvalidInput` before anything is made.
    pub fn under(base: &Path, name: &str) -> io::Result<Scratch> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = base36(u64::from(std::process::id()));
        if name.is_empty() || name.contains('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a scratch name is one plain path component, not {name:?}"),
            ));
        }
        make(base, &format!("{name}-{pid}-{}", base36(n)))
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Creates `base`, when it is missing, and the new directory `base/<leaf>`.
fn make(base: &Path, leaf: &str) -> io::Result<Scratch> {
    let path = base.join(leaf);
    std::fs::create_dir_all(base)?;
    // `create_dir`, not `create_dir_all`: a directory that already exists is an error.
    std::fs::create_dir(&path)?;
    Ok(Scratch(path))
}

/// `n` in base 36, lower-case digits and letters, most significant first.
fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
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

    /// Proves: the URL of the tests' server is the variable's value, and a value that is none or
    /// blank fails the test that needs the server, saying how to start one.
    ///
    /// Catches: a test that runs against no server, or skips, when the variable is not set.
    #[test]
    fn the_postgres_url_is_the_variables_and_none_fails_saying_how_to_start_one() {
        assert_eq!(
            postgres_url_of(Some("postgres://u@127.0.0.1:1/d".to_owned())),
            "postgres://u@127.0.0.1:1/d"
        );
        for value in [None, Some("  ".to_owned())] {
            let failed = std::panic::catch_unwind(|| postgres_url_of(value)).expect_err("fails");
            let said = failed.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(
                said.starts_with(
                    "HOSTKEEPER_TEST_POSTGRES_URL names no Postgres server: start one"
                ),
                "{said}"
            );
        }
    }

    /// The base of these tests: a directory beside the test binary, inside the target directory.
    fn base(name: &str) -> PathBuf {
        let exe = std::env::current_exe().expect("the test binary");
        exe.parent().expect("its directory").join(name)
    }

    /// Catches a fixture hook mode that exits or reports otherwise than a deferral for live
    /// leases: exit `75` and the exact last line, class, reason and retry time.
    #[test]
    fn the_leases_live_mode_defers_with_its_exit_code_and_last_line() {
        let mode = HookMode::DeferLeasesLive;
        assert_eq!(mode.exit_code(), 75);
        assert_eq!(
            mode.last_line(),
            r#"{"class":"environment","reason":"leases_live","retry_after_secs":60}"#
        );
    }

    /// Catches a `fifo` that makes another kind of file, or that passes over a path that already
    /// exists or whose directory is missing.
    #[test]
    fn fifo_makes_a_named_pipe_and_refuses_a_path_that_exists_or_has_no_directory() {
        use std::os::unix::fs::FileTypeExt;
        let dir = Scratch::under(&base("testkit-unit"), "fifo").expect("scratch");
        let pipe = dir.path().join("pipe");
        fifo(&pipe, 0o600).expect("a fifo");
        let meta = std::fs::symlink_metadata(&pipe).expect("its metadata");
        assert!(meta.file_type().is_fifo());
        assert_eq!(
            fifo(&pipe, 0o600).expect_err("it exists").kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fifo(&dir.path().join("missing/pipe"), 0o600)
                .expect_err("no directory")
                .kind(),
            io::ErrorKind::NotFound
        );
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

    /// Catches a name that leaves the base: an absolute name would replace it when joined, and
    /// `..` would climb out of it. Nothing is created for a refused name.
    #[test]
    fn a_name_that_is_not_one_plain_component_is_refused() {
        let parent = Scratch::under(&base("testkit-unit"), "names").expect("parent");
        for name in ["", "/abs", "../up", "a/b", "a/"] {
            let refused = Scratch::under(parent.path(), name).expect_err(name);
            assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{name:?}");
            assert_eq!(
                refused.to_string(),
                format!("a scratch name is one plain path component, not {name:?}")
            );
        }
        let made: Vec<_> = std::fs::read_dir(parent.path()).expect("list").collect();
        assert!(made.is_empty(), "{made:?}");
        // A name that only starts with dots is one component and stays inside the base.
        let dots = Scratch::under(parent.path(), "..").expect("a plain name");
        assert_eq!(dots.path().parent(), Some(parent.path()));
    }

    /// Catches a scratch directory that already exists being shared or emptied: making it
    /// again is an error and leaves its contents as they were.
    #[test]
    fn an_existing_directory_is_refused_and_left_alone() {
        let parent = Scratch::under(&base("testkit-unit"), "existing").expect("parent");
        let first = make(parent.path(), "taken").expect("first");
        std::fs::write(first.path().join("kept"), b"x").expect("write");
        let refused = make(parent.path(), "taken").expect_err("taken");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(first.path().join("kept")).expect("read"),
            b"x"
        );
    }

    /// Catches a name that grows a unix socket's path past its limit, or that two numbers
    /// share: each number has one short spelling.
    #[test]
    fn numbers_are_spelled_in_base_36() {
        let cases = [
            (0, "0"),
            (35, "z"),
            (36, "10"),
            (1295, "zz"),
            (4_194_304, "2hwcg"),
        ];
        for (n, spelled) in cases {
            assert_eq!(base36(n), spelled, "{n}");
        }
        assert_eq!(base36(u64::MAX), "3w5e11264sgsf");
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
