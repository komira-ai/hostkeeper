//! The parts of `hk xcode-push` that are functions of their input: what a version and a build
//! are, how pkgutil's report is read and judged, and what the store's refusals are. The whole
//! push is in tests/cli/xcode_push.rs.

use super::archive::{is_build, is_version};
use super::signature::{APPLE_ROOT_CA, APPLE_ROOT_CA_SHA256, Link, Report, parse, verdict};
use super::upload::{error_code, exit_of_status};
use super::{Mac, command};

/// Proves: a version is decimal numbers joined by dots, and nothing else is one: not empty, no
/// empty part, no letter, no sign, no space.
#[test]
fn a_version_is_decimal_numbers_joined_by_dots() {
    for good in ["27", "27.2", "26.1.1", "0.0"] {
        assert!(is_version(good.as_bytes()), "{good}");
    }
    for bad in [
        "", ".", "27.", ".2", "27..2", "27.2b", "-27", "27 .2", "beta",
    ] {
        assert!(!is_version(bad.as_bytes()), "{bad}");
    }
}

/// Proves: a build is 1 to 32 ASCII letters, digits and dots, as the store takes it.
#[test]
fn a_build_is_1_to_32_letters_digits_and_dots() {
    let longest = "B".repeat(32);
    for good in ["27B52", "17B100", "a.1", longest.as_str()] {
        assert!(is_build(good.as_bytes()), "{good}");
    }
    let too_long = "B".repeat(33);
    for bad in [
        "",
        "27 B52",
        "27B52\n",
        "27-B52",
        "27B5é",
        too_long.as_str(),
    ] {
        assert!(!is_build(bad.as_bytes()), "{bad}");
    }
}

/// The report of an archive Apple signed, with the root's fingerprint split over two rows.
const REPORT: &str = "\
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

/// Proves: the report is read as its status and its chain, leaf first, each with its name and
/// its fingerprint in upper-case hex with no spaces, rows joined; a hex row that is not under a
/// fingerprint's line, or follows a blank line that ends one, is not one, and an entry with no
/// fingerprint has an empty one.
#[test]
fn the_report_is_read_as_a_status_and_a_chain() {
    assert_eq!(
        parse(REPORT),
        Report {
            status: Some("signed Apple Software".to_owned()),
            chain: vec![
                Link {
                    name: "Software Signing".to_owned(),
                    sha256: "D84D".to_owned(),
                },
                Link {
                    name: APPLE_ROOT_CA.to_owned(),
                    sha256: APPLE_ROOT_CA_SHA256.to_owned(),
                },
            ],
        }
    );
    assert_eq!(
        parse("1. Lone\nAB CD\n"),
        Report {
            status: None,
            chain: vec![Link {
                name: "Lone".to_owned(),
                sha256: String::new(),
            }],
        }
    );
    assert_eq!(parse("SHA256 Fingerprint:\nAB CD\n"), Report::default());
    assert_eq!(
        parse("1. Ended\nSHA256 Fingerprint:\n\nAB CD\n"),
        Report {
            status: None,
            chain: vec![Link {
                name: "Ended".to_owned(),
                sha256: String::new(),
            }],
        }
    );
    assert_eq!(parse("x. not an entry\n. nor this\n"), Report::default());
}

/// Proves: the verdict takes exactly Apple's own status and a chain ending at Apple Root CA by
/// name and fingerprint, and says what is wrong otherwise: another status, none, no chain, a
/// root of another name, and a root of the name with another fingerprint or none.
#[test]
fn only_apples_own_signature_ending_at_its_root_passes() {
    let good = parse(REPORT);
    assert_eq!(verdict(&good), Ok(()));
    let with = |status: Option<&str>, name: &str, sha256: &str| Report {
        status: status.map(str::to_owned),
        chain: vec![Link {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
        }],
    };
    let apple = Some("signed Apple Software");
    assert_eq!(
        verdict(&with(apple, APPLE_ROOT_CA, APPLE_ROOT_CA_SHA256)),
        Ok(())
    );
    assert_eq!(
        verdict(&with(
            Some("no signature"),
            APPLE_ROOT_CA,
            APPLE_ROOT_CA_SHA256
        )),
        Err("its status is \"no signature\", not \"signed Apple Software\"".to_owned())
    );
    assert_eq!(
        verdict(&with(None, APPLE_ROOT_CA, APPLE_ROOT_CA_SHA256)),
        Err("it gives no status".to_owned())
    );
    assert_eq!(
        verdict(&Report {
            status: apple.map(str::to_owned),
            chain: Vec::new(),
        }),
        Err("it gives no certificate chain".to_owned())
    );
    let lower = APPLE_ROOT_CA_SHA256.to_lowercase();
    for (name, sha256) in [
        ("Apple Root CA - G3", APPLE_ROOT_CA_SHA256),
        (APPLE_ROOT_CA, ""),
        (APPLE_ROOT_CA, lower.as_str()),
    ] {
        assert_eq!(
            verdict(&with(apple, name, sha256)),
            Err(format!(
                "its chain ends at \"{name}\" with the SHA-256 fingerprint \"{sha256}\", not at \
                 Apple Root CA ({APPLE_ROOT_CA_SHA256})"
            ))
        );
    }
}

/// Proves: the store's error answer is `{"error":"<code>"}`, spaces around it allowed, and
/// nothing else is one; and a refusal's exit code is 3 for 401 and 403, 2 for another 4xx and 1
/// for anything else.
#[test]
fn the_stores_refusals_and_their_exit_codes() {
    assert_eq!(
        error_code(b"{\"error\":\"not_listed\"}"),
        Some(b"not_listed".as_slice())
    );
    assert_eq!(
        error_code(b" {\"error\":\"too_large\"}\n"),
        Some(b"too_large".as_slice())
    );
    let bad: [&[u8]; 5] = [
        b"",
        b"{}",
        b"{\"code\":\"X\"}",
        b"{\"error\":\"x\"",
        b"<html>",
    ];
    for bad in bad {
        assert_eq!(error_code(bad), None);
    }
    for (status, code) in [
        (401, 3),
        (403, 3),
        (400, 2),
        (409, 2),
        (413, 2),
        (499, 2),
        (500, 1),
        (503, 1),
        (302, 1),
    ] {
        assert_eq!(exit_of_status(status), code, "{status}");
    }
}

/// Proves: a program the push runs is given the environment without the variables of the
/// runner's token request, and is given nothing else.
#[test]
fn a_program_runs_without_the_runners_token_variables() {
    let program = command(std::path::Path::new("xcodes"));
    let mut changed: Vec<(String, Option<String>)> = program
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    changed.sort();
    assert_eq!(
        changed,
        [
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN".to_owned(), None),
            ("ACTIONS_ID_TOKEN_REQUEST_URL".to_owned(), None),
        ]
    );
}

/// Proves: the real system names Apple's programs by their paths, takes `xcodes` from the `PATH`,
/// fetches Apple's root from the certificate authority page and holds the fingerprint the
/// signature check holds; and its clock never goes back.
#[test]
fn the_system_names_apples_programs_and_a_clock_that_does_not_go_back() {
    let mac = Mac::system();
    assert_eq!(mac.xcodes, std::path::Path::new("xcodes"));
    let paths: Vec<&std::path::Path> = [
        &mac.pkgutil,
        &mac.xip,
        &mac.plutil,
        &mac.codesign,
        &mac.spctl,
        &mac.xar,
        &mac.sw_vers,
        &mac.uname,
        &mac.cp,
        &mac.df,
        &mac.archiveutil,
        &mac.aa,
        &mac.aea,
    ]
    .into_iter()
    .map(std::path::PathBuf::as_path)
    .collect();
    assert!(paths.iter().all(|path| path.is_absolute()));
    assert_eq!(mac.cp, std::path::Path::new("/bin/cp"));
    assert_eq!(mac.pkgutil, std::path::Path::new("/usr/sbin/pkgutil"));
    assert_eq!(
        mac.apple_root_url,
        "https://www.apple.com/appleca/AppleIncRootCertificate.cer"
    );
    assert_eq!(mac.apple_root_sha256, APPLE_ROOT_CA_SHA256);
    let first = (mac.clock)();
    assert!((mac.clock)() >= first);
}
