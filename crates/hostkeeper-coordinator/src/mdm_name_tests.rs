//! The MDM name's rule: what is kept, and what is refused.

use super::{enrollment_url, normalize};

/// Proves: a name is trimmed and lower-cased, and a DNS name of two or more labels is kept.
#[test]
fn a_dns_name_is_kept_in_lower_case() {
    for (given, kept) in [
        ("mdm.internal.example.com", "mdm.internal.example.com"),
        ("  MDM.Example.COM \n", "mdm.example.com"),
        ("a-b.c1.example.com", "a-b.c1.example.com"),
    ] {
        assert_eq!(normalize(given).as_deref(), Some(kept), "{given:?}");
    }
}

/// Proves: each way a name is not DNS-safe is refused: one label, an empty label, a trailing dot,
/// a leading or trailing hyphen, another character, a label over 63, a name over 253, an IP
/// address and a numeric last label.
#[test]
fn a_name_that_is_not_dns_safe_is_refused() {
    let long_label = format!("{}.example.com", "a".repeat(64));
    let long_name = format!("{}.com", vec!["a".repeat(60); 5].join("."));
    for refused in [
        "",
        "mdm",
        "mdm..example.com",
        "mdm.example.com.",
        ".example.com",
        "-mdm.example.com",
        "mdm-.example.com",
        "mdm_1.example.com",
        "mdm. example.com",
        "mdm.example.com/enroll",
        "mdm.examplé.com",
        "192.0.2.5",
        "mdm.example.123",
        &long_label,
        &long_name,
    ] {
        assert_eq!(normalize(refused), None, "{refused:?}");
    }
    assert!(long_name.len() > 253);
    // The longest label and the longest name are kept.
    let longest_label = format!("{}.example.com", "a".repeat(63));
    assert!(normalize(&longest_label).is_some());
    let longest = format!(
        "{}.{}.{}.{}.com",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(57)
    );
    assert_eq!(longest.len(), 253);
    assert!(normalize(&longest).is_some());
}

/// Proves: the enrollment URL is `https://<name>/enroll`.
#[test]
fn the_enrollment_url_is_the_names_enroll_path() {
    assert_eq!(
        enrollment_url("mdm.internal.example.com"),
        "https://mdm.internal.example.com/enroll"
    );
}
