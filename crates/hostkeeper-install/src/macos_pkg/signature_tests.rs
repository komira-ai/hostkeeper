//! Tests of the reading of `pkgutil --check-signature`.

use super::*;

const LEAF: &str = "3D 2A 00 11 22 33 44 55 66 77 88 99 AA BB CC DD\n\
                    EE FF 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E";
const INTERMEDIATE: &str = "A1 A2 A3 A4 A5 A6 A7 A8 A9 B1 B2 B3 B4 B5 B6 B7\n\
                            C1 C2 C3 C4 C5 C6 C7 C8 C9 D1 D2 D3 D4 D5 D6 D7";
const ROOT: &str = "F1 F2 F3 F4 F5 F6 F7 F8 F9 E1 E2 E3 E4 E5 E6 E7\n\
                    D8 D9 D0 C0 B0 A0 90 80 70 60 50 40 30 20 10 00";

fn fingerprint_block(pairs: &str) -> String {
    let indented: Vec<String> = pairs.lines().map(|l| format!("           {l}")).collect();
    format!("       SHA256 Fingerprint:\n{}\n", indented.join("\n"))
}

/// A `pkgutil --check-signature` output with `status`, the leaf, an intermediate and the root.
fn output(status: &str) -> String {
    format!(
        "Package \"x-1.0.pkg\":\n   Status: {status}\n   Certificate Chain:\n    1. Leaf\n       Expires: 2030-01-01\n{}       ----------\n    2. Intermediate\n{}       ----------\n    3. Root\n{}",
        fingerprint_block(LEAF),
        fingerprint_block(INTERMEDIATE),
        fingerprint_block(ROOT),
    )
}

fn compact(pairs: &str) -> String {
    pairs
        .split_whitespace()
        .collect::<String>()
        .to_ascii_lowercase()
}

fn signers_of(pairs: &str) -> Signers {
    Signers::new(vec![Fingerprint::parse(&compact(pairs)).unwrap()]).unwrap()
}

const TRUSTED: &str = "signed by a certificate trusted by macOS";

#[test]
fn the_leafs_fingerprint_over_two_lines_matches_its_signer() {
    let leaf = check(Some(0), &output(TRUSTED), &signers_of(LEAF)).unwrap();
    assert_eq!(leaf.as_str(), compact(LEAF));
}

/// Catches: the intermediate's or the root's fingerprint taken for the leaf's.
#[test]
fn an_intermediates_fingerprint_as_the_only_signer_is_not_a_signer() {
    let got = check(Some(0), &output(TRUSTED), &signers_of(INTERMEDIATE));
    assert_eq!(got, Err(SignatureError::NotASigner(compact(LEAF))));
    let got = check(Some(0), &output(TRUSTED), &signers_of(ROOT));
    assert_eq!(got, Err(SignatureError::NotASigner(compact(LEAF))));
}

#[test]
fn leaf_fingerprint_is_the_first_certificates_only() {
    let leaf = leaf_fingerprint(&output(TRUSTED)).unwrap();
    assert_eq!(leaf.as_str(), compact(LEAF));
}

#[test]
fn an_unsigned_package_is_an_exit_error_quoting_the_last_line() {
    let out = "Package \"x.pkg\":\n   Status: no signature\n";
    assert_eq!(
        check(Some(1), out, &signers_of(LEAF)),
        Err(SignatureError::Exit(Some(1), "Status: no signature".into()))
    );
}

#[test]
fn a_tool_killed_by_a_signal_is_an_exit_error_with_no_text() {
    assert_eq!(
        check(None, "", &signers_of(LEAF)),
        Err(SignatureError::Exit(None, String::new()))
    );
}

/// Catches: a package signed by an untrusted chain accepted because the tool exited 0.
#[test]
fn an_untrusted_chain_that_exits_zero_is_refused() {
    let out = output("signed by untrusted certificate");
    assert_eq!(
        check(Some(0), &out, &signers_of(LEAF)),
        Err(SignatureError::Untrusted(
            "signed by untrusted certificate".into()
        ))
    );
}

#[test]
fn an_output_with_no_status_line_is_refused() {
    assert_eq!(
        check(Some(0), "Package \"x.pkg\":\n", &signers_of(LEAF)),
        Err(SignatureError::NoStatus)
    );
}

#[test]
fn a_trusted_status_without_a_fingerprint_is_refused() {
    let out = format!("   Status: {TRUSTED}\n   Certificate Chain:\n");
    assert_eq!(
        check(Some(0), &out, &signers_of(LEAF)),
        Err(SignatureError::NoFingerprint)
    );
}

#[test]
fn a_fingerprint_that_is_not_32_bytes_is_no_fingerprint() {
    let out = format!("   Status: {TRUSTED}\n SHA256 Fingerprint:\n   3D 2A\n");
    assert_eq!(
        check(Some(0), &out, &signers_of(LEAF)),
        Err(SignatureError::NoFingerprint)
    );
    let ends = format!("   Status: {TRUSTED}\n SHA256 Fingerprint:\n");
    assert_eq!(leaf_fingerprint(&ends), None);
}

#[test]
fn the_fingerprint_stops_at_the_first_line_that_is_not_hex_pairs() {
    let out = format!(
        "   Status: {TRUSTED}\n SHA256 Fingerprint:\n   {}\n   -----\n   {}\n",
        compact(LEAF)
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" "),
        "AA BB"
    );
    assert_eq!(leaf_fingerprint(&out).unwrap().as_str(), compact(LEAF));
}

#[test]
fn each_trusted_status_prefix_is_accepted() {
    for status in [
        "signed by a developer certificate issued by Apple for distribution",
        "signed by a certificate trusted by macOS",
        "signed by a certificate trusted by the user",
        "signed Apple Software",
    ] {
        assert!(
            check(Some(0), &output(status), &signers_of(LEAF)).is_ok(),
            "{status}"
        );
    }
}

#[test]
fn status_is_the_text_after_the_first_status_label() {
    assert_eq!(
        status("a\n   Status:   hello \nStatus: later"),
        Some("hello")
    );
    assert_eq!(status("nothing here"), None);
}

/// Catches: pairs after a blank line joined onto the leaf's fingerprint.
#[test]
fn the_fingerprint_stops_at_a_blank_line() {
    let out = format!(
        "   Status: {TRUSTED}\n SHA256 Fingerprint:\n{}\n\n   AA BB\n",
        LEAF
    );
    assert_eq!(leaf_fingerprint(&out).unwrap().as_str(), compact(LEAF));
}
