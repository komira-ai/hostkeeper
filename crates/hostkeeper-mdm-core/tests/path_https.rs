//! Absolute paths and https URLs, the two small value types the settings use.

use hostkeeper_mdm_core::https::{HttpsUrl, HttpsUrlError};
use hostkeeper_mdm_core::path::{AbsPath, PathError};

/// Catches a path check that lets a unit name something outside its directory (`..`),
/// names `/` itself, takes a relative path or an empty component, or a NUL byte.
#[test]
fn an_absolute_path_has_clean_components() {
    let ok = AbsPath::new("/var/lib/hostkeeper-mdm").expect("valid");
    assert_eq!(ok.as_str(), "/var/lib/hostkeeper-mdm");
    assert_eq!(ok.to_string(), "/var/lib/hostkeeper-mdm");
    let cases = [
        ("var/lib", PathError::NotAbsolute("var/lib".to_owned())),
        ("", PathError::NotAbsolute(String::new())),
        ("/", PathError::Empty("/".to_owned())),
        ("/a//b", PathError::Component("/a//b".to_owned())),
        ("/a/", PathError::Component("/a/".to_owned())),
        ("/a/./b", PathError::Component("/a/./b".to_owned())),
        ("/a/../b", PathError::Component("/a/../b".to_owned())),
        ("/a/b\0c", PathError::Component("/a/b\0c".to_owned())),
    ];
    for (text, want) in cases {
        assert_eq!(AbsPath::new(text), Err(want), "{text:?}");
    }
    let said = |text: &str| AbsPath::new(text).expect_err(text).to_string();
    assert_eq!(said("a"), "\"a\" is not absolute");
    assert!(said("/").contains("names no file"));
    assert!(said("/a/../b").contains("component"));
}

/// Catches a URL parser that takes another scheme, a user part (it would be printed
/// with the URL), an IPv6 literal, a port that is not 1 to 65535, or an empty host, and
/// one that loses the port, path or file name of a good URL.
#[test]
fn an_https_url_is_parsed_into_host_port_and_path() {
    let u = HttpsUrl::new("https://example.com/r/v1.0/a.zip?x=1").expect("valid");
    assert_eq!(
        (u.host(), u.port(), u.path(), u.file_name()),
        ("example.com", 443, "/r/v1.0/a.zip?x=1", "a.zip")
    );
    assert_eq!(u.to_string(), "https://example.com/r/v1.0/a.zip?x=1");
    let u = HttpsUrl::new("https://example.com:8443").expect("valid");
    assert_eq!(
        (u.host(), u.port(), u.path(), u.file_name()),
        ("example.com", 8443, "/", "")
    );
    let cases = [
        ("http://example.com/", "scheme"),
        ("example.com", "scheme"),
        ("https://user:key@example.com/", "host"),
        ("https://[::1]/", "host"),
        ("https://[::1/", "host"),
        ("https://::1]/", "host"),
        ("https://:443/", "host"),
        ("https:///x", "host"),
        ("https://example.com:0/", "port"),
        ("https://example.com:x/", "port"),
        ("https://example.com:65536/", "port"),
    ];
    for (text, kind) in cases {
        let got = HttpsUrl::new(text).expect_err(text);
        let want = match kind {
            "scheme" => HttpsUrlError::Scheme(text.to_owned()),
            "host" => HttpsUrlError::Host(text.to_owned()),
            _ => HttpsUrlError::Port(text.to_owned()),
        };
        assert_eq!(got, want, "{text}");
    }
    let said = |text: &str| HttpsUrl::new(text).expect_err(text).to_string();
    assert!(said("http://a.example/").contains("is not an https:// URL"));
    assert!(said("https://a@b.example/").contains("names no host"));
    assert!(said("https://a.example:0/").contains("port"));
}

/// Catches an authority that runs past its end or holds what no host name holds
/// (komira-ai/kbf#2248): a query or a fragment right after the host was read as part of
/// it, and whitespace inside it was kept. A query after the host is the path's; a fragment,
/// whitespace and control characters are refused anywhere; a host must be letters, digits
/// and `-` in dotted labels, and a port digits only.
#[test]
fn an_authority_ends_at_its_path_or_query_and_holds_only_a_host_name_and_port() {
    let u = HttpsUrl::new("https://example.com?x=1").expect("a query after the host");
    assert_eq!(
        (u.host(), u.port(), u.path(), u.file_name()),
        ("example.com", 443, "/?x=1", "")
    );
    let u = HttpsUrl::new("https://example.com:8443?x=1").expect("a query after the port");
    assert_eq!(
        (u.host(), u.port(), u.path()),
        ("example.com", 8443, "/?x=1")
    );
    let u = HttpsUrl::new("https://Mirror.Example.COM/a/b.zip").expect("a host in any case");
    assert_eq!((u.host(), u.file_name()), ("Mirror.Example.COM", "b.zip"));
    let u = HttpsUrl::new("https://192.0.2.1:443/d").expect("a dotted address");
    assert_eq!((u.host(), u.port(), u.path()), ("192.0.2.1", 443, "/d"));
    let u = HttpsUrl::new("https://localhost/").expect("one label");
    assert_eq!(u.host(), "localhost");
    let cases = [
        (
            "https://example.com\u{20}x/",
            HttpsUrlError::Space as fn(String) -> HttpsUrlError,
        ),
        ("https://example.com /a.zip", HttpsUrlError::Space),
        ("https://example.com/a b.zip", HttpsUrlError::Space),
        ("https://example.com\t/", HttpsUrlError::Space),
        ("https://example.com\n", HttpsUrlError::Space),
        ("https://example.com/\u{7f}", HttpsUrlError::Space),
        ("https://example.com\u{a0}/", HttpsUrlError::Space),
        ("https://example.com#top", HttpsUrlError::Fragment),
        ("https://example.com/#top", HttpsUrlError::Fragment),
        ("https://example.com/a.zip?x=1#top", HttpsUrlError::Fragment),
        ("https://example.com:443#top", HttpsUrlError::Fragment),
        ("https://ex_ample.com/", HttpsUrlError::Host),
        ("https://-a.example.com/", HttpsUrlError::Host),
        ("https://a-.example.com/", HttpsUrlError::Host),
        ("https://a..example.com/", HttpsUrlError::Host),
        ("https://example.com./", HttpsUrlError::Host),
        ("https://.example.com/", HttpsUrlError::Host),
        ("https://exa%6dple.com/", HttpsUrlError::Host),
        ("https://?x=1", HttpsUrlError::Host),
        ("https://example.com:/", HttpsUrlError::Port),
        ("https://example.com:+443/", HttpsUrlError::Port),
        ("https://example.com:44 3/", HttpsUrlError::Space),
        ("https://example.com:443:443/", HttpsUrlError::Host),
    ];
    for (text, kind) in cases {
        assert_eq!(HttpsUrl::new(text), Err(kind(text.to_owned())), "{text:?}");
    }
    let said = |text: &str| HttpsUrl::new(text).expect_err(text).to_string();
    assert_eq!(
        said("https://a.example\u{20}b/"),
        "\"https://a.example b/\" holds whitespace or a control character"
    );
    assert_eq!(
        said("https://a.example/#x"),
        "\"https://a.example/#x\" has a fragment (#)"
    );
    assert_eq!(
        said("https://a_b.example/"),
        "\"https://a_b.example/\" names no host name (letters, digits and -, at most 63 characters a label and 253 in all), or names a user"
    );
}

/// Catches a host name longer than DNS allows (komira-ai/kbf#2248): a label of 63 octets and
/// a name of 253 are taken, a label of 64 and a name of 254 refused.
#[test]
fn a_url_host_is_within_the_dns_length_limits() {
    let host = |labels: &[usize]| -> String {
        let parts: Vec<String> = labels.iter().map(|n| "a".repeat(*n)).collect();
        parts.join(".")
    };
    // 63 + 1 + 63 + 1 + 63 + 1 + 61 = 253.
    let longest = host(&[63, 63, 63, 61]);
    assert_eq!(longest.len(), 253);
    for ok in [host(&[63, 1]), longest.clone()] {
        let url = format!("https://{ok}/x");
        let u = HttpsUrl::new(&url).unwrap_or_else(|e| panic!("{}: {e}", ok.len()));
        assert_eq!(u.host(), ok);
    }
    let too_long = host(&[63, 63, 63, 62]);
    assert_eq!(too_long.len(), 254);
    for bad in [host(&[64, 1]), host(&[1, 64]), too_long] {
        let url = format!("https://{bad}/x");
        assert_eq!(
            HttpsUrl::new(&url),
            Err(HttpsUrlError::Host(url.clone())),
            "{}",
            bad.len()
        );
    }
}
