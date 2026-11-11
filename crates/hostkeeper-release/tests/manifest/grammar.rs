//! The locator grammar of section 7.1, and the digest and name grammars.

use hostkeeper_release::grammar::{
    LocatorError, MAX_LOCATOR, check_locator, is_installer_name, is_sha256, is_store_name,
};

/// Catches: a locator of URL path characters refused, or one at 1024 bytes refused.
#[test]
fn a_locator_of_url_path_segments_passes() {
    let longest = "a".repeat(MAX_LOCATOR);
    for good in [
        "a",
        "example-org/web/releases/download/v1.6.0/web_1.6.0_amd64.deb",
        "web/1.6.0/web.bin.sigstore.json",
        "a/b:c/d@e/~f!$&'()*+,;=",
        "releases",
        "..a/b..",
        longest.as_str(),
    ] {
        assert_eq!(check_locator(good), Ok(()), "{good}");
    }
}

/// Catches: a locator that could name another host, leave its store's base, or have two
/// spellings passing: a scheme, `//`, a leading or trailing `/`, a `.` or `..` segment, a query,
/// a fragment, a `%`; and one empty or over 1024 bytes.
#[test]
fn a_locator_with_a_scheme_an_empty_or_dot_segment_a_query_a_fragment_or_a_percent_is_refused() {
    let long = "a".repeat(MAX_LOCATOR + 1);
    for (bad, why) in [
        ("", LocatorError::Empty),
        (long.as_str(), LocatorError::TooLong(1025)),
        ("a/%2e%2e/b", LocatorError::Percent),
        ("a?b=c", LocatorError::Query),
        ("a#b", LocatorError::Fragment),
        ("a b", LocatorError::Char(' ')),
        ("a\\b", LocatorError::Char('\\')),
        ("a/é", LocatorError::Char('é')),
        ("a\"b", LocatorError::Char('"')),
        ("/a/b", LocatorError::EmptySegment),
        ("a/b/", LocatorError::EmptySegment),
        ("a//b", LocatorError::EmptySegment),
        ("https://example.com/a", LocatorError::EmptySegment),
        ("a/../b", LocatorError::DotSegment),
        ("..", LocatorError::DotSegment),
        ("./a", LocatorError::DotSegment),
        ("a/.", LocatorError::DotSegment),
        ("https:example.com/a", LocatorError::Scheme),
        ("mailto:a", LocatorError::Scheme),
    ] {
        assert_eq!(check_locator(bad), Err(why), "{bad}");
    }
    for (why, text) in [
        (LocatorError::Empty, "is empty"),
        (LocatorError::TooLong(1025), "is 1025 bytes, more than 1024"),
        (
            LocatorError::Percent,
            "holds `%`; a locator takes no escape",
        ),
        (LocatorError::Query, "holds `?`; a locator has no query"),
        (
            LocatorError::Fragment,
            "holds `#`; a locator has no fragment",
        ),
        (
            LocatorError::Char(' '),
            "holds ' ', which is outside the characters of a URL path",
        ),
        (
            LocatorError::EmptySegment,
            "has an empty segment (a leading, trailing or doubled `/`)",
        ),
        (LocatorError::DotSegment, "has a `.` or `..` segment"),
        (
            LocatorError::Scheme,
            "has a `:` in its first segment, which reads as a scheme",
        ),
    ] {
        assert_eq!(why.to_string(), text);
    }
}

/// Catches: the digest, installer or store grammar taking a value outside it.
#[test]
fn the_digest_and_name_grammars_are_exact() {
    assert!(is_sha256(&"0f".repeat(32)));
    assert!(!is_sha256(&"0F".repeat(32)));
    assert!(!is_sha256(&"0f".repeat(31)));
    for check in [is_installer_name, is_store_name] {
        assert!(check("a-0"));
        assert!(check(&"z".repeat(32)));
        assert!(!check(&"z".repeat(33)));
        assert!(!check(""));
        assert!(!check("A"));
        assert!(!check("a.b"));
    }
}
