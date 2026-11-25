//! The tests of the verdict on `pkgutil --check-signature`'s report; each failure is asserted by
//! equality of its whole text.

use super::*;

/// The report of an archive Apple signed, with the root's fingerprint split over two rows and
/// the leaf's in lower case.
pub(in crate::maintenance_hook::xcode) const APPLE_REPORT: &str = "\
Package \"Xcode_27.2.xip\":
   Status: signed Apple Software
   Certificate Chain:
    1. Software Signing
       Expires: 2030-10-24 17:39:41 +0000
       SHA256 Fingerprint:
           d8 4d
       ------------------------------------------------------------------------
    2. Apple Root CA
       Expires: 2035-02-09 21:40:36 +0000
       SHA256 Fingerprint:
           B0 B1 73 0E CB C7 FF 45 05 14 2C 49 F1 29 5E 6E
           DA 6B CA ED 7E 2C 68 C5 BE 91 B5 A1 10 01 F0 24
";

fn link(name: &str, sha256: &str) -> Link {
    Link {
        name: name.to_owned(),
        sha256: sha256.to_owned(),
    }
}

/// Proves: the report is read as its status and its chain, leaf first, each with its name and
/// its fingerprint in upper-case hex with no spaces, rows joined; hex rows not under a
/// fingerprint's line (or after a blank line that ends one, or before any entry) are not
/// fingerprint rows, and an entry with no fingerprint has an empty one.
#[test]
fn the_report_is_read_as_a_status_and_a_chain() {
    assert_eq!(
        parse(APPLE_REPORT),
        Report {
            status: Some("signed Apple Software".to_owned()),
            chain: vec![
                link("Software Signing", "D84D"),
                link(APPLE_ROOT_CA, APPLE_ROOT_CA_SHA256),
            ],
        }
    );
    assert_eq!(
        parse("1. Lone\nAB CD\n"),
        Report {
            status: None,
            chain: vec![link("Lone", "")],
        }
    );
    assert_eq!(parse("SHA256 Fingerprint:\nAB CD\n"), Report::default());
    assert_eq!(
        parse("1. Ended\nSHA256 Fingerprint:\n\nAB CD\n"),
        Report {
            status: None,
            chain: vec![link("Ended", "")],
        }
    );
}

/// Proves: a numbered line that is not an entry (`1.x`, no space after the dot; `a. x`, not a
/// number; `. x`, no number) adds no certificate to the chain.
#[test]
fn a_numbered_line_that_is_not_an_entry_is_not_one() {
    for line in ["1.x", "a. x", ". x"] {
        assert_eq!(parse(line), Report::default(), "{line}");
    }
}

/// Proves: Apple's own report, a chain ending at Apple Root CA with its fingerprint, passes.
#[test]
fn apples_report_passes() {
    assert_eq!(verdict(&parse(APPLE_REPORT)), Ok(()));
}

/// Proves: a developer's archive (status "signed by a developer certificate issued by Apple for
/// distribution") fails naming that status; a report with no status fails; one with no chain
/// fails; a chain ending at another root, or at Apple Root CA with another fingerprint, fails
/// naming what it ends at.
#[test]
fn a_report_that_is_not_apples_fails_with_its_reason() {
    let developer = APPLE_REPORT.replace(
        "signed Apple Software",
        "signed by a developer certificate issued by Apple for distribution",
    );
    assert_eq!(
        verdict(&parse(&developer)),
        Err(
            "its status is \"signed by a developer certificate issued by Apple for \
             distribution\", not \"signed Apple Software\""
                .to_owned()
        )
    );
    assert_eq!(
        verdict(&parse("Package \"x\":\n")),
        Err("it gives no status".to_owned())
    );
    assert_eq!(
        verdict(&parse("Status: signed Apple Software\n")),
        Err("it gives no certificate chain".to_owned())
    );
    let other_root = APPLE_REPORT.replace("2. Apple Root CA", "2. Mallory Root CA");
    assert_eq!(
        verdict(&parse(&other_root)),
        Err(format!(
            "its chain ends at \"Mallory Root CA\" with the SHA-256 fingerprint \
             \"{APPLE_ROOT_CA_SHA256}\", not at Apple Root CA ({APPLE_ROOT_CA_SHA256})"
        ))
    );
    let other_print = APPLE_REPORT.replace("DA 6B CA ED", "DA 6B CA EE");
    assert_eq!(
        verdict(&parse(&other_print)),
        Err(
            "its chain ends at \"Apple Root CA\" with the SHA-256 fingerprint \
             \"B0B1730ECBC7FF4505142C49F1295E6EDA6BCAEE7E2C68C5BE91B5A11001F024\", not at \
             Apple Root CA (B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024)"
                .to_owned()
        )
    );
}
