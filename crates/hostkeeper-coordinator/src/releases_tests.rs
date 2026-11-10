//! The releases a version was set with ([`crate::releases`]): the JSON a release is stored as, the
//! releases file, the cap on the releases kept, the checks of a set's targets, and the reason of
//! the audit entry. The builders at the top are the other tests of
//! releases' too.

use std::sync::Arc;

use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Hook, InstallerBounds, Location, Manifest, StepBounds, Target, TargetList,
};
use hostkeeper_wire::version::Version;
use prost::Message as _;
use prost_reflect::{DescriptorPool, MessageDescriptor};
use prost_types::FileDescriptorSet;
use serde_json::{Value, json};

use crate::desired::DesiredError;
use crate::registry::{Digest, MAX_REGISTRY};
use crate::releases::{
    Book, Books, Release, Targets, audit_reason, checked, encode_file, parse_file, parse_file_by,
    record, releases_path, target_list,
};
use crate::service::SetError;

/// The release stored as `value`, read as the stores read it, by this build's `TargetList`.
fn from_json(value: &Value) -> Result<Release, String> {
    Release::read(value, target_list())
}

/// A digest of 64 lower-case hex characters, all `c`.
pub(crate) fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

/// [`hex`] as a digest.
pub(crate) fn digest(c: char) -> Digest {
    Digest::parse(&hex(c)).expect("a digest")
}

fn location(locator: &str) -> Location {
    Location {
        store: "github".to_owned(),
        locator: locator.to_owned(),
    }
}

/// A manifest that passes every rule a manifest keeps, for `service` on Linux.
fn manifest(service: &str, installer: &str) -> Manifest {
    if installer == "hostkeeper-archive" {
        return Manifest {
            min_hostkeeper: "0.4.0".to_owned(),
            ..Manifest::default()
        };
    }
    Manifest {
        package: (installer == "deb" || installer == "rpm").then(|| service.to_owned()),
        hook: Some(Hook {
            command: vec![format!("bin/{service}"), "update-hook".to_owned()],
            timeout_secs: Some(StepBounds {
                prepare: 0,
                cutover: 10,
                verify: 20,
                rollback: 30,
                cleanup: 40,
                uninstall: None,
            }),
            verify_deadline_secs: 50,
            ..Hook::default()
        }),
        run: vec![format!("{service}.service")],
        min_hostkeeper: "0.4.0".to_owned(),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 120,
            remove_secs: 300,
        }),
        ..Manifest::default()
    }
}

/// An entry of `installer` for `service` whose artifact's digest is all `c` and whose size is
/// `bytes`.
pub(crate) fn entry(service: &str, installer: &str, c: char, bytes: u64) -> Target {
    Target {
        installer: installer.to_owned(),
        locations: vec![location(&format!(
            "example-org/{service}/releases/download/v2.0/{service}-{installer}"
        ))],
        digest: hex(c),
        bytes,
        attestation: Some(Blob {
            digest: hex('e'),
            bytes: 7311,
            locations: vec![location(&format!(
                "example-org/{service}/releases/download/v2.0/{service}.sigstore.json"
            ))],
        }),
        manifest: Some(manifest(service, installer)),
        ..Target::default()
    }
}

/// An `apt` entry of `service` from the repository `komira-testing`, pinned by the digest
/// [`crate::marks_support::digest`] `n`, as a release lists it: no location, no size and no
/// attestation, and its hook the package's own program.
pub(crate) fn apt_entry(service: &str, n: u8) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: crate::marks_support::digest(n),
        manifest: Some(Manifest {
            package: Some(service.to_owned()),
            hook: Some(Hook {
                command: Vec::new(),
                timeout_secs: Some(StepBounds {
                    prepare: 0,
                    cutover: 10,
                    verify: 20,
                    rollback: 30,
                    cleanup: 40,
                    uninstall: None,
                }),
                verify_deadline_secs: 50,
                ..Hook::default()
            }),
            run: vec![format!("{service}.service")],
            min_hostkeeper: "0.4.0".to_owned(),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 120,
                remove_secs: 300,
            }),
            ..Manifest::default()
        }),
        repository: Some("komira-testing".to_owned()),
        package_version: Some(format!("1:{n}.0-1")),
        ..Target::default()
    }
}

/// The list of `entries`.
pub(crate) fn list(entries: Vec<Target>) -> TargetList {
    TargetList { entries }
}

/// Targets of `service`: on `linux-x86_64` a `deb` and a `tarball`, on `macos-arm64` a `tarball`.
pub(crate) fn targets(service: &str) -> Targets {
    Targets::from([
        (
            "linux-x86_64".to_owned(),
            list(vec![
                entry(service, "deb", 'a', 4_000),
                entry(service, "tarball", 'c', 3_000),
            ]),
        ),
        (
            "macos-arm64".to_owned(),
            list(vec![entry(service, "tarball", 'd', 2_000)]),
        ),
    ])
}

/// Targets of `service` with a `hostkeeper-archive` entry on `linux-x86_64` (after a `deb`; digest
/// all `1`, 1111 bytes) and on `macos-arm64` (digest all `2`, 2222 bytes).
pub(crate) fn archive_targets(service: &str) -> Targets {
    Targets::from([
        (
            "linux-x86_64".to_owned(),
            list(vec![
                entry(service, "deb", 'a', 4_000),
                entry(service, "hostkeeper-archive", '1', 1_111),
            ]),
        ),
        (
            "macos-arm64".to_owned(),
            list(vec![entry(service, "hostkeeper-archive", '2', 2_222)]),
        ),
    ])
}

/// The text of a release index of `service` at `version` listing `packages`: platform, digest
/// character and size. The coordinator does not read an index's text, so it is any text that
/// differs with its arguments.
pub(crate) fn index_of(service: &str, version: &str, packages: &[(Platform, char, u64)]) -> String {
    let entries: Vec<Value> = packages
        .iter()
        .map(|(platform, c, bytes)| json!({"platform": platform.as_str(), "sha256": hex(*c), "bytes": bytes}))
        .collect();
    json!({"service": service, "version": version, "packages": entries}).to_string()
}

/// The text of the index of [`archive_targets`].
pub(crate) fn agreeing_index(service: &str, version: &str) -> String {
    index_of(
        service,
        version,
        &[
            (Platform::LinuxX86_64, '1', 1_111),
            (Platform::MacosArm64, '2', 2_222),
        ],
    )
}

fn v(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

/// Proves: a release survives being stored as JSON and read back: the same targets in the same
/// order, the same index digest, and written again as the same JSON; a release with no index has
/// no `release_index_digest` key.
#[test]
fn a_release_is_read_back_from_its_json_as_it_was() {
    let with = Release::new(targets("web"), Some(digest('9')));
    let stored = with.to_json();
    assert_eq!(stored["release_index_digest"], json!(hex('9')));
    assert_eq!(
        stored["targets"]["linux-x86_64"]["entries"][0]["installer"],
        "deb"
    );
    let read = from_json(&stored).expect("reads");
    assert_eq!(read, with);
    assert_eq!(read.targets(), with.targets());
    assert_eq!(read.index_digest(), Some(&digest('9')));
    assert_eq!(read.to_json(), stored);

    let without = Release::new(targets("web"), None);
    let stored = without.to_json();
    assert_eq!(stored.as_object().expect("an object").len(), 1);
    assert_eq!(from_json(&stored).expect("reads").index_digest(), None);
}

/// Proves: two releases are one when their targets encode alike and they name one index digest;
/// another entry order, another digest of an entry, another index digest or an index against none
/// makes another release.
#[test]
fn releases_are_equal_by_encoded_targets_and_index_digest() {
    let one = Release::new(targets("web"), Some(digest('9')));
    assert_eq!(one, Release::new(targets("web"), Some(digest('9'))));
    assert_ne!(one, Release::new(targets("web"), Some(digest('8'))));
    assert_ne!(one, Release::new(targets("web"), None));
    let mut reversed = targets("web");
    reversed
        .get_mut("linux-x86_64")
        .expect("a list")
        .entries
        .reverse();
    assert_ne!(one, Release::new(reversed, Some(digest('9'))));
    let mut other = targets("web");
    other.get_mut("macos-arm64").expect("a list").entries[0].digest = hex('f');
    assert_ne!(one, Release::new(other, Some(digest('9'))));
}

/// Proves: a stored release that is no release is refused with the reason by exact text: no
/// `targets`, `targets` that is no object, a list that does not read (named by its platform),
/// and an index digest that is no digest, whether text or not.
#[test]
fn a_stored_release_that_is_none_is_refused_by_reason() {
    let refused = |value: Value| from_json(&value).expect_err("refused");
    assert_eq!(refused(json!({})), "no targets");
    assert_eq!(refused(json!({"targets": []})), "no targets");
    assert_eq!(
        refused(json!({"targets": {"linux-x86_64": 5}})),
        "the list of linux-x86_64 does not read"
    );
    assert_eq!(
        refused(json!({"targets": {"linux-x86_64": {"entries": [{"bytes": "many"}]}}})),
        "the list of linux-x86_64 does not read"
    );
    for digest in [json!("abc"), json!(5), json!(hex('F'))] {
        assert_eq!(
            refused(json!({"targets": {}, "release_index_digest": digest})),
            "a release_index_digest that is no digest"
        );
    }
    let empty = from_json(&json!({"targets": {}})).expect("no platform reads");
    assert!(empty.targets().is_empty());
}

/// Proves: a field of a stored list this build does not know is left out of the targets it reads,
/// and the release is written back exactly as it was read, so a coordinator rolled back keeps what
/// a newer one wrote; the same holds through the releases file.
#[test]
fn a_field_this_build_does_not_know_is_kept_as_read() {
    let mut stored = Release::new(targets("web"), Some(digest('9'))).to_json();
    stored["targets"]["linux-x86_64"]["future_list_field"] = json!({"a": [1, 2]});
    stored["targets"]["linux-x86_64"]["entries"][0]["future"] = json!("later");
    let read = from_json(&stored).expect("reads");
    assert_eq!(read.targets(), &targets("web"));
    assert_eq!(read.to_json(), stored);

    let books = Books::from([(
        "web".to_owned(),
        Book::from([("2.0".to_owned(), Arc::new(read))]),
    )]);
    let again = parse_file(&encode_file(&books).expect("encodes")).expect("parses");
    assert_eq!(again["web"]["2.0"].to_json(), stored);
}

/// Proves: the releases file is `{"services": {service: {version: release}}}` in pretty JSON with
/// a final newline, by an exact golden text for a small release, and reads back as the same books;
/// no releases are `{"services": {}}`.
#[test]
fn the_releases_file_is_written_as_this_text_and_read_back() {
    let release = Release::new(
        Targets::from([(
            "linux-x86_64".to_owned(),
            list(vec![Target {
                installer: "tarball".to_owned(),
                locations: vec![location("o/r/a")],
                digest: hex('a'),
                bytes: 9,
                attestation: None,
                manifest: None,
                ..Target::default()
            }]),
        )]),
        Some(digest('9')),
    );
    let books = Books::from([(
        "web".to_owned(),
        Book::from([("2.0".to_owned(), Arc::new(release))]),
    )]);
    let text = String::from_utf8(encode_file(&books).expect("encodes")).expect("text");
    let golden = r#"{
  "services": {
    "web": {
      "2.0": {
        "release_index_digest": "9999999999999999999999999999999999999999999999999999999999999999",
        "targets": {
          "linux-x86_64": {
            "entries": [
              {
                "bytes": "9",
                "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "installer": "tarball",
                "locations": [
                  {
                    "locator": "o/r/a",
                    "store": "github"
                  }
                ]
              }
            ]
          }
        }
      }
    }
  }
}
"#;
    assert_eq!(text, golden);
    let read = parse_file(text.as_bytes()).expect("parses");
    assert_eq!(read["web"]["2.0"], books["web"]["2.0"]);
    assert_eq!(
        encode_file(&Books::new()).expect("encodes"),
        b"{\n  \"services\": {}\n}\n"
    );
}

/// Proves: a releases file that is not the file is refused by exact reason: not JSON, no
/// `services`, a service that holds no versions, a version that is no version, a release that does
/// not read (named by service and version); a service whose book is empty is not listed.
#[test]
fn a_releases_file_that_is_none_is_refused_by_reason() {
    let refused = |value: &Value| {
        parse_file(value.to_string().as_bytes())
            .expect_err("refused")
            .to_string()
    };
    assert_eq!(
        parse_file(b"nope").expect_err("refused").to_string(),
        "the desired state is malformed: the releases: expected ident at line 1 column 2"
    );
    assert_eq!(
        refused(&json!({"service": {}})),
        "the desired state is malformed: the releases: no services"
    );
    assert_eq!(
        refused(&json!({"services": []})),
        "the desired state is malformed: the releases: no services"
    );
    assert_eq!(
        refused(&json!({"services": {"web": 5}})),
        "the desired state is malformed: the releases: web holds no versions"
    );
    let version_error = Version::parse("x1").expect_err("no version");
    assert_eq!(
        refused(&json!({"services": {"web": {"x1": {"targets": {}}}}})),
        format!("the desired state is malformed: the releases: web x1: {version_error}")
    );
    assert_eq!(
        refused(&json!({"services": {"web": {"2.0": {"targets": 3}}}})),
        "the desired state is malformed: the releases: web 2.0: no targets"
    );
    let books = parse_file(
        json!({"services": {"web": {}, "db": {"1.0": {"targets": {}}}}})
            .to_string()
            .as_bytes(),
    )
    .expect("parses");
    assert_eq!(books.keys().collect::<Vec<_>>(), ["db"]);
}

/// Proves: the releases file of a state file is the state file's name and `.releases`.
#[test]
fn the_releases_file_is_beside_the_state_file() {
    assert_eq!(
        releases_path(std::path::Path::new("/state/desired.json")),
        std::path::PathBuf::from("/state/desired.json.releases")
    );
}

/// A book of `count` releases of no targets, versions `1.<first>` on.
fn book(first: usize, count: usize) -> Book {
    let release = Arc::new(Release::new(Targets::new(), None));
    (first..first + count)
        .map(|n| (format!("1.{n}"), Arc::clone(&release)))
        .collect()
}

/// Proves: a book holds at most `MAX_REGISTRY` releases: one past it drops the oldest by release
/// numbers (`1.9` before `1.10`, not by text) and keeps the rest and the release set.
#[test]
fn the_oldest_release_past_the_cap_is_dropped() {
    let mut full = book(9, MAX_REGISTRY);
    assert!(full.contains_key("1.9") && full.contains_key("1.10"));
    let release = Arc::new(Release::new(targets("web"), None));
    record(&mut full, &v("2.0"), release.clone(), &|_| false);
    assert_eq!(full.len(), MAX_REGISTRY);
    assert!(!full.contains_key("1.9"));
    assert!(full.contains_key("1.10"));
    assert!(Arc::ptr_eq(&full["2.0"], &release));
}

/// Proves: the release being set is never the one dropped, even when it is the oldest: the oldest
/// other release goes instead.
#[test]
fn the_release_being_set_is_never_dropped() {
    let mut full = book(0, MAX_REGISTRY);
    let release = Arc::new(Release::new(targets("web"), None));
    record(&mut full, &v("0.1"), release.clone(), &|_| false);
    assert_eq!(full.len(), MAX_REGISTRY);
    assert!(Arc::ptr_eq(&full["0.1"], &release));
    assert!(!full.contains_key("1.0"));
}

/// Proves: a release a mark keeps is not dropped: the oldest one not kept goes; when every other
/// release is kept, none is dropped and the book grows past the cap.
#[test]
fn a_release_kept_by_a_mark_is_not_dropped() {
    let release = Arc::new(Release::new(Targets::new(), None));
    let mut full = book(0, MAX_REGISTRY);
    record(&mut full, &v("2.0"), release.clone(), &|v| {
        v.as_str() == "1.0"
    });
    assert_eq!(full.len(), MAX_REGISTRY);
    assert!(full.contains_key("1.0"));
    assert!(!full.contains_key("1.1"));

    let mut kept = book(0, MAX_REGISTRY);
    record(&mut kept, &v("2.0"), release, &|_| true);
    assert_eq!(kept.len(), MAX_REGISTRY + 1);
    assert!(kept.contains_key("1.0"));
}

/// Proves: a book built one release at a time holds the cap: the release past it drops exactly
/// the oldest.
#[test]
fn a_book_built_release_by_release_holds_the_cap() {
    let release = Arc::new(Release::new(Targets::new(), None));
    let mut built = Book::new();
    for n in 0..=MAX_REGISTRY {
        record(&mut built, &v(&format!("1.{n}")), release.clone(), &|_| {
            false
        });
    }
    assert_eq!(built.len(), MAX_REGISTRY);
    assert!(!built.contains_key("1.0"));
    assert!(built.contains_key(&format!("1.{MAX_REGISTRY}")));
}

fn refusal(why: Result<Release, SetError>) -> String {
    why.expect_err("refused").to_string()
}

/// Proves: targets are judged before an index: with a package digest beside them, with a name that
/// is no service, by the rules of the release (`target_invalid`, here no platform), in that order.
#[test]
fn targets_are_refused_for_a_package_digest_a_service_and_the_rules() {
    assert_eq!(
        refusal(checked("web", targets("web"), true, None)),
        "index_invalid: targets are not set with a package digest"
    );
    assert_eq!(
        refusal(checked("Not A Service", targets("web"), false, None)),
        DesiredError::UnknownService.to_string()
    );
    assert_eq!(
        refusal(checked("web", Targets::new(), false, None)),
        "target_invalid: the targets name no platform"
    );
    let ok = checked("web", targets("web"), false, None).expect("a release");
    assert_eq!(ok, Release::new(targets("web"), None));
}

/// Proves: a release index set beside the targets is kept as its digest alone: the release holds
/// the targets as set and the digest, whatever the targets and whatever text the index had.
#[test]
fn an_index_set_beside_targets_is_kept_as_its_digest() {
    let ok = checked("web", archive_targets("web"), false, Some(&digest('9'))).expect("kept");
    assert_eq!(ok.index_digest(), Some(&digest('9')));
    assert_eq!(ok.targets(), &archive_targets("web"));
    let none = checked("web", targets("web"), false, None).expect("kept");
    assert_eq!(none.index_digest(), None);
}

/// The SHA-256 of the encoding of the manifest `min_hostkeeper = "0.4.0"` alone.
const ARCHIVE_MANIFEST: &str = "ef988f5193350fca65f09260a1a6a02deacbb965292afdd4a760e03de317ea00";
/// The SHA-256 of no bytes: the manifest of an entry that has none.
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Proves: the audit reason names the version, the workflow when CI set it, the release index
/// when there is one, then each platform in spelling order with its entries in list order, each as
/// installer, digest and `manifest` with the SHA-256 of the manifest's encoding (that of no bytes
/// for an entry without one), platforms apart by `; ` and entries by `, `.
#[test]
fn the_audit_reason_names_the_version_the_workflow_the_index_and_every_entry() {
    let bare = |installer: &str, c: char| Target {
        installer: installer.to_owned(),
        digest: hex(c),
        ..Target::default()
    };
    let targets = Targets::from([
        // Spelling order: linux-arm64 before linux-x86_64, whatever the order of insertion.
        (
            "linux-x86_64".to_owned(),
            list(vec![bare("tarball", 'b'), bare("deb", 'a')]),
        ),
        ("linux-arm64".to_owned(), list(vec![bare("rpm", 'c')])),
    ]);
    let platforms = format!(
        "linux-arm64 rpm {} manifest {EMPTY}; linux-x86_64 tarball {} manifest {EMPTY}, deb {} manifest {EMPTY}",
        hex('c'),
        hex('b'),
        hex('a')
    );
    let release = Release::new(targets.clone(), None);
    assert_eq!(
        audit_reason(&v("2.0"), &release, None),
        format!("2.0 with targets: {platforms}")
    );
    assert_eq!(
        audit_reason(
            &v("2.0"),
            &release,
            Some("org/repo/set.yml@refs/heads/main")
        ),
        format!("2.0 with targets by workflow org/repo/set.yml@refs/heads/main: {platforms}")
    );
    let indexed = Release::new(targets, Some(digest('9')));
    assert_eq!(
        audit_reason(&v("2.0"), &indexed, None),
        format!(
            "2.0 with targets and release index {}: {platforms}",
            hex('9')
        )
    );
    assert_eq!(
        audit_reason(&v("2.0"), &indexed, Some("w@r")),
        format!(
            "2.0 with targets by workflow w@r and release index {}: {platforms}",
            hex('9')
        )
    );
}

/// Proves: an entry's manifest is named by the SHA-256 of its protobuf encoding, by a golden
/// digest of the encoding of a manifest with one field set.
#[test]
fn the_audit_reason_names_a_manifest_by_the_sha256_of_its_encoding() {
    let release = Release::new(
        Targets::from([(
            "linux-x86_64".to_owned(),
            list(vec![Target {
                installer: "hostkeeper-archive".to_owned(),
                digest: hex('a'),
                manifest: Some(Manifest {
                    min_hostkeeper: "0.4.0".to_owned(),
                    ..Manifest::default()
                }),
                ..Target::default()
            }]),
        )]),
        None,
    );
    assert_eq!(
        audit_reason(&v("2.0"), &release, None),
        format!(
            "2.0 with targets: linux-x86_64 hostkeeper-archive {} manifest {ARCHIVE_MANIFEST}",
            hex('a')
        )
    );
}

/// The descriptor of `TargetList` that a coordinator before packages-repositories.md's A16 reads
/// a release's lists with: this build's, with fields 7 (`repository`) and 8 (`package_version`)
/// of `Target` and their synthetic oneofs taken out.
pub(crate) fn target_list_before() -> MessageDescriptor {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes");
    let mut stripped = 0;
    for file in &mut set.file {
        if file.name() != "hostkeeper/release/v1/release.proto" {
            continue;
        }
        for message in &mut file.message_type {
            if message.name() == "Target" {
                let before = message.field.len();
                message.field.retain(|field| field.number() < 7);
                stripped += before - message.field.len();
                message.oneof_decl.clear();
            }
        }
    }
    assert_eq!(
        stripped, 2,
        "this build's Target has fields 7 and 8 and no other past 6"
    );
    DescriptorPool::from_file_descriptor_set(set)
        .expect("the release before's set builds")
        .get_message_by_name(&format!("hostkeeper.release.v1.{}", "TargetList"))
        .expect("TargetList")
}

/// A repository entry of `service` (packages-repositories.md section 2.1): `apt` from
/// `komira-testing` at `1:1.6.0-1`, pinned by the digest all `c`; no artifact field.
pub(crate) fn repository_entry(service: &str, c: char) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: hex(c),
        manifest: Some(Manifest {
            package: Some(service.to_owned()),
            ..manifest(service, "apt")
        }),
        repository: Some("komira-testing".to_owned()),
        package_version: Some("1:1.6.0-1".to_owned()),
        ..Target::default()
    }
}

/// Targets of `service` with a repository entry then a tarball on `linux-x86_64`.
pub(crate) fn repository_targets(service: &str) -> Targets {
    Targets::from([(
        "linux-x86_64".to_owned(),
        list(vec![
            repository_entry(service, 'a'),
            entry(service, "tarball", 'c', 3_000),
        ]),
    )])
}

/// Proves: a release with a repository entry is stored with the entry's `repository` and
/// `packageVersion` keys, as proto3 JSON spells them, beside its pin and manifest and no artifact
/// key, and reads back equal.
///
/// Catches: either field left out of the stored JSON, written under another key, or lost on the
/// way back.
#[test]
fn a_repository_entry_is_stored_with_its_repository_and_package_version() {
    let release = Release::new(repository_targets("web"), None);
    let stored = release.to_json();
    let first = &stored["targets"]["linux-x86_64"]["entries"][0];
    assert_eq!(
        first,
        &json!({
            "installer": "apt",
            "digest": hex('a'),
            "manifest": first["manifest"].clone(),
            "repository": "komira-testing",
            "packageVersion": "1:1.6.0-1",
        })
    );
    assert_eq!(first["manifest"]["package"], json!("web"));
    let read = from_json(&stored).expect("reads");
    assert_eq!(read, release);
    assert_eq!(read.targets(), &repository_targets("web"));
}

/// Proves: the reader of a coordinator before A16 starts on a releases file this build wrote
/// with a repository entry: it reads it, its own copy of the entry has no repository and no
/// package version (section 2.3: an `apt` entry with neither), the rest of the release is as
/// set, and the file it writes back is byte for byte the one this build wrote, which this build
/// reads back with both fields.
///
/// Catches: a releases file with a repository entry that stops an older coordinator from
/// starting, or that it rewrites without the two keys.
#[test]
fn the_previous_coordinators_reader_starts_on_a_releases_file_with_a_repository_entry() {
    let books = Books::from([(
        "web".to_owned(),
        Book::from([(
            "2.0".to_owned(),
            Arc::new(Release::new(repository_targets("web"), Some(digest('9')))),
        )]),
    )]);
    let written = encode_file(&books).expect("encodes");
    let before = target_list_before();
    let read = parse_file_by(&written, Some(&before)).expect("the release before reads the file");
    let release = &read["web"]["2.0"];
    let entries = &release.targets()["linux-x86_64"].entries;
    let first = &entries[0];
    assert_eq!(
        (first.repository.clone(), first.package_version.clone()),
        (None, None)
    );
    assert_eq!(
        (
            first.installer.as_str(),
            first.digest.as_str(),
            first.locations.len()
        ),
        ("apt", hex('a').as_str(), 0)
    );
    assert_eq!(
        entries[1],
        repository_targets("web")["linux-x86_64"].entries[1]
    );
    assert_eq!(release.index_digest(), Some(&digest('9')));
    let rewritten = encode_file(&read).expect("encodes");
    assert_eq!(rewritten, written);
    let again = parse_file(&rewritten).expect("this build reads it back");
    assert_eq!(again["web"]["2.0"].targets(), &repository_targets("web"));
}

/// Proves: the audit reason names a repository entry by its installer, `repository` and its
/// name, `package` and the manifest's package, `package_version` and its version, the pin and
/// the manifest's SHA-256; an artifact entry beside it as before.
///
/// Catches: a repository entry audited as an artifact, with no word of where it comes from.
#[test]
fn the_audit_reason_names_a_repository_entrys_repository_package_and_version() {
    let targets = repository_targets("web");
    let manifest = |at: usize| {
        let entry = &targets["linux-x86_64"].entries[at];
        crate::ops_file::digest(&entry.manifest.as_ref().expect("a manifest").encode_to_vec())
    };
    let release = Release::new(targets.clone(), None);
    assert_eq!(
        audit_reason(&v("2.0"), &release, None),
        format!(
            "2.0 with targets: linux-x86_64 apt repository komira-testing package web \
             package_version 1:1.6.0-1 {} manifest {}, tarball {} manifest {}",
            hex('a'),
            manifest(0),
            hex('c'),
            manifest(1)
        )
    );
    // A repository entry with no manifest (never one a set keeps) is named all the same.
    let bare = Release::new(
        Targets::from([(
            "linux-x86_64".to_owned(),
            list(vec![Target {
                manifest: None,
                package_version: None,
                ..repository_entry("web", 'b')
            }]),
        )]),
        None,
    );
    assert_eq!(
        audit_reason(&v("2.0"), &bare, None),
        format!(
            "2.0 with targets: linux-x86_64 apt repository komira-testing package  \
             package_version  {} manifest {EMPTY}",
            hex('b')
        )
    );
}
