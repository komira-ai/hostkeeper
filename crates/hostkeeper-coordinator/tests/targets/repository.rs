//! Repository entries through the coordinator (packages-repositories.md sections 2.2 and 7, build
//! item A16): each refusal of section 2.2 is `target_invalid` and changes nothing; the list CI
//! sends (`apt` from a repository, then a tarball) is kept as set on both routes, through a
//! restart, with its `repository` and `packageVersion` in the releases file; it is immutable
//! (`release_exists`); and its audit entry names the repository, the package, its version and
//! the pin.

use std::collections::BTreeMap;

use hostkeeper_oidc::testkit::WORKFLOW;
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Target, TargetList};
use prost::Message;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::checks::{INVALID_ARGUMENT, judged, refused, rig_with_web};
use super::{
    GET_RELEASE, OPERATOR, SET_DESIRED, ci_token, gateway, grpc, proto, rig, set, set_json,
};
use crate::support::targets::{digest, entry, list, location, repository_entry};

type Targets = BTreeMap<String, TargetList>;

/// `FAILED_PRECONDITION`.
const FAILED_PRECONDITION: i32 = 9;

/// The list CI sends for `web` on Linux: `apt` from `komira-testing` at `1:1.6.0-1` pinned by
/// the digest all `a`, then a tarball.
fn sent() -> Targets {
    with_repository(repository_entry("web", "1:1.6.0-1", 'a'))
}

/// `sent` with `first` in place of its repository entry.
fn with_repository(first: Target) -> Targets {
    BTreeMap::from([(
        "linux-x86_64".to_owned(),
        list(vec![first, entry("web", "tarball", 'c', 40_000_000)]),
    )])
}

/// The repository entry of `sent` with `change` applied.
fn changed(change: impl FnOnce(&mut Target)) -> Targets {
    let mut first = repository_entry("web", "1:1.6.0-1", 'a');
    change(&mut first);
    with_repository(first)
}

/// What `GetRelease` answers for `web` at `version`.
async fn release(rig: &super::Rig, version: &str) -> proto::GetReleaseResponse {
    let ask = proto::GetReleaseRequest {
        service: "web".to_owned(),
        version: version.to_owned(),
        host_group: String::new(),
    };
    grpc(rig, GET_RELEASE, Some(OPERATOR), &ask).await.decoded()
}

/// The reasons of the `set_desired` entries of the audit log, oldest first.
async fn audit_reasons(rig: &super::Rig) -> Vec<String> {
    let ask = proto::ListAuditRequest {
        action: Some("set_desired".to_owned()),
        ..proto::ListAuditRequest::default()
    };
    let answered = grpc(
        rig,
        "/hostkeeper.operator.v1.Audit/List",
        Some(OPERATOR),
        &ask,
    )
    .await;
    answered
        .decoded::<proto::ListAuditResponse>()
        .entries
        .into_iter()
        .filter_map(|entry| entry.reason)
        .collect()
}

/// The lower-case hex SHA-256 of `bytes`.
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The text of the releases file beside the rig's state file.
fn releases_file(rig: &super::Rig) -> String {
    std::fs::read_to_string(rig.dir.path().join("desired.json.releases")).expect("the file")
}

/// Proves that a set whose repository entry breaks a row of section 2.2 is refused
/// `target_invalid` (`INVALID_ARGUMENT`) for that row, as `hostkeeper-release` states it, and
/// changes no file, adds no audit entry and leaves the version without a release: a repository
/// of 33 bytes and one with an upper-case letter; no package version, one of 129 bytes, one
/// starting with a letter and one with a space; a package version on an entry without a
/// repository; a location, a size of 1 and an attestation; no pin, one of 63 hex digits and one
/// with an upper-case letter; no `manifest.package`, one of 129 bytes and one starting with `-`;
/// the installer `hostkeeper-archive` from a repository; and the service `hostkeeper` with its
/// repository entry after its archive entry (its native entry first is accepted, A18).
///
/// Catches: a row of section 2.2 that the coordinator forgot to ask of a set, or one that a
/// refusal leaves a trace of.
#[tokio::test]
async fn each_row_of_section_2_2_refuses_the_set_target_invalid() {
    let rig = rig_with_web("targets-repository-rules").await;
    let at = "entry 0 of linux-x86_64";
    let long_version = format!("1{}", "a".repeat(128));
    let long_package = "p".repeat(129);
    let attestation = Blob {
        digest: digest('e'),
        bytes: 7311,
        locations: vec![location("o/r/web.sigstore.json")],
    };
    let mut own = repository_entry("hostkeeper", "1.0-1", 'a');
    own.manifest.as_mut().expect("a manifest").run = vec!["hostkeeper.service".to_owned()];
    let rows: Vec<(&str, &str, Targets, String)> = vec![
        (
            "a repository of 33 bytes",
            "web",
            changed(|e| e.repository = Some("r".repeat(33))),
            format!(
                "{at}: the repository {:?} is not 1 to 32 bytes of a-z, 0-9 and `-`",
                "r".repeat(33)
            ),
        ),
        (
            "a repository with an upper-case letter",
            "web",
            changed(|e| e.repository = Some("Komira".to_owned())),
            format!("{at}: the repository \"Komira\" is not 1 to 32 bytes of a-z, 0-9 and `-`"),
        ),
        (
            "no package version",
            "web",
            changed(|e| e.package_version = None),
            format!("{at}: the repository entry has no package_version"),
        ),
        (
            "a package version of 129 bytes",
            "web",
            changed(|e| e.package_version = Some(long_version.clone())),
            format!(
                "{at}: the package_version {long_version:?} is not 1 to 128 bytes of A-Z, a-z, 0-9 and `.+~:_-` with a digit first"
            ),
        ),
        (
            "a package version starting with a letter",
            "web",
            changed(|e| e.package_version = Some("v1.6.0".to_owned())),
            format!(
                "{at}: the package_version \"v1.6.0\" is not 1 to 128 bytes of A-Z, a-z, 0-9 and `.+~:_-` with a digit first"
            ),
        ),
        (
            "a package version with a space",
            "web",
            changed(|e| e.package_version = Some("1.6.0 1".to_owned())),
            format!(
                "{at}: the package_version \"1.6.0 1\" is not 1 to 128 bytes of A-Z, a-z, 0-9 and `.+~:_-` with a digit first"
            ),
        ),
        (
            "a package version without a repository",
            "web",
            with_repository(Target {
                package_version: Some("1.6.0-1".to_owned()),
                ..entry("web", "deb", 'a', 1000)
            }),
            format!("{at}: the entry has a package_version and no repository"),
        ),
        (
            "a location",
            "web",
            changed(|e| e.locations = vec![location("o/r/web.deb")]),
            format!(
                "{at}: the repository entry sets locations, a field of an artifact; it names no artifact"
            ),
        ),
        (
            "a size of 1",
            "web",
            changed(|e| e.bytes = 1),
            format!(
                "{at}: the repository entry sets bytes, a field of an artifact; it names no artifact"
            ),
        ),
        (
            "an attestation",
            "web",
            changed(|e| e.attestation = Some(attestation.clone())),
            format!(
                "{at}: the repository entry sets attestation, a field of an artifact; it names no artifact"
            ),
        ),
        (
            "no pin",
            "web",
            changed(|e| e.digest = String::new()),
            format!("{at}: the package digest \"\" is not 64 lower-case hex characters"),
        ),
        (
            "a pin of 63 hex digits",
            "web",
            changed(|e| e.digest = "a".repeat(63)),
            format!(
                "{at}: the package digest {:?} is not 64 lower-case hex characters",
                "a".repeat(63)
            ),
        ),
        (
            "a pin with an upper-case letter",
            "web",
            changed(|e| e.digest = format!("A{}", "a".repeat(63))),
            format!(
                "{at}: the package digest {:?} is not 64 lower-case hex characters",
                format!("A{}", "a".repeat(63))
            ),
        ),
        (
            "no manifest.package",
            "web",
            changed(|e| e.manifest.as_mut().expect("a manifest").package = None),
            format!("{at}: the manifest of a repository entry needs package"),
        ),
        (
            "a manifest.package of 129 bytes",
            "web",
            changed(|e| {
                e.manifest.as_mut().expect("a manifest").package = Some(long_package.clone());
            }),
            format!(
                "{at}: the package {long_package:?} is not 1 to 128 bytes of A-Z, a-z, 0-9 and `+._@-` with a letter or digit first"
            ),
        ),
        (
            "a manifest.package starting with `-`",
            "web",
            changed(|e| e.manifest.as_mut().expect("a manifest").package = Some("-web".to_owned())),
            format!(
                "{at}: the package \"-web\" is not 1 to 128 bytes of A-Z, a-z, 0-9 and `+._@-` with a letter or digit first"
            ),
        ),
        (
            "hostkeeper-archive from a repository",
            "web",
            changed(|e| e.installer = "hostkeeper-archive".to_owned()),
            format!(
                "{at}: the repository entry has the installer hostkeeper-archive, which installs archives only"
            ),
        ),
        (
            "the service hostkeeper with its repository entry after its archive entry",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![entry("hostkeeper", "hostkeeper-archive", 'b', 1000), own]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry with at most one hostkeeper-archive entry after it, nor one hostkeeper-archive entry"
                .to_owned(),
        ),
    ];
    for (why, service, broken, message) in rows {
        assert_eq!(judged(service, &broken), message, "{why}: the rule");
        refused(
            &rig,
            &set(service, "2.0", broken),
            (INVALID_ARGUMENT, "target_invalid"),
            why,
        )
        .await;
    }
}

/// Proves: the list `[apt from a repository, tarball]` set by CI over gRPC is accepted and
/// answered by `GetRelease` as set; the releases file holds the repository entry with its
/// `repository`, `packageVersion`, pin and manifest and no artifact field; the same list set
/// through the JSON gateway, its keys as proto3 JSON spells them, is kept the same; and a
/// restart answers both releases as set and leaves the releases file byte for byte.
///
/// Catches: a repository entry dropped, or kept without its two fields, by either route, by the
/// releases file, or by a restart.
#[tokio::test]
async fn a_repository_entry_and_a_tarball_are_kept_as_set_on_both_routes_and_through_a_restart() {
    let mut rig = rig("targets-repository-kept");
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set("web", "2.0", sent()),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    assert_eq!(release(&rig, "2.0").await.targets, sent());

    let body = set_json(&set("web", "2.1", sent()));
    let first = &body["targets"]["linux-x86_64"]["entries"][0];
    assert_eq!(first["repository"], "komira-testing");
    assert_eq!(first["packageVersion"], "1:1.6.0-1");
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &body.to_string()).await;
    assert_eq!(code, 200, "{answer}");
    assert_eq!(release(&rig, "2.1").await.targets, sent());

    let text = releases_file(&rig);
    let book: Value = serde_json::from_str(&text).expect("JSON");
    for version in ["2.0", "2.1"] {
        let stored = &book["services"]["web"][version]["targets"]["linux-x86_64"]["entries"][0];
        let mut keys: Vec<&str> = stored
            .as_object()
            .expect("an entry")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "digest",
                "installer",
                "manifest",
                "packageVersion",
                "repository"
            ],
            "{version}"
        );
        assert_eq!(stored["repository"], "komira-testing");
        assert_eq!(stored["packageVersion"], "1:1.6.0-1");
        assert_eq!(stored["digest"], digest('a'));
        assert_eq!(stored["manifest"]["package"], "web");
    }

    rig.restart();
    assert_eq!(release(&rig, "2.0").await.targets, sent());
    assert_eq!(release(&rig, "2.1").await.targets, sent());
    assert_eq!(releases_file(&rig), text);
}

/// Proves: the same repository entry set again under its version is accepted and answers as
/// before; another package version, another pin or another repository under the same version
/// is `release_exists` and leaves the release as it was.
///
/// Catches: a repository entry's version, pin or repository changed after it was set, which
/// would install other bytes on the hosts that stage after the change.
#[tokio::test]
async fn a_repository_release_is_immutable_by_package_version_pin_and_repository() {
    let rig = rig("targets-repository-exists");
    let ci = |targets: Targets| set("web", "2.0", targets);
    let first = grpc(&rig, SET_DESIRED, Some(&ci_token()), &ci(sent())).await;
    assert_eq!(first.code, 0, "{first:?}");
    let again = grpc(&rig, SET_DESIRED, Some(&ci_token()), &ci(sent())).await;
    assert_eq!(again.code, 0, "{again:?}");
    for (why, other) in [
        (
            "another package version",
            changed(|e| e.package_version = Some("1:1.6.0-2".to_owned())),
        ),
        ("another pin", changed(|e| e.digest = digest('b'))),
        (
            "another repository",
            changed(|e| e.repository = Some("komira-stable".to_owned())),
        ),
    ] {
        let answered = grpc(&rig, SET_DESIRED, Some(&ci_token()), &ci(other)).await;
        assert_eq!(
            answered.refusal(),
            (FAILED_PRECONDITION, "release_exists"),
            "{why}"
        );
        assert_eq!(release(&rig, "2.0").await.targets, sent(), "{why}");
    }
}

/// Proves: the audit entry of a set with a repository entry names, for that entry, its
/// installer, `repository` and its name, `package` and the manifest's package,
/// `package_version` and its version, then the pin and the SHA-256 of the manifest's encoding;
/// the tarball beside it is named as every artifact entry is.
///
/// Catches: a repository entry audited as an artifact, with no word of where its package comes
/// from or at which version.
#[tokio::test]
async fn the_audit_entry_names_the_repository_the_package_its_version_and_the_pin() {
    let rig = rig("targets-repository-audit");
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set("web", "2.0", sent()),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let entries = &sent()["linux-x86_64"].entries;
    let manifest = |at: usize| {
        sha(&entries[at]
            .manifest
            .as_ref()
            .expect("a manifest")
            .encode_to_vec())
    };
    assert_eq!(
        audit_reasons(&rig).await,
        [format!(
            "2.0 with targets by workflow {WORKFLOW}: linux-x86_64 apt repository komira-testing \
             package web package_version 1:1.6.0-1 {} manifest {}, tarball {} manifest {}",
            digest('a'),
            manifest(0),
            digest('c'),
            manifest(1)
        )]
    );
}
