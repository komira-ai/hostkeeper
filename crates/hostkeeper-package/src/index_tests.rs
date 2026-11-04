use super::*;
use crate::testing::{config, digest, index_json, sha256_hex};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The service and version every index of these tests is asked for.
fn asked() -> (ServiceName, Version) {
    (
        ServiceName::new("tierstore").expect("a service name"),
        Version::new("1.5.0").expect("a version"),
    )
}

/// A channel URL read by the real config loader.
fn channel(text: &str) -> HttpsUrl {
    config(text, &"0".repeat(64)).release_channel
}

/// One package object whose four values are given as JSON text.
fn raw_package(platform: &str, file: &str, sha256: &str, bytes: &str) -> String {
    format!("{{\"bytes\":{bytes},\"file\":{file},\"platform\":{platform},\"sha256\":{sha256}}}")
}

/// One package object whose text values are given unquoted.
fn package(platform: &str, file: &str, sha256: &str, bytes: &str) -> String {
    raw_package(
        &format!("\"{platform}\""),
        &format!("\"{file}\""),
        &format!("\"{sha256}\""),
        bytes,
    )
}

/// A correct package object of tierstore 1.5.0 for `platform`.
fn good_package(platform: Platform) -> String {
    let (service, version) = asked();
    package(
        platform.name(),
        &package_file(&service, &version, platform),
        SHA,
        "7",
    )
}

/// An index of `service` `version` with the schema and the packages array given as JSON text.
fn document(schema: &str, service: &str, version: &str, packages: &str) -> String {
    format!(
        "{{\"packages\":{packages},\"schema\":{schema},\"service\":\"{service}\",\"version\":\"{version}\"}}"
    )
}

/// An index of tierstore 1.5.0 holding `packages` (JSON text of each).
fn document_of(packages: &[String]) -> String {
    document(
        "1",
        "tierstore",
        "1.5.0",
        &format!("[{}]", packages.join(",")),
    )
}

/// What parsing `text` as the index of tierstore 1.5.0 refuses with.
fn refusal(text: &str) -> IndexInvalid {
    let (service, version) = asked();
    ReleaseIndex::parse(text.as_bytes(), &service, &version).expect_err("a refusal")
}

/// Every `(text, why)` is refused with exactly `index_invalid: {why}`.
fn assert_refusals(cases: &[(String, String)]) {
    for (text, why) in cases {
        assert_eq!(refusal(text), IndexInvalid(why.clone()), "{text}");
    }
}

/// Proves the platforms are listed in the order of their names, with the names the files and the
/// index use. Catches a reordered set (the index order check would then refuse good indexes) or
/// a renamed platform (file names would then differ from what the publisher writes).
#[test]
fn the_platforms_are_the_three_named_in_name_order() {
    let names: Vec<&str> = Platform::ALL.iter().map(|p| p.name()).collect();
    assert_eq!(names, ["linux-arm64", "linux-x86_64", "macos-arm64"]);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert_eq!(Platform::LinuxArm64.name(), "linux-arm64");
    assert_eq!(Platform::LinuxX86_64.name(), "linux-x86_64");
    assert_eq!(Platform::MacosArm64.name(), "macos-arm64");
}

/// Proves a platform is found by its exact name and by nothing else. Catches a lookup that
/// ignores case, trims, accepts a prefix, or invents a platform for an unknown name.
#[test]
fn a_platform_is_found_by_its_exact_name_only() {
    for platform in Platform::ALL {
        assert_eq!(Platform::of_name(platform.name()), Some(platform));
    }
    for unknown in [
        "windows-x86_64",
        "linux",
        "",
        "Linux-x86_64",
        "linux-x86_64 ",
        "linux-aarch64",
    ] {
        assert_eq!(Platform::of_name(unknown), None, "{unknown:?}");
    }
}

/// The platform the compiled target must give, for the target this build is compiled for.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET_PLATFORM: Platform = Platform::LinuxX86_64;
/// The platform the compiled target must give, for the target this build is compiled for.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET_PLATFORM: Platform = Platform::LinuxArm64;
/// The platform the compiled target must give, for the target this build is compiled for.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET_PLATFORM: Platform = Platform::MacosArm64;

/// Proves the platform of the running build is the one the compilation target names (linux
/// x86_64, linux aarch64 or macos aarch64, each expected only on its own target) and that it is
/// found again by its own name. Catches a `current` that returns another supported platform,
/// which would select the wrong package.
#[test]
fn the_current_platform_is_the_compilation_targets_and_round_trips_by_name() {
    let current = Platform::current();
    assert_eq!(current, TARGET_PLATFORM);
    assert_eq!(Platform::of_name(current.name()), Some(current));
}

/// Proves the index URL is the version directory plus `index.json`, with one trailing `/` of
/// the channel dropped. Catches a doubled slash, a missing segment or a changed channel.
#[test]
fn the_index_url_is_the_version_directory_and_index_json() {
    let (service, version) = asked();
    for text in ["https://releases.example", "https://releases.example/"] {
        assert_eq!(
            index_url(&channel(text), &service, &version).as_str(),
            "https://releases.example/tierstore/1.5.0/index.json",
            "{text}"
        );
    }
}

/// Proves the package URL is the version directory plus the platform's file, for each platform,
/// with or without a trailing `/` on the channel. Catches a wrong directory, a platform name
/// left out of the file or a doubled slash.
#[test]
fn the_package_url_is_the_version_directory_and_the_platforms_file() {
    let (service, version) = asked();
    let expected = [
        (
            Platform::LinuxArm64,
            "https://releases.example/tierstore/1.5.0/tierstore-1.5.0-linux-arm64.tar.zst",
        ),
        (
            Platform::LinuxX86_64,
            "https://releases.example/tierstore/1.5.0/tierstore-1.5.0-linux-x86_64.tar.zst",
        ),
        (
            Platform::MacosArm64,
            "https://releases.example/tierstore/1.5.0/tierstore-1.5.0-macos-arm64.tar.zst",
        ),
    ];
    for text in ["https://releases.example", "https://releases.example/"] {
        for (platform, url) in expected {
            assert_eq!(
                package_url(&channel(text), &service, &version, platform).as_str(),
                url,
                "{text} {platform:?}"
            );
        }
    }
}

/// Proves the package file name is `{service}-{version}-{platform}.tar.zst` for each platform.
/// Catches a changed order of the parts or a changed suffix.
#[test]
fn the_package_file_is_service_version_platform_and_the_suffix() {
    let (service, version) = asked();
    let files: Vec<String> = Platform::ALL
        .iter()
        .map(|p| package_file(&service, &version, *p))
        .collect();
    assert_eq!(
        files,
        [
            "tierstore-1.5.0-linux-arm64.tar.zst",
            "tierstore-1.5.0-linux-x86_64.tar.zst",
            "tierstore-1.5.0-macos-arm64.tar.zst",
        ]
    );
}

/// Proves a one-package index parses to its one entry, with the file name derived from the
/// platform, the digest and the size of the package. Catches a dropped or altered field.
#[test]
fn a_one_platform_index_parses_to_its_entry() {
    let (service, version) = asked();
    let text = index_json(
        "tierstore",
        "1.5.0",
        &[(Platform::LinuxX86_64, &b"abc"[..])],
    );
    let index = ReleaseIndex::parse(text.as_bytes(), &service, &version).expect("an index");
    let entry = IndexEntry {
        platform: Platform::LinuxX86_64,
        file: "tierstore-1.5.0-linux-x86_64.tar.zst".to_owned(),
        sha256: digest(&sha256_hex(b"abc")),
        bytes: 3,
    };
    assert_eq!(
        index,
        ReleaseIndex {
            service,
            version,
            packages: vec![entry.clone()],
        }
    );
    assert_eq!(index.package(Platform::LinuxX86_64), Some(&entry));
    assert_eq!(index.package(Platform::LinuxArm64), None);
    assert_eq!(index.package(Platform::MacosArm64), None);
}

/// Proves a three-platform index keeps its entries in order, each with its own digest and size,
/// and `package` finds each by platform. Catches entries swapped between platforms.
#[test]
fn a_three_platform_index_finds_each_platforms_own_entry() {
    let (service, version) = asked();
    let text = index_json(
        "tierstore",
        "1.5.0",
        &[
            (Platform::LinuxArm64, &b"one"[..]),
            (Platform::LinuxX86_64, &b"three!"[..]),
            (Platform::MacosArm64, &b"fifteen"[..]),
        ],
    );
    let index = ReleaseIndex::parse(text.as_bytes(), &service, &version).expect("an index");
    let sizes = [3_u64, 6, 7];
    let bodies: [&[u8]; 3] = [b"one", b"three!", b"fifteen"];
    for (n, platform) in Platform::ALL.into_iter().enumerate() {
        let entry = IndexEntry {
            platform,
            file: package_file(&service, &version, platform),
            sha256: digest(&sha256_hex(bodies[n])),
            bytes: sizes[n],
        };
        assert_eq!(index.packages[n], entry);
        assert_eq!(index.package(platform), Some(&entry));
    }
    assert_eq!(index.packages.len(), 3);
}

/// Proves a text that is not JSON is refused with the reason `not JSON`. Catches a parse that
/// panics or passes on empty or non-UTF-8 input.
#[test]
fn a_text_that_is_not_json_is_refused() {
    for bytes in [&b""[..], &b"{\"packages\":"[..], &b"\xff\xfe"[..]] {
        let (service, version) = asked();
        let why = ReleaseIndex::parse(bytes, &service, &version)
            .expect_err("a refusal")
            .to_string();
        assert!(why.starts_with("index_invalid: not JSON: "), "{why}");
    }
}

/// Proves the refusal prints with the token `index_invalid: ` before the reason. Catches a
/// Display that drops the token the failure mapping relies on.
#[test]
fn the_refusal_displays_with_the_index_invalid_token() {
    assert_eq!(
        IndexInvalid("why".to_owned()).to_string(),
        "index_invalid: why"
    );
    assert_eq!(
        refusal("[]").to_string(),
        "index_invalid: the index is not a JSON object"
    );
}

/// Proves the top level of the index is read strictly: an object with exactly the four fields,
/// schema 1, and the service and version asked for. Catches a lax reader that accepts another
/// shape, an extra or missing field, another schema or another release.
#[test]
fn the_index_is_read_strictly_and_each_refusal_says_why() {
    let one = good_package(Platform::LinuxX86_64);
    let packages = format!("[{one}]");
    let fields = r#"["packages", "schema", "service", "version"]"#;
    let good = document("1", "tierstore", "1.5.0", &packages);
    let cases = [
        ("[]".to_owned(), "the index is not a JSON object".to_owned()),
        (
            "\"text\"".to_owned(),
            "the index is not a JSON object".to_owned(),
        ),
        (
            good.replace("\"schema\":1,", ""),
            format!(
                "the index has the fields [\"packages\", \"service\", \"version\"], not exactly {fields}"
            ),
        ),
        (
            good.replace(
                "\"version\":\"1.5.0\"}",
                "\"version\":\"1.5.0\",\"zextra\":1}",
            ),
            format!(
                "the index has the fields [\"packages\", \"schema\", \"service\", \"version\", \"zextra\"], not exactly {fields}"
            ),
        ),
        (
            document("2", "tierstore", "1.5.0", &packages),
            "the schema is not 1".to_owned(),
        ),
        (
            document("\"1\"", "tierstore", "1.5.0", &packages),
            "the schema is not 1".to_owned(),
        ),
        (
            document("1", "other", "1.5.0", &packages),
            "the index is of other 1.5.0, not tierstore 1.5.0".to_owned(),
        ),
        (
            document("1", "tierstore", "1.5.1", &packages),
            "the index is of tierstore 1.5.1, not tierstore 1.5.0".to_owned(),
        ),
        (
            good.replace("\"service\":\"tierstore\"", "\"service\":5"),
            "the index: service is not a string".to_owned(),
        ),
        (
            good.replace("\"version\":\"1.5.0\"", "\"version\":[]"),
            "the index: version is not a string".to_owned(),
        ),
        (
            document("1", "tierstore", "1.5.0", "{}"),
            "packages is not a list".to_owned(),
        ),
        (
            document("1", "tierstore", "1.5.0", "[1]"),
            "packages[0] is not a JSON object".to_owned(),
        ),
    ];
    assert_refusals(&cases);
}

/// Proves each package entry is read strictly: exactly its four fields, a known platform, the
/// file named for it, a lower-case 64-digit digest and a whole non-negative size. Catches an
/// entry accepted with an extra field, a foreign file name, a digest in another case or length,
/// or a size that is signed, fractional or text; and an error that names the wrong entry.
#[test]
fn each_package_entry_is_read_strictly_and_each_refusal_says_why() {
    let file = "tierstore-1.5.0-linux-x86_64.tar.zst";
    let fields = r#"["bytes", "file", "platform", "sha256"]"#;
    let sha_why =
        |text: &str| format!("packages[0]: {text:?} is not a SHA-256 (64 lower-case hex digits)");
    let wrapped = |entry: String| document_of(&[entry]);
    let cases = [
        (
            wrapped(good_package(Platform::LinuxX86_64).replace("}", ",\"zextra\":1}")),
            format!(
                "packages[0] has the fields [\"bytes\", \"file\", \"platform\", \"sha256\", \"zextra\"], not exactly {fields}"
            ),
        ),
        (
            wrapped(good_package(Platform::LinuxX86_64).replace("\"bytes\":7,", "")),
            format!(
                "packages[0] has the fields [\"file\", \"platform\", \"sha256\"], not exactly {fields}"
            ),
        ),
        (
            wrapped(raw_package(
                "5",
                &format!("\"{file}\""),
                &format!("\"{SHA}\""),
                "7",
            )),
            "packages[0]: platform is not a string".to_owned(),
        ),
        (
            wrapped(package("windows-x86_64", file, SHA, "7")),
            "packages[0]: \"windows-x86_64\" is not a platform".to_owned(),
        ),
        (
            wrapped(package(
                "linux-x86_64",
                "tierstore-1.5.0-linux-arm64.tar.zst",
                SHA,
                "7",
            )),
            format!(
                "packages[0]: the file is \"tierstore-1.5.0-linux-arm64.tar.zst\", not \"{file}\""
            ),
        ),
        (
            wrapped(raw_package(
                "\"linux-x86_64\"",
                "7",
                &format!("\"{SHA}\""),
                "7",
            )),
            "packages[0]: file is not a string".to_owned(),
        ),
        (
            wrapped(package("linux-x86_64", file, &SHA.to_uppercase(), "7")),
            sha_why(&SHA.to_uppercase()),
        ),
        (
            wrapped(package("linux-x86_64", file, &SHA[..63], "7")),
            sha_why(&SHA[..63]),
        ),
        (
            wrapped(package("linux-x86_64", file, &"g".repeat(64), "7")),
            sha_why(&"g".repeat(64)),
        ),
        (
            wrapped(raw_package(
                "\"linux-x86_64\"",
                &format!("\"{file}\""),
                "5",
                "7",
            )),
            "packages[0]: sha256 is not a string".to_owned(),
        ),
        (
            wrapped(package("linux-x86_64", file, SHA, "-1")),
            "packages[0]: bytes is not a whole number".to_owned(),
        ),
        (
            wrapped(package("linux-x86_64", file, SHA, "\"7\"")),
            "packages[0]: bytes is not a whole number".to_owned(),
        ),
        (
            wrapped(package("linux-x86_64", file, SHA, "1.5")),
            "packages[0]: bytes is not a whole number".to_owned(),
        ),
        (
            document_of(&[
                good_package(Platform::LinuxArm64),
                package("macos-arm64", file, SHA, "7"),
            ]),
            format!(
                "packages[1]: the file is \"{file}\", not \"tierstore-1.5.0-macos-arm64.tar.zst\""
            ),
        ),
    ];
    assert_refusals(&cases);
}

/// Proves the set of platforms must be at least one, each once, in name order. Catches an index
/// with no package, a platform listed twice, or platforms out of order being accepted.
#[test]
fn the_platforms_must_be_one_or_more_each_once_and_in_order() {
    let x86 = || good_package(Platform::LinuxX86_64);
    let arm = || good_package(Platform::LinuxArm64);
    let mac = || good_package(Platform::MacosArm64);
    let cases = [
        (
            document_of(&[]),
            "the platforms [] are not one or more, each once, in order".to_owned(),
        ),
        (
            document_of(&[x86(), x86()]),
            "the platforms [\"linux-x86_64\", \"linux-x86_64\"] are not one or more, each once, in order"
                .to_owned(),
        ),
        (
            document_of(&[x86(), arm()]),
            "the platforms [\"linux-x86_64\", \"linux-arm64\"] are not one or more, each once, in order"
                .to_owned(),
        ),
        (
            document_of(&[arm(), mac(), x86()]),
            "the platforms [\"linux-arm64\", \"macos-arm64\", \"linux-x86_64\"] are not one or more, each once, in order"
                .to_owned(),
        ),
    ];
    assert_refusals(&cases);
}

/// Proves an index that repeats a key is refused naming the key, at the top level and inside a
/// package entry, even when the last value would have passed every other check (schema 2 then 1,
/// a second digest or size). Catches a parse that keeps the last duplicate silently.
#[test]
fn a_repeated_key_anywhere_in_the_index_is_refused_naming_the_key() {
    let one = good_package(Platform::LinuxX86_64);
    let good = document("1", "tierstore", "1.5.0", &format!("[{one}]"));
    let repeated = |text: String, key: &str| {
        (
            text,
            format!("an object of the index repeats the key {key:?}"),
        )
    };
    let cases = [
        repeated(
            good.replace("\"schema\":1,", "\"schema\":2,\"schema\":1,"),
            "schema",
        ),
        repeated(
            good.replace("\"service\":", "\"service\":\"other\",\"service\":"),
            "service",
        ),
        repeated(
            good.replace(
                &format!("\"sha256\":\"{SHA}\""),
                &format!("\"sha256\":\"{SHA}\",\"sha256\":\"{SHA}\""),
            ),
            "sha256",
        ),
        repeated(
            good.replace("\"bytes\":7,", "\"bytes\":99,\"bytes\":7,"),
            "bytes",
        ),
        repeated(
            good.replace("\"packages\":[", "\"packages\":[],\"packages\":["),
            "packages",
        ),
    ];
    assert_refusals(&cases);
}

/// Proves a key that appears once per object, even if the same name is in several objects, is
/// not mistaken for a repeat. Catches a duplicate check that shares keys between the index and
/// its entries.
#[test]
fn the_same_key_in_different_objects_of_the_index_is_accepted() {
    let (service, version) = asked();
    let text = document_of(&[
        good_package(Platform::LinuxArm64),
        good_package(Platform::LinuxX86_64),
    ]);
    assert!(ReleaseIndex::parse(text.as_bytes(), &service, &version).is_ok());
}
