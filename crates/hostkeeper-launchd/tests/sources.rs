//! The sources and the macOS doc carry no label prefix of another project.

use hostkeeper_launchd::REVERSE_DNS_PREFIX;

const SOURCES: [(&str, &str); 10] = [
    ("src/lib.rs", include_str!("../src/lib.rs")),
    ("src/fake.rs", include_str!("../src/fake.rs")),
    ("src/label.rs", include_str!("../src/label.rs")),
    ("src/launchctl.rs", include_str!("../src/launchctl.rs")),
    ("src/manager.rs", include_str!("../src/manager.rs")),
    ("src/platform.rs", include_str!("../src/platform.rs")),
    ("src/plist.rs", include_str!("../src/plist.rs")),
    ("src/procid.rs", include_str!("../src/procid.rs")),
    (
        "src/unit_control.rs",
        include_str!("../src/unit_control.rs"),
    ),
    ("docs/macos.md", include_str!("../../../docs/macos.md")),
];

fn offenders<'a>(files: &[(&'a str, &str)], needles: &[String]) -> Vec<&'a str> {
    files
        .iter()
        .filter(|(_, text)| needles.iter().any(|needle| text.contains(needle.as_str())))
        .map(|(name, _)| *name)
        .collect()
}

/// Catches a label, constant or doc that carries the reverse-DNS prefix of another project, or a
/// string literal that starts with a `com.` domain.
#[test]
fn no_label_or_doc_uses_the_old_mdm_profile_prefix() {
    let old_prefix = ["com", "komira"].join(".");
    let literal = format!("\"{}.", "com");
    let needles = [old_prefix.clone(), literal.clone()];

    // The scanner finds both needles in text that has them.
    let sample = [
        ("with_prefix", format!("const A: &str = {old_prefix};")),
        ("with_literal", format!("const A: &str = {literal}x\";")),
        ("clean", "const A: &str = \"dev.x\";".to_owned()),
    ];
    let files: Vec<(&str, &str)> = sample.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_eq!(
        offenders(&files, &needles),
        vec!["with_prefix", "with_literal"]
    );

    assert_eq!(offenders(&SOURCES, &needles), Vec::<&str>::new());
    assert!(!REVERSE_DNS_PREFIX.starts_with(&format!("{}.", "com")));
}
