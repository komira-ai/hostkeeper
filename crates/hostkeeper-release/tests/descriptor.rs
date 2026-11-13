//! The rules of `hostkeeper_release::descriptor` (docs/deploy/package-repositories.md, "Structure
//! of a repository"): the komira example parses, and each rule refuses with its own message.

use hostkeeper_release::descriptor::{self, DescriptorError, MAX_BYTES, check};
use serde_json::{Value, json};

const PRIMARY: &str = "CB09CF205649494A92DFC22C70D1F2A25906CB22";
const SUBKEY: &str = "F767235D83B0410CFAF4779E8738AD1BE061EEDD";

/// The descriptor packages.komira.ai serves.
fn komira() -> Value {
    json!({
        "version": 1,
        "name": "komira",
        "description": "komira-ai packages",
        "channels": ["testing", "stable"],
        "key": {
            "keyring": "apt/komira-archive-keyring.gpg",
            "fingerprints": [PRIMARY, SUBKEY]
        },
        "formats": {
            "apt": { "path": "apt", "components": ["main"], "architectures": ["amd64", "arm64"] },
            "rpm": { "path": "rpm", "architectures": ["x86_64", "aarch64"] },
            "macos-pkg": {
                "path": "pkg",
                "platforms": ["macos-arm64"],
                "packages": { "hostkeeper": { "dir": "hostkeeper" }, "hello": { "dir": "hostkeeper" } }
            }
        }
    })
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("json")
}

/// The komira descriptor with `change` applied.
fn with(change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value = komira();
    change(&mut value);
    bytes(&value)
}

fn refused(raw: &[u8]) -> String {
    check(raw).expect_err("a refused descriptor").to_string()
}

/// Proves the example the organisation serves parses into every field, derives its platforms and
/// the apt root, and maps hello's installers to hostkeeper's directory.
#[test]
fn the_komira_example_parses() {
    let parsed = check(&bytes(&komira())).expect("parses");
    assert_eq!(parsed.name, "komira");
    assert_eq!(parsed.description, "komira-ai packages");
    assert_eq!(parsed.channels, ["testing", "stable"]);
    assert_eq!(parsed.keyring, "apt/komira-archive-keyring.gpg");
    assert_eq!(parsed.fingerprints, [PRIMARY, SUBKEY]);
    assert_eq!(parsed.apt.path, "apt");
    assert_eq!(parsed.apt.components, ["main"]);
    assert_eq!(parsed.apt.architectures, ["amd64", "arm64"]);
    let rpm = parsed.rpm.as_ref().expect("rpm");
    assert_eq!((rpm.path.as_str(), rpm.architectures.len()), ("rpm", 2));
    let mac = parsed.macos_pkg.as_ref().expect("macos");
    assert_eq!(mac.path, "pkg");
    assert_eq!(mac.platforms, ["macos-arm64"]);
    assert_eq!(mac.dir("hello"), "hostkeeper");
    assert_eq!(mac.dir("tierstore"), "tierstore");
    assert_eq!(
        parsed.platforms(),
        ["linux-x86_64", "linux-arm64", "macos-arm64"]
    );
    assert_eq!(
        parsed.apt_uri("https://packages.komira.ai"),
        "https://packages.komira.ai/apt"
    );
}

/// Proves the smallest descriptor of version 1 (apt only, no name, no description, no
/// architectures) parses, serves both Linux platforms and no macOS one.
#[test]
fn the_smallest_descriptor_is_apt_only_and_serves_both_linux_platforms() {
    let raw = bytes(&json!({
        "version": 1,
        "channels": ["stable"],
        "key": { "keyring": "key.gpg", "fingerprints": [PRIMARY] },
        "formats": { "apt": { "path": "debian", "components": ["main"] } }
    }));
    let parsed = check(&raw).expect("parses");
    assert_eq!(parsed.name, "");
    assert_eq!(parsed.description, "");
    assert!(parsed.rpm.is_none() && parsed.macos_pkg.is_none());
    assert_eq!(parsed.platforms(), ["linux-x86_64", "linux-arm64"]);
}

/// Proves the apt architectures pick the Linux platforms: `arm64` alone serves `linux-arm64`.
#[test]
fn the_apt_architectures_pick_the_linux_platforms() {
    let parsed = check(&with(|v| {
        v["formats"]["apt"]["architectures"] = json!(["arm64"]);
        v["formats"]["macos-pkg"] = Value::Null;
        v["formats"].as_object_mut().expect("o").remove("macos-pkg");
    }))
    .expect("parses");
    assert_eq!(parsed.platforms(), ["linux-arm64"]);
}

/// Proves a key the version 1 reader does not know, at any level, is ignored.
#[test]
fn unknown_keys_are_ignored_at_every_level() {
    let parsed = check(&with(|v| {
        v["later"] = json!({"x": 1});
        v["key"]["rotation"] = json!("soon");
        v["formats"]["apt"]["pool"] = json!("pool");
        v["formats"]["macos-pkg"]["packages"]["hello"]["note"] = json!("x");
        v["formats"]["oci"] = json!({"path": "oci"});
    }))
    .expect("parses");
    assert_eq!(parsed.channels, ["testing", "stable"]);
}

/// Proves the size limit: the descriptor at the limit is read as JSON, one byte over is refused
/// by size, with the exact message.
#[test]
fn a_descriptor_over_64_kib_is_refused_by_size() {
    let mut big = bytes(&komira());
    big.resize(MAX_BYTES, b' ');
    assert_eq!(check(&big).map(|d| d.name), Ok("komira".to_owned()));
    big.push(b' ');
    assert_eq!(
        refused(&big),
        "the descriptor is 65537 bytes, at most 65536"
    );
}

/// Proves text that is not JSON, or JSON that is not an object, is refused with its own message.
#[test]
fn text_that_is_not_a_json_object_is_refused() {
    assert!(refused(b"nope").starts_with("the descriptor is not JSON: "));
    assert_eq!(refused(b"[1]"), "the descriptor is not a JSON object");
}

/// Proves the version rules: missing, zero, negative, fractional and text are refused as no
/// version, a higher one as newer than this coordinator reads.
#[test]
fn the_version_is_a_whole_number_from_one_and_no_higher() {
    for version in [json!(null), json!(0), json!(-1), json!(1.5), json!("1")] {
        let raw = with(|v| v["version"] = version.clone());
        assert_eq!(
            refused(&raw),
            "the descriptor's version is missing or not a whole number from 1",
            "{version}"
        );
    }
    let raw = with(|v| {
        v.as_object_mut().expect("o").remove("version");
    });
    assert_eq!(check(&raw), Err(DescriptorError::Version));
    assert_eq!(
        refused(&with(|v| v["version"] = json!(2))),
        "the descriptor is version 2, newer than this coordinator reads (1)"
    );
    assert_eq!(descriptor::VERSION, 1);
}

/// Proves the name and the description rules, each by its message, and a name or description of
/// the wrong type.
#[test]
fn the_name_and_the_description_are_bounded() {
    assert_eq!(
        refused(&with(|v| v["name"] = json!("Komira"))),
        "name: \"Komira\" is not 1 to 32 bytes of [a-z0-9-]"
    );
    assert_eq!(
        refused(&with(|v| v["name"] = json!("a".repeat(33)))),
        format!(
            "name: \"{}\" is not 1 to 32 bytes of [a-z0-9-]",
            "a".repeat(33)
        )
    );
    assert_eq!(
        refused(&with(|v| v["name"] = json!(7))),
        "name is missing or of the wrong type"
    );
    assert_eq!(
        refused(&with(|v| v["description"] = json!("d".repeat(201)))),
        "description: 201 bytes, at most 200"
    );
    let ok = with(|v| v["description"] = json!("d".repeat(200)));
    assert!(check(&ok).is_ok());
}

/// Proves the channels: 1 to 8, each a suite name, none twice, an array of texts.
#[test]
fn the_channels_are_one_to_eight_distinct_suite_names() {
    let cases: [(Value, &str); 6] = [
        (json!([]), "channels: 0 entries, not 1 to 8"),
        (
            json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
            "channels: 9 entries, not 1 to 8",
        ),
        (
            json!(["Testing"]),
            "channels: \"Testing\" breaks the rule of its form",
        ),
        (
            json!(["a".repeat(32)]),
            &format!(
                "channels: \"{}\" breaks the rule of its form",
                "a".repeat(32)
            ),
        ),
        (json!(["a", "a"]), "channels: \"a\" is listed twice"),
        (json!([1]), "channels is missing or of the wrong type"),
    ];
    for (channels, message) in cases {
        assert_eq!(
            refused(&with(|v| v["channels"] = channels.clone())),
            message
        );
    }
    assert_eq!(
        refused(&with(|v| v["channels"] = json!("testing"))),
        "channels is missing or of the wrong type"
    );
    assert_eq!(
        refused(&with(|v| {
            v.as_object_mut().expect("o").remove("channels");
        })),
        "channels is missing or of the wrong type"
    );
}

/// Proves the key: the keyring is a locator (no absolute path, no `..`, no scheme, no empty
/// text), the fingerprints are 1 to 8 distinct upper-case hex strings of 40 or 64 characters.
#[test]
fn the_key_is_a_relative_keyring_and_well_formed_fingerprints() {
    for (keyring, message) in [
        (
            "/apt/key.gpg",
            "key.keyring has an empty segment (a leading, trailing or doubled `/`)",
        ),
        ("apt/../key.gpg", "key.keyring has a `.` or `..` segment"),
        (
            "https://example.com/key.gpg",
            "key.keyring has an empty segment (a leading, trailing or doubled `/`)",
        ),
        ("", "key.keyring is empty"),
    ] {
        assert_eq!(
            refused(&with(|v| v["key"]["keyring"] = json!(keyring))),
            message,
            "{keyring}"
        );
    }
    for (prints, message) in [
        (
            json!([]),
            "key.fingerprints: 0 entries, not 1 to 8".to_owned(),
        ),
        (
            json!([PRIMARY.to_lowercase()]),
            format!(
                "key.fingerprints: \"{}\" breaks the rule of its form",
                PRIMARY.to_lowercase()
            ),
        ),
        (
            json!(["ABCD"]),
            "key.fingerprints: \"ABCD\" breaks the rule of its form".to_owned(),
        ),
        (
            json!([PRIMARY, PRIMARY]),
            format!("key.fingerprints: \"{PRIMARY}\" is listed twice"),
        ),
    ] {
        assert_eq!(
            refused(&with(|v| v["key"]["fingerprints"] = prints.clone())),
            message
        );
    }
    let v6 = "A".repeat(64);
    let ok = with(|v| v["key"]["fingerprints"] = json!([v6]));
    assert_eq!(check(&ok).map(|d| d.fingerprints), Ok(vec![v6]));
    assert_eq!(
        refused(&with(|v| {
            v.as_object_mut().expect("o").remove("key");
        })),
        "key is missing or of the wrong type"
    );
    assert_eq!(
        refused(&with(|v| v["key"]["keyring"] = json!(3))),
        "key.keyring is missing or of the wrong type"
    );
}

/// Proves version 1 needs an apt format, with its path a locator and its components 1 to 16
/// well-formed distinct names, its architectures well-formed.
#[test]
fn the_apt_format_is_required_and_judged() {
    assert_eq!(
        check(&with(|v| {
            v["formats"].as_object_mut().expect("o").remove("apt");
        })),
        Err(DescriptorError::NoApt)
    );
    assert_eq!(
        refused(&with(|v| {
            v["formats"].as_object_mut().expect("o").remove("apt");
        })),
        "formats.apt is required: version 1 describes an apt repository"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["apt"]["path"] = json!("../apt"))),
        "formats.apt.path has a `.` or `..` segment"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["apt"]["components"] = json!([]))),
        "formats.apt.components: 0 entries, not 1 to 16"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["apt"]["components"] = json!(["a b"]))),
        "formats.apt.components: \"a b\" breaks the rule of its form"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["apt"]["architectures"] = json!(["AMD"])
        )),
        "formats.apt.architectures: \"AMD\" breaks the rule of its form"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["apt"]["architectures"] = json!(vec!["a"; 17])
        )),
        "formats.apt.architectures: 17 entries, not 0 to 16"
    );
    assert_eq!(
        refused(&with(|v| v["formats"] = json!({}))),
        "formats.apt is required: version 1 describes an apt repository"
    );
    assert_eq!(
        refused(&with(|v| {
            v.as_object_mut().expect("o").remove("formats");
        })),
        "formats is missing or of the wrong type"
    );
}

/// Proves the rpm format, listed for display, is judged: a locator path and well-formed
/// architectures.
#[test]
fn the_rpm_format_is_judged() {
    assert_eq!(
        refused(&with(|v| v["formats"]["rpm"]["path"] = json!("rpm//x"))),
        "formats.rpm.path has an empty segment (a leading, trailing or doubled `/`)"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["rpm"]["architectures"] = json!("x86_64")
        )),
        "formats.rpm.architectures is missing or of the wrong type"
    );
}

/// Proves the macOS format: platforms are Mac platforms (a Linux one or a made up one is
/// refused), the path is a locator, the packages are well-formed names mapped to well-formed
/// directories, at most 64.
#[test]
fn the_macos_format_is_judged() {
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["platforms"] = json!(["linux-arm64"])
        )),
        "formats.macos-pkg.platforms: \"linux-arm64\" breaks the rule of its form"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["platforms"] = json!(["macos-x86"])
        )),
        "formats.macos-pkg.platforms: \"macos-x86\" breaks the rule of its form"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["macos-pkg"]["platforms"] = json!([]))),
        "formats.macos-pkg.platforms: 0 entries, not 1 to 3"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["macos-pkg"]["path"] = json!("pkg?x"))),
        "formats.macos-pkg.path holds `?`; a locator has no query"
    );
    assert_eq!(
        refused(&with(|v| v["formats"]["macos-pkg"]["packages"] = json!([]))),
        "formats.macos-pkg.packages is missing or of the wrong type"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["packages"]["Bad"] = json!({"dir": "x"})
        )),
        "formats.macos-pkg.packages: the package \"Bad\" is not 1 to 64 bytes of [a-z0-9-]"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["packages"]["web"] = json!({"dir": "A/b"})
        )),
        "formats.macos-pkg.packages: the dir \"A/b\" of web is not 1 to 64 bytes of [a-z0-9-]"
    );
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["packages"]["web"] = json!({})
        )),
        "formats.macos-pkg.packages.web.dir is missing or of the wrong type"
    );
    let many: serde_json::Map<String, Value> = (0..65)
        .map(|n| (format!("p{n}"), json!({"dir": "d"})))
        .collect();
    assert_eq!(
        refused(&with(
            |v| v["formats"]["macos-pkg"]["packages"] = Value::Object(many.clone())
        )),
        "formats.macos-pkg.packages: 65 packages, at most 64"
    );
    let none = check(&with(|v| {
        v["formats"]["macos-pkg"]
            .as_object_mut()
            .expect("o")
            .remove("packages");
    }))
    .expect("packages are optional");
    assert_eq!(none.macos_pkg.expect("mac").dir("hello"), "hello");
}
