//! The commit and the version the coordinator prints: `hostkeeper-coordinator X.Y.Z+<12 hex>`
//! for `--version`, and the 12 digits as `commit` in the status and convergence answers.
//!
//! The commit comes from one of two build inputs, never from configuration read by the running
//! program (AGENTS.md S2):
//!
//! - [`RELEASE_COMMIT`], the environment variable of that name seen by the compiler: the full
//!   40-digit lowercase hex commit a release build is given (`.github/workflows/artifacts.yml`).
//!   When it is set it wins, and a malformed one fails the build ([`BUILD_COMMIT`]) rather than
//!   shipping a wrong stamp;
//! - otherwise [`CHECKOUT_COMMIT`], what `build.rs` read from the checkout: 12 hex digits, or
//!   `unknown` where there is no git checkout.

use std::sync::LazyLock;

/// How many hex digits of the commit the coordinator prints.
pub const SHORT_COMMIT: usize = 12;

/// The commit a release build is given, when it is given one. `option_env!` makes rustc record
/// the variable as an input of the crate, so cargo rebuilds when it changes.
pub const RELEASE_COMMIT: Option<&str> = option_env!("RELEASE_COMMIT");

/// The commit of the checkout the coordinator was built in, as `build.rs` found it.
pub const CHECKOUT_COMMIT: &str = env!("HOSTKEEPER_BUILD_COMMIT");

/// The first [`SHORT_COMMIT`] digits of `commit` when it is exactly 40 lowercase hex digits;
/// none for anything else.
pub const fn short_commit(commit: &str) -> Option<&str> {
    let bytes = commit.as_bytes();
    if bytes.len() != 40 {
        return None;
    }
    let mut at = 0;
    while at < bytes.len() {
        if !matches!(bytes[at], b'0'..=b'9' | b'a'..=b'f') {
            return None;
        }
        at += 1;
    }
    Some(commit.split_at(SHORT_COMMIT).0)
}

/// The commit a build prints: the first digits of `release` when the build was given one (none
/// when it is malformed), otherwise `checkout`.
pub const fn build_commit<'a>(release: Option<&'a str>, checkout: &'a str) -> Option<&'a str> {
    match release {
        Some(commit) => short_commit(commit),
        None => Some(checkout),
    }
}

/// The commit this coordinator was built from, as it prints it. Evaluated by the compiler: a
/// malformed [`RELEASE_COMMIT`] fails the build here.
pub const BUILD_COMMIT: &str = match build_commit(RELEASE_COMMIT, CHECKOUT_COMMIT) {
    Some(commit) => commit,
    None => panic!("RELEASE_COMMIT must be 40 lowercase hex digits"),
};

/// The text `--version` prints after `hostkeeper-coordinator `: the crate version, `+`, and
/// [`BUILD_COMMIT`].
pub fn version() -> &'static str {
    static TEXT: LazyLock<String> =
        LazyLock::new(|| format!("{}+{BUILD_COMMIT}", env!("CARGO_PKG_VERSION")));
    &TEXT
}
