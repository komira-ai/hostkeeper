//! The version the binary prints: `hostkeeper X.Y.Z+<first 12 hex digits of the commit>` for a
//! release build, `hostkeeper X.Y.Z` for a developer build.
//!
//! The commit comes in as a build-time input, the environment variable `RELEASE_COMMIT` seen by
//! the compiler, not configuration read by the running program (AGENTS.md S2). It is a full
//! 40-digit lowercase hex commit, or absent. A malformed one fails the build ([`COMMIT_IS_VALID`])
//! rather than shipping a wrong stamp.

use std::sync::LazyLock;

/// The commit this binary was built from, when the build was given one. `option_env!` makes
/// rustc record the variable as a dependency of the crate, so cargo rebuilds when it changes and
/// no `build.rs` is needed.
pub const RELEASE_COMMIT: Option<&str> = option_env!("RELEASE_COMMIT");

/// Whether `commit` is exactly 40 lowercase hex digits.
pub const fn is_commit(commit: &str) -> bool {
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
pub const fn is_valid_input(commit: Option<&str>) -> bool {
    match commit {
        Some(commit) => is_commit(commit),
        None => true,
    }
}

/// Whether [`RELEASE_COMMIT`] is acceptable; the build fails when it is not.
pub const COMMIT_IS_VALID: bool = is_valid_input(RELEASE_COMMIT);

const _: () = assert!(
    COMMIT_IS_VALID,
    "RELEASE_COMMIT must be 40 lowercase hex digits"
);

/// The text after the program name: `version` and, for a release build, `+` and the first 12
/// digits of `commit`.
pub fn version_text(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{version}+{}", commit.get(..12).unwrap_or(commit)),
        None => version.to_owned(),
    }
}

/// The version of this binary, as `--version` prints it after `hostkeeper `.
pub fn version() -> &'static str {
    static TEXT: LazyLock<String> =
        LazyLock::new(|| version_text(env!("CARGO_PKG_VERSION"), RELEASE_COMMIT));
    &TEXT
}
