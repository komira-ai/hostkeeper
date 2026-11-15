//! The signer pin: Team ID, Apple's own issuance, and the Developer ID Installer identity.

use hostkeeper_bootstrap::signer::{SignerError, check};

use crate::support::{TEAM, good_pkgutil, leaf_hex, pkgutil_output};

const APPLE: &str = "signed by a developer certificate issued by Apple for distribution";

/// A pkg signed by the pinned Team ID's installer identity passes, and the leaf's fingerprint is
/// what the signers file will hold.
#[test]
fn the_pinned_team_passes_and_yields_the_leaf_fingerprint() {
    let leaf = check(Some(0), &good_pkgutil(), TEAM).expect("signed by the team");
    assert_eq!(leaf.as_str(), leaf_hex());
}

/// A pkg signed by another Team ID is refused, naming both.
#[test]
fn another_team_id_is_refused() {
    let out = pkgutil_output(APPLE, "Developer ID Installer: Other Co (ABCDE12345)");
    let error = check(Some(0), &out, TEAM).expect_err("wrong team");
    assert_eq!(
        error,
        SignerError::WrongTeam {
            found: "ABCDE12345".into(),
            wanted: TEAM.into()
        }
    );
    assert_eq!(
        error.to_string(),
        "the pkg is signed by Team ID ABCDE12345, not U7RTX4XD3X"
    );
}

/// The statuses the macos-pkg installer also takes (a certificate trusted through a profile, Apple's
/// own) are not enough: a farm root could issue a leaf that carries any Team ID in its name.
#[test]
fn a_signature_that_apple_did_not_issue_is_refused() {
    for status in [
        "signed by a certificate trusted by macOS",
        "signed Apple Software",
        "untrusted",
    ] {
        let out = pkgutil_output(
            status,
            &format!("Developer ID Installer: Komira AI, Inc. ({TEAM})"),
        );
        let error = check(Some(0), &out, TEAM).expect_err("not Apple's");
        assert!(matches!(error, SignerError::NotApple(_)), "{error}");
    }
}

/// An application identity of the right team is not an installer identity.
#[test]
fn an_application_identity_is_refused() {
    let out = pkgutil_output(
        APPLE,
        &format!("Developer ID Application: Komira AI, Inc. ({TEAM})"),
    );
    assert!(matches!(
        check(Some(0), &out, TEAM),
        Err(SignerError::NotInstaller(_))
    ));
}

/// A leaf name that ends in no parentheses names no Team ID.
#[test]
fn a_leaf_with_no_team_id_is_refused() {
    let out = pkgutil_output(APPLE, "Developer ID Installer: Komira AI, Inc.");
    assert!(matches!(
        check(Some(0), &out, TEAM),
        Err(SignerError::NoTeamId(_))
    ));
}

/// A failed `pkgutil`, a missing status, a missing chain and a missing fingerprint are each
/// refused with their own reason.
#[test]
fn unreadable_pkgutil_output_is_refused() {
    assert_eq!(
        check(Some(1), &good_pkgutil(), TEAM),
        Err(SignerError::Exit(Some(1)))
    );
    assert_eq!(
        check(None, &good_pkgutil(), TEAM),
        Err(SignerError::Exit(None))
    );
    assert_eq!(
        check(Some(0), "Package \"x\":\n", TEAM),
        Err(SignerError::NoStatus)
    );
    let no_chain = format!("   Status: {APPLE}\n");
    assert_eq!(check(Some(0), &no_chain, TEAM), Err(SignerError::NoLeaf));
    let empty_chain = format!("   Status: {APPLE}\n   Certificate Chain:\n");
    assert_eq!(check(Some(0), &empty_chain, TEAM), Err(SignerError::NoLeaf));
    let unnumbered = format!("   Status: {APPLE}\n   Certificate Chain:\n    leaf\n");
    assert_eq!(check(Some(0), &unnumbered, TEAM), Err(SignerError::NoLeaf));
    let no_print = format!(
        "   Status: {APPLE}\n   Certificate Chain:\n    1. Developer ID Installer: Komira AI, Inc. ({TEAM})\n"
    );
    assert_eq!(
        check(Some(0), &no_print, TEAM),
        Err(SignerError::NoFingerprint)
    );
}
