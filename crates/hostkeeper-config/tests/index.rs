//! The release index: the channel paths, the canonical writer, the strict parser with one case per
//! refusal, and the lookup of a platform's package.

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::{
    INDEX_INVALID, IndexError, NO_PACKAGE_FOR_PLATFORM, PackageEntry, ReleaseIndex,
    attestation_path, index_path, package_path,
};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::{Platform, UnknownPlatform};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// The canonical index of tierstore 1.5.0 with a package for each platform.
const CANONICAL: &str = concat!(
    r#"{"packages":["#,
    r#"{"bytes":1,"file":"tierstore-1.5.0-linux-arm64.tar.zst","platform":"linux-arm64","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"#,
    r#"{"bytes":2,"file":"tierstore-1.5.0-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"#,
    r#"{"bytes":3,"file":"tierstore-1.5.0-macos-arm64.tar.zst","platform":"macos-arm64","sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}"#,
    r#"],"schema":1,"service":"tierstore","version":"1.5.0"}"#,
);

fn service() -> ServiceName {
    ServiceName::new("tierstore").unwrap()
}

fn version() -> Version {
    Version::new("1.5.0").unwrap()
}

fn entry(platform: Platform, sha256: &str, bytes: u64) -> PackageEntry {
    PackageEntry {
        platform,
        sha256: Sha256Hex::parse(sha256).unwrap(),
        bytes,
    }
}

fn parse(text: &str) -> Result<ReleaseIndex, IndexError> {
    ReleaseIndex::parse(text, &service(), &version())
}

/// `CANONICAL` with `from` written as `to`. Panics if `from` is not in it.
fn with(from: &str, to: &str) -> String {
    assert!(CANONICAL.contains(from), "{from:?} is not in the index");
    CANONICAL.replacen(from, to, 1)
}

/// The refusal of `text`, checked to carry the token `index_invalid`.
fn refusal(text: &str) -> IndexError {
    let error = parse(text).expect_err("the index must be refused");
    assert_eq!(error.reason(), INDEX_INVALID, "{error}");
    error
}

/// The three channel paths of a version, relative to the channel.
#[test]
fn the_channel_paths_of_a_version() {
    assert_eq!(
        index_path(&service(), &version()),
        "tierstore/1.5.0/index.json"
    );
    assert_eq!(
        package_path(&service(), &version(), Platform::LinuxX86_64),
        "tierstore/1.5.0/tierstore-1.5.0-linux-x86_64.tar.zst"
    );
    assert_eq!(
        package_path(&service(), &version(), Platform::MacosArm64),
        "tierstore/1.5.0/tierstore-1.5.0-macos-arm64.tar.zst"
    );
    assert_eq!(
        attestation_path(&service(), &version(), Platform::LinuxArm64),
        "tierstore/1.5.0/tierstore-1.5.0-linux-arm64.tar.zst.sigstore.json"
    );
}

/// The writer sorts the packages by platform and the keys by name, with no white space; the
/// parser reads that text back to the same index.
#[test]
fn the_writer_is_canonical_and_the_parser_reads_it_back() {
    let index = ReleaseIndex::new(
        service(),
        version(),
        vec![
            entry(Platform::MacosArm64, C, 3),
            entry(Platform::LinuxArm64, A, 1),
            entry(Platform::LinuxX86_64, B, 2),
        ],
    )
    .unwrap();
    assert_eq!(index.to_json(), CANONICAL);
    assert_eq!(parse(CANONICAL), Ok(index.clone()));
    assert_eq!(index.service(), &service());
    assert_eq!(index.version(), &version());
    let platforms: Vec<Platform> = index.packages().iter().map(|e| e.platform).collect();
    assert_eq!(platforms, Platform::ALL);
}

/// One package is enough, and the size reaches the whole range of 64 bits.
#[test]
fn one_package_of_any_size_round_trips() {
    for bytes in [0, u64::MAX] {
        let index = ReleaseIndex::new(
            service(),
            version(),
            vec![entry(Platform::LinuxX86_64, B, bytes)],
        )
        .unwrap();
        let text = index.to_json();
        assert_eq!(
            text,
            format!(
                r#"{{"packages":[{{"bytes":{bytes},"file":"tierstore-1.5.0-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"{B}"}}],"schema":1,"service":"tierstore","version":"1.5.0"}}"#
            )
        );
        assert_eq!(parse(&text), Ok(index));
    }
}

/// White space and key order do not matter on read, and a digest in upper case is read and
/// written back in lower case.
#[test]
fn the_parser_takes_any_layout_and_either_case_of_digest() {
    let text = format!(
        r#"
        {{
          "version": "1.5.0",
          "service": "tierstore",
          "schema": 1,
          "packages": [
            {{ "sha256": "{}", "platform": "linux-x86_64", "bytes": 2,
               "file": "tierstore-1.5.0-linux-x86_64.tar.zst" }}
          ]
        }}
        "#,
        B.to_ascii_uppercase()
    );
    let index = parse(&text).unwrap();
    assert_eq!(index.packages(), [entry(Platform::LinuxX86_64, B, 2)]);
    assert!(index.to_json().contains(B));
}

/// `entry_for` gives the package of a listed platform; a platform the index does not list is
/// `no_package_for_platform`.
#[test]
fn entry_for_finds_a_listed_platform_and_refuses_another() {
    let index = parse(CANONICAL).unwrap();
    assert_eq!(
        index.entry_for(Platform::LinuxX86_64),
        Ok(&entry(Platform::LinuxX86_64, B, 2))
    );
    let only_arm = ReleaseIndex::new(
        service(),
        version(),
        vec![entry(Platform::LinuxArm64, A, 1)],
    )
    .unwrap();
    let error = only_arm.entry_for(Platform::MacosArm64).unwrap_err();
    assert_eq!(
        error,
        IndexError::NoPackageForPlatform(Platform::MacosArm64)
    );
    assert_eq!(error.reason(), NO_PACKAGE_FOR_PLATFORM);
    assert_eq!(
        error.to_string(),
        "the release index has no package for macos-arm64"
    );
}

/// Text that is not JSON, or JSON with anything after the document, is refused.
#[test]
fn text_that_is_not_one_json_document_is_refused() {
    for bad in ["", "not json", "{", &format!("{CANONICAL}{{}}")] {
        assert!(matches!(refusal(bad), IndexError::Syntax(_)), "{bad:?}");
    }
}

/// An unknown key is refused, at the top and in a package.
#[test]
fn an_unknown_key_is_refused() {
    for bad in [
        with(r#""schema":1,"#, r#""schema":1,"signature":"x","#),
        with(r#"{"bytes":1,"#, r#"{"bytes":1,"isa":"v3","#),
    ] {
        let IndexError::Syntax(why) = refusal(&bad) else {
            panic!("not a syntax refusal: {bad}");
        };
        assert!(why.contains("unknown field"), "{why}");
    }
}

/// A key written twice is refused, even with the same value.
#[test]
fn a_repeated_key_is_refused() {
    let bad = with(
        r#""service":"tierstore","#,
        r#""service":"tierstore","service":"tierstore","#,
    );
    let IndexError::Syntax(why) = refusal(&bad) else {
        panic!("not a syntax refusal");
    };
    assert!(why.contains("duplicate field"), "{why}");
}

/// A missing key is refused, at the top and in a package; a missing schema too.
#[test]
fn a_missing_key_is_refused() {
    for bad in [
        with(r#","version":"1.5.0""#, ""),
        with(r#""bytes":1,"#, ""),
        with(r#""schema":1,"#, ""),
    ] {
        let IndexError::Syntax(why) = refusal(&bad) else {
            panic!("not a syntax refusal: {bad}");
        };
        assert!(why.contains("missing field"), "{why}");
    }
}

/// A value of the wrong type is refused: a size that is text, negative or a fraction, a platform
/// that is not text, and a service that is a list.
#[test]
fn a_value_of_the_wrong_type_is_refused() {
    for bad in [
        with(r#""bytes":1,"#, r#""bytes":"1","#),
        with(r#""bytes":1,"#, r#""bytes":-1,"#),
        with(r#""bytes":1,"#, r#""bytes":1.5,"#),
        with(r#""platform":"linux-arm64""#, r#""platform":3"#),
        with(r#""service":"tierstore""#, r#""service":["tierstore"]"#),
    ] {
        assert!(matches!(refusal(&bad), IndexError::Syntax(_)), "{bad}");
    }
}

/// Any schema but the whole number 1 is refused, naming what was found, before the other keys
/// are judged (a schema 2 index with a new key is a schema refusal).
#[test]
fn another_schema_is_refused() {
    for (schema, found) in [("2", "2"), ("0", "0"), (r#""1""#, r#""1""#), ("1.0", "1.0")] {
        let bad = with(
            r#""schema":1,"#,
            &format!(r#""schema":{schema},"new":true,"#),
        );
        assert_eq!(
            refusal(&bad),
            IndexError::Schema {
                found: found.to_owned()
            }
        );
    }
    assert_eq!(
        IndexError::Schema { found: "2".into() }.to_string(),
        "the release index has schema 2; this daemon reads schema 1 only"
    );
}

/// An index for another service is refused.
#[test]
fn another_service_is_refused() {
    let bad = with(r#""service":"tierstore""#, r#""service":"kbf""#);
    assert_eq!(
        refusal(&bad),
        IndexError::ServiceMismatch {
            expected: "tierstore".into(),
            found: "kbf".into()
        }
    );
}

/// An index for another version is refused.
#[test]
fn another_version_is_refused() {
    let bad = with(r#""version":"1.5.0""#, r#""version":"1.5.1""#);
    assert_eq!(
        refusal(&bad),
        IndexError::VersionMismatch {
            expected: "1.5.0".into(),
            found: "1.5.1".into()
        }
    );
}

/// An index with no package is refused, read or built.
#[test]
fn an_empty_package_list_is_refused() {
    let bad = r#"{"packages":[],"schema":1,"service":"tierstore","version":"1.5.0"}"#;
    assert_eq!(refusal(bad), IndexError::NoPackages);
    assert_eq!(
        ReleaseIndex::new(service(), version(), Vec::new()),
        Err(IndexError::NoPackages)
    );
}

/// A package for a platform outside the three is refused, naming the spelling.
#[test]
fn an_unknown_platform_is_refused() {
    let bad = with(
        r#""platform":"linux-arm64""#,
        r#""platform":"linux-aarch64""#,
    );
    assert_eq!(
        refusal(&bad),
        IndexError::UnknownPlatform(UnknownPlatform("linux-aarch64".into()))
    );
}

/// A platform listed twice is refused, read (one after the other) or built (anywhere in the
/// list).
#[test]
fn a_platform_listed_twice_is_refused() {
    let twice = concat!(
        r#"{"packages":["#,
        r#"{"bytes":1,"file":"tierstore-1.5.0-linux-arm64.tar.zst","platform":"linux-arm64","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"#,
        r#"{"bytes":1,"file":"tierstore-1.5.0-linux-arm64.tar.zst","platform":"linux-arm64","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"],"schema":1,"service":"tierstore","version":"1.5.0"}"#,
    );
    assert_eq!(
        refusal(twice),
        IndexError::DuplicatePlatform(Platform::LinuxArm64)
    );
    assert_eq!(
        ReleaseIndex::new(
            service(),
            version(),
            vec![
                entry(Platform::MacosArm64, C, 3),
                entry(Platform::LinuxArm64, A, 1),
                entry(Platform::MacosArm64, C, 4),
            ],
        ),
        Err(IndexError::DuplicatePlatform(Platform::MacosArm64))
    );
}

/// Packages out of platform order are refused on read (the writer always sorts them).
#[test]
fn packages_out_of_order_are_refused() {
    let swapped = concat!(
        r#"{"packages":["#,
        r#"{"bytes":2,"file":"tierstore-1.5.0-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"#,
        r#"{"bytes":1,"file":"tierstore-1.5.0-linux-arm64.tar.zst","platform":"linux-arm64","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        r#"],"schema":1,"service":"tierstore","version":"1.5.0"}"#,
    );
    assert_eq!(
        refusal(swapped),
        IndexError::Unsorted {
            before: Platform::LinuxX86_64,
            after: Platform::LinuxArm64
        }
    );
}

/// A `file` other than `{S}-{V}-{platform}.tar.zst` is refused: another platform's name, another
/// version's, and a path out of the version's directory.
#[test]
fn a_file_other_than_the_package_name_is_refused() {
    for found in [
        "tierstore-1.5.0-linux-x86_64.tar.zst",
        "tierstore-1.4.0-linux-arm64.tar.zst",
        "../../kbf/1.0/kbf-1.0-linux-arm64.tar.zst",
        "tierstore-1.5.0-linux-arm64.tar.gz",
    ] {
        let bad = with(
            r#""file":"tierstore-1.5.0-linux-arm64.tar.zst""#,
            &format!(r#""file":"{found}""#),
        );
        assert_eq!(
            refusal(&bad),
            IndexError::FileName {
                platform: Platform::LinuxArm64,
                expected: "tierstore-1.5.0-linux-arm64.tar.zst".into(),
                found: found.into(),
            }
        );
    }
}

/// A `sha256` that is not 64 hex digits is refused: one short, one long, not hex, empty.
#[test]
fn a_digest_that_is_not_64_hex_digits_is_refused() {
    for found in [&A[1..], &format!("{A}a"), &format!("{}g", &A[1..]), ""] {
        let bad = with(
            &format!(r#""sha256":"{A}""#),
            &format!(r#""sha256":"{found}""#),
        );
        assert_eq!(
            refusal(&bad),
            IndexError::Digest {
                platform: Platform::LinuxArm64,
                found: found.to_owned(),
            }
        );
    }
}

/// Each refusal says what was wrong.
#[test]
fn each_refusal_says_what_was_wrong() {
    for (error, text) in [
        (
            IndexError::ServiceMismatch {
                expected: "a".into(),
                found: "b".into(),
            },
            r#"the release index is for the service "b", not "a""#,
        ),
        (
            IndexError::VersionMismatch {
                expected: "1".into(),
                found: "2".into(),
            },
            r#"the release index is for the version "2", not "1""#,
        ),
        (IndexError::NoPackages, "the release index lists no package"),
        (
            IndexError::UnknownPlatform(UnknownPlatform("x".into())),
            r#"the release index lists a package for "x" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64"#,
        ),
        (
            IndexError::DuplicatePlatform(Platform::LinuxArm64),
            "the release index lists the platform linux-arm64 more than once",
        ),
        (
            IndexError::Unsorted {
                before: Platform::MacosArm64,
                after: Platform::LinuxArm64,
            },
            "the release index lists linux-arm64 after macos-arm64; packages are sorted by platform",
        ),
        (
            IndexError::FileName {
                platform: Platform::LinuxArm64,
                expected: "e".into(),
                found: "f".into(),
            },
            r#"the release index names the file "f" for linux-arm64, not "e""#,
        ),
        (
            IndexError::Digest {
                platform: Platform::LinuxArm64,
                found: "z".into(),
            },
            r#"the release index's sha256 for linux-arm64 is "z", not 64 hex digits"#,
        ),
        (
            IndexError::Syntax("x".into()),
            "the release index is not an index of schema 1: x",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
}
