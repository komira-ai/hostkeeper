//! The coordinator's version stamp: a release build prints the first 12 digits of the full
//! commit it was given, any other build the checkout's commit, and the binary's `--version` is
//! `hostkeeper-coordinator <crate version>+<that commit>` and nothing else.

use std::process::Command;

use hostkeeper_coordinator::stamp::{
    BUILD_COMMIT, CHECKOUT_COMMIT, RELEASE_COMMIT, build_commit, short_commit, version,
};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Catches a stamp that carries the whole commit, or a commit that is not 40 lowercase hex
/// digits: only those give their first 12 digits.
#[test]
fn only_a_full_lowercase_hex_commit_gives_its_first_12_digits() {
    assert_eq!(short_commit(COMMIT), Some("0123456789ab"));
    let bad: [String; 5] = [
        COMMIT[..39].to_owned(),
        format!("{COMMIT}0"),
        COMMIT.to_uppercase(),
        format!("g{}", &COMMIT[1..]),
        String::new(),
    ];
    for text in &bad {
        assert_eq!(short_commit(text), None, "{text:?}");
    }
}

/// Catches a release build that prints the checkout's commit, or a build without a release
/// commit that prints anything but the checkout's: the release commit wins when it is given,
/// and a malformed one gives no commit at all (the build fails on it).
#[test]
fn the_release_commit_wins_over_the_checkout() {
    assert_eq!(
        build_commit(Some(COMMIT), "fedcba987654"),
        Some("0123456789ab")
    );
    assert_eq!(build_commit(None, "fedcba987654"), Some("fedcba987654"));
    assert_eq!(build_commit(None, "unknown"), Some("unknown"));
    assert_eq!(build_commit(Some("abc"), "fedcba987654"), None);
}

/// Catches a binary whose commit is not built from this build's inputs, and a `--version`
/// text that is not the crate version and that commit.
#[test]
fn this_build_prints_the_commit_of_its_inputs() {
    assert_eq!(
        Some(BUILD_COMMIT),
        build_commit(RELEASE_COMMIT, CHECKOUT_COMMIT)
    );
    assert_eq!(
        version(),
        format!("{}+{BUILD_COMMIT}", env!("CARGO_PKG_VERSION"))
    );
}

/// Catches the shipped binary printing anything but its stamp for `--version`: exactly
/// `hostkeeper-coordinator <version>+<commit>` and one line break, exit 0, nothing on stderr.
/// `cargo xtask release check-binary` holds a release build to the same line.
#[test]
fn the_binary_prints_its_stamp_for_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_hostkeeper-coordinator"))
        .arg("--version")
        .output()
        .expect("run hostkeeper-coordinator");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("hostkeeper-coordinator {}\n", version())
    );
    assert!(out.stderr.is_empty(), "{out:?}");
}
