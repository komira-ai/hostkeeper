//! The privacy profile of `packaging/macos-mdm/` against the signing table: device management
//! grants Full Disk Access to exactly hostkeeper's program, by the identifier and the designated
//! requirement the release signs it with, and to nothing else.

use std::path::PathBuf;

use plist::{Dictionary, Value};

use super::{MacProgram, TEAM_ID};

/// The committed profile, from the workspace root.
fn profile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/macos-mdm/hostkeeper-full-disk-access.mobileconfig")
}

fn committed_text() -> String {
    std::fs::read_to_string(profile_path()).expect("the profile is committed")
}

fn parse(text: &str) -> Value {
    Value::from_reader_xml(text.as_bytes()).expect("the profile is an XML property list")
}

fn string(s: &str) -> Value {
    Value::String(s.to_string())
}

fn dict(entries: &[(&str, Value)]) -> Value {
    let mut d = Dictionary::new();
    for (key, value) in entries {
        d.insert((*key).to_string(), value.clone());
    }
    Value::Dictionary(d)
}

/// The one profile the table allows: one PPPC payload, one `SystemPolicyAllFiles` entry for
/// hostkeeper by its identifier and requirement, allowed, scoped to the system.
fn expected() -> Value {
    let hostkeeper = MacProgram::named("hostkeeper").expect("hostkeeper is in the table");
    let entry = dict(&[
        ("Authorization", string("Allow")),
        ("CodeRequirement", string(&hostkeeper.requirement())),
        (
            "Comment",
            string(
                "hostkeeper, the host manager that runs as root from launchd, signed by the release under this identifier and team",
            ),
        ),
        ("Identifier", string(hostkeeper.identifier)),
        ("IdentifierType", string("bundleID")),
        ("StaticCode", Value::Boolean(false)),
    ]);
    let payload = dict(&[
        ("PayloadDisplayName", string("hostkeeper Full Disk Access")),
        (
            "PayloadIdentifier",
            string("dev.hostkeeper.full-disk-access.tcc"),
        ),
        (
            "PayloadType",
            string("com.apple.TCC.configuration-profile-policy"),
        ),
        (
            "PayloadUUID",
            string("7AA2C3A5-FE04-4CD8-BEE7-8CAC03E6AAC6"),
        ),
        ("PayloadVersion", Value::Integer(1.into())),
        (
            "Services",
            dict(&[("SystemPolicyAllFiles", Value::Array(vec![entry]))]),
        ),
    ]);
    dict(&[
        ("PayloadContent", Value::Array(vec![payload])),
        (
            "PayloadDescription",
            string(
                "Grants hostkeeper Full Disk Access (SystemPolicyAllFiles), so the host manager, root under launchd, may read and change every file of the Mac.",
            ),
        ),
        ("PayloadDisplayName", string("hostkeeper Full Disk Access")),
        (
            "PayloadIdentifier",
            string("dev.hostkeeper.full-disk-access"),
        ),
        ("PayloadScope", string("System")),
        ("PayloadType", string("Configuration")),
        (
            "PayloadUUID",
            string("E6F8497F-6A3C-421E-A4BF-E2F06EB29F6B"),
        ),
        ("PayloadVersion", Value::Integer(1.into())),
    ])
}

/// The text of the committed profile with `from` replaced by `to`, which must occur once.
fn committed_with(from: &str, to: &str) -> String {
    let text = committed_text();
    assert_eq!(text.matches(from).count(), 1, "{from:?} occurs once");
    text.replacen(from, to, 1)
}

#[test]
fn the_committed_profile_grants_full_disk_access_to_hostkeeper_by_its_signing_requirement() {
    // Catches: a profile whose requirement, identifier or service differs from what the release
    // signs hostkeeper with, so device management would grant a program that never runs.
    assert_eq!(parse(&committed_text()), expected());
}

#[test]
fn the_profiles_requirement_names_the_team_and_identifier_and_no_hash() {
    // Catches: a requirement pinned to one build's hash, which the next release would not meet.
    let text = committed_text();
    assert!(!text.contains("cdhash"));
    assert!(text.contains(&format!("certificate leaf[subject.OU] = {TEAM_ID}")));
    assert!(text.contains("identifier \"dev.hostkeeper.hostkeeper\""));
}

#[test]
fn a_profile_naming_another_team_or_identifier_or_a_hash_does_not_match_the_table() {
    // Catches: a comparison too loose to see a changed requirement.
    for (from, to) in [
        ("= U7RTX4XD3X</string>", "= AAAAAAAAAA</string>"),
        (
            "<string>dev.hostkeeper.hostkeeper</string>",
            "<string>dev.hostkeeper.hk</string>",
        ),
        (
            "anchor apple generic and certificate 1",
            "cdhash H\"00\" and anchor apple generic and certificate 1",
        ),
        ("<string>bundleID</string>", "<string>path</string>"),
        (
            "<key>SystemPolicyAllFiles</key>",
            "<key>SystemPolicySysAdminFiles</key>",
        ),
        ("<string>Allow</string>", "<string>Deny</string>"),
    ] {
        assert_ne!(
            parse(&committed_with(from, to)),
            expected(),
            "{from} -> {to}"
        );
    }
}

#[test]
fn a_profile_with_a_second_program_does_not_match_the_table() {
    // Catches: a grant widened to another program without the table saying so.
    let entry_end = "\t\t\t\t\t</dict>\n\t\t\t\t</array>";
    let text = committed_text();
    let start = text.find("\t\t\t\t\t<dict>").expect("the entry starts");
    let end = text.find(entry_end).expect("the entry ends");
    let entry = &text[start..end + "\t\t\t\t\t</dict>\n".len()];
    let doubled = committed_with(entry, &format!("{entry}{entry}"));
    assert_ne!(parse(&doubled), expected());
}
