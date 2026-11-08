//! The releases a version was set with ([`crate::releases`]): the JSON a release is stored as, the
//! releases file, the cap on the releases kept, the checks of a set's targets against its release
//! index, and the reason of the audit entry. The builders at the top are the other tests of
//! releases' too.

use std::sync::Arc;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::index::{PackageEntry, ReleaseIndex};
use hostkeeper_config::names::{ServiceName, Version as IndexVersion};
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Hook, InstallerBounds, Location, Manifest, StepBounds, Target, TargetList,
};
use hostkeeper_wire::version::Version;
use serde_json::{Value, json};

use crate::desired::DesiredError;
use crate::registry::{Digest, MAX_REGISTRY};
use crate::releases::{
    Book, Books, Release, Targets, audit_reason, checked, encode_file, parse_file, record,
    releases_path,
};
use crate::service::SetError;

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

/// The text of the release index of `service` at `version` listing `packages`: platform, digest
/// character and size.
pub(crate) fn index_of(service: &str, version: &str, packages: &[(Platform, char, u64)]) -> String {
    let entries = packages
        .iter()
        .map(|(platform, c, bytes)| PackageEntry {
            platform: *platform,
            sha256: Sha256Hex::parse(&hex(*c)).expect("a SHA-256"),
            bytes: *bytes,
        })
        .collect();
    ReleaseIndex::new(
        ServiceName::new(service).expect("a service name"),
        IndexVersion::new(version).expect("a version"),
        entries,
    )
    .expect("an index")
    .to_json()
}

/// The index that agrees with [`archive_targets`].
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
    let read = Release::from_json(&stored).expect("reads");
    assert_eq!(read, with);
    assert_eq!(read.targets(), with.targets());
    assert_eq!(read.index_digest(), Some(&digest('9')));
    assert_eq!(read.to_json(), stored);

    let without = Release::new(targets("web"), None);
    let stored = without.to_json();
    assert_eq!(stored.as_object().expect("an object").len(), 1);
    assert_eq!(
        Release::from_json(&stored).expect("reads").index_digest(),
        None
    );
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
    let refused = |value: Value| Release::from_json(&value).expect_err("refused");
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
    let empty = Release::from_json(&json!({"targets": {}})).expect("no platform reads");
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
    let read = Release::from_json(&stored).expect("reads");
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
    let version = v("2.0");
    assert_eq!(
        refusal(checked("web", &version, targets("web"), true, None)),
        "index_invalid: targets are not set with a package digest"
    );
    assert_eq!(
        refusal(checked(
            "Not A Service",
            &version,
            targets("web"),
            false,
            None
        )),
        DesiredError::UnknownService.to_string()
    );
    assert_eq!(
        refusal(checked("web", &version, Targets::new(), false, None)),
        "target_invalid: the targets name no platform"
    );
    let ok = checked("web", &version, targets("web"), false, None).expect("a release");
    assert_eq!(ok, Release::new(targets("web"), None));
}

/// Proves: an index must be the index of the service at the version and list exactly the
/// `hostkeeper-archive` entries of the targets: each platform with one, its digest and its bytes,
/// no platform more and none fewer; a platform whose list holds another installer only is not
/// listed.
#[test]
fn an_index_must_list_exactly_the_archive_entries() {
    let version = v("2.0");
    let wrong_index = "release_index is not the release index of the service at the version";
    let disagrees = "release_index does not list exactly the archive entries of the targets";
    let try_index = |targets: Targets, text: &str| {
        checked("web", &version, targets, false, Some((text, &digest('9'))))
    };
    let archive = |text: &str| try_index(archive_targets("web"), text);

    let ok = archive(&agreeing_index("web", "2.0")).expect("agrees");
    assert_eq!(ok.index_digest(), Some(&digest('9')));
    assert_eq!(ok.targets(), &archive_targets("web"));

    let index_invalid = |text: &str, why: &str| {
        assert_eq!(
            refusal(archive(text)),
            format!("index_invalid: {why}"),
            "{text}"
        );
    };
    index_invalid(&agreeing_index("web", "2.1"), wrong_index);
    index_invalid(&agreeing_index("db", "2.0"), wrong_index);
    index_invalid("not an index", wrong_index);
    // A platform fewer, a platform more, another digest, other bytes.
    index_invalid(
        &index_of("web", "2.0", &[(Platform::LinuxX86_64, '1', 1_111)]),
        disagrees,
    );
    index_invalid(
        &index_of(
            "web",
            "2.0",
            &[
                (Platform::LinuxArm64, '3', 5),
                (Platform::LinuxX86_64, '1', 1_111),
                (Platform::MacosArm64, '2', 2_222),
            ],
        ),
        disagrees,
    );
    index_invalid(
        &index_of(
            "web",
            "2.0",
            &[
                (Platform::LinuxX86_64, '5', 1_111),
                (Platform::MacosArm64, '2', 2_222),
            ],
        ),
        disagrees,
    );
    index_invalid(
        &index_of(
            "web",
            "2.0",
            &[
                (Platform::LinuxX86_64, '1', 1_112),
                (Platform::MacosArm64, '2', 2_222),
            ],
        ),
        disagrees,
    );

    // Targets with no archive entry on macOS: the index lists the Linux archive alone.
    let mut mixed = archive_targets("web");
    mixed.insert(
        "macos-arm64".to_owned(),
        list(vec![entry("web", "tarball", 'd', 9)]),
    );
    let linux_only = index_of("web", "2.0", &[(Platform::LinuxX86_64, '1', 1_111)]);
    try_index(mixed.clone(), &linux_only).expect("a tarball is not listed");
    assert_eq!(
        refusal(try_index(mixed, &agreeing_index("web", "2.0"))),
        format!("index_invalid: {disagrees}")
    );
    // Targets with no archive entry at all agree with no index that lists a package.
    assert_eq!(
        refusal(try_index(targets("web"), &agreeing_index("web", "2.0"))),
        format!("index_invalid: {disagrees}")
    );
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
