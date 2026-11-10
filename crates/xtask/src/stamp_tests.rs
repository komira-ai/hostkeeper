//! The version stamp: the first 12 digits of a release commit after the version, nothing after
//! it for a developer build, and a commit accepted only when it is 40 lowercase hex digits.

use super::{COMMIT_IS_VALID, RELEASE_COMMIT, is_commit, is_valid_input, version, version_text};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Catches a stamp that carries the whole commit, or none of it: a release build prints the
/// version, `+`, and exactly the first 12 digits.
#[test]
fn a_release_build_prints_the_version_and_the_first_12_digits_of_the_commit() {
    assert_eq!(version_text("0.1.7", Some(COMMIT)), "0.1.7+0123456789ab");
}

/// Catches a developer build that prints a `+` suffix: without a commit the text is the version
/// alone.
#[test]
fn a_build_without_a_commit_prints_the_version_alone() {
    assert_eq!(version_text("0.1.7", None), "0.1.7");
}

/// Catches a commit shorter than 12 digits being cut or panicking: the text carries what there is.
#[test]
fn a_commit_shorter_than_12_digits_is_printed_whole() {
    assert_eq!(version_text("0.1.7", Some("abc")), "0.1.7+abc");
}

/// Catches a check that lets a malformed commit into a stamp: only 40 lowercase hex digits pass
/// (one short, one long, upper case, a non-hex digit and the empty text are refused).
#[test]
fn only_40_lowercase_hex_digits_are_a_commit() {
    assert!(is_commit(COMMIT));
    assert!(!is_commit(&COMMIT[..39]));
    assert!(!is_commit(&format!("{COMMIT}0")));
    assert!(!is_commit(&COMMIT.to_uppercase()));
    assert!(!is_commit(&format!("g{}", &COMMIT[1..])));
    assert!(!is_commit(""));
}

/// Catches a build input that is rejected when absent, or accepted when malformed: no commit and
/// a full commit are valid, a short one is not.
#[test]
fn a_build_input_is_absent_or_a_full_commit() {
    assert!(is_valid_input(None));
    assert!(is_valid_input(Some(COMMIT)));
    assert!(!is_valid_input(Some("abc")));
}

/// Catches `--version` text that is not built from this crate's version and build commit: it is
/// `version_text` of both, and the build's commit passed the check.
#[test]
fn the_binary_version_is_the_crate_version_and_the_build_commit() {
    const { assert!(COMMIT_IS_VALID) };
    assert_eq!(
        version(),
        version_text(env!("CARGO_PKG_VERSION"), RELEASE_COMMIT)
    );
}
