//! The version `hostkeeper-mdm --version` prints: `hostkeeper-mdm X.Y.Z+<first 12 hex digits of
//! the commit>` for a release build, `hostkeeper-mdm X.Y.Z` for a developer build. `cargo xtask
//! release check-binary` holds a release build to the first form.
//!
//! The commit comes in as a build-time input, the environment variable `RELEASE_COMMIT` seen by
//! the compiler (`.github/workflows/artifacts.yml` sets it), not configuration read by the
//! running program (AGENTS.md S2). It is a full 40-digit lowercase hex commit, or absent. A
//! malformed one fails the build ([`COMMIT_IS_VALID`]) rather than shipping a wrong stamp.

use std::sync::LazyLock;

/// The commit this binary was built from, when the build was given one. `option_env!` makes
/// rustc record the variable as a dependency of the crate, so cargo rebuilds when it changes and
/// no `build.rs` is needed.
const RELEASE_COMMIT: Option<&str> = option_env!("RELEASE_COMMIT");

/// How many hex digits of the commit the stamp carries.
const SHORT_COMMIT: usize = 12;

/// Whether `commit` is exactly 40 lowercase hex digits.
const fn is_commit(commit: &str) -> bool {
    let bytes = commit.as_bytes();
    if bytes.len() != 40 {
        return false;
    }
    let mut at = 0;
    while at < bytes.len() {
        if !matches!(bytes[at], b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        at += 1;
    }
    true
}

/// Whether a build's commit is acceptable: absent, or a full commit ([`is_commit`]).
const fn is_valid_input(commit: Option<&str>) -> bool {
    match commit {
        Some(commit) => is_commit(commit),
        None => true,
    }
}

/// Whether [`RELEASE_COMMIT`] is acceptable; the build fails when it is not.
const COMMIT_IS_VALID: bool = is_valid_input(RELEASE_COMMIT);

const _: () = assert!(
    COMMIT_IS_VALID,
    "RELEASE_COMMIT must be 40 lowercase hex digits"
);

/// The text after the program name: `version` and, for a release build, `+` and the first
/// [`SHORT_COMMIT`] digits of `commit` (all of it when it is shorter, which the build check
/// above never lets through).
fn version_text(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{version}+{}", commit.get(..SHORT_COMMIT).unwrap_or(commit)),
        None => version.to_owned(),
    }
}

/// The version of this binary, as `--version` prints it after `hostkeeper-mdm `.
pub(crate) fn version() -> &'static str {
    static TEXT: LazyLock<String> =
        LazyLock::new(|| version_text(env!("CARGO_PKG_VERSION"), RELEASE_COMMIT));
    &TEXT
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
