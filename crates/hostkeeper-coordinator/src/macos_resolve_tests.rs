//! How a Mac finds the `.pkg` a release names by version ([`crate::macos_resolve`]): the pure
//! resolution, the sources the registry makes of its descriptors, the release the picks read, and
//! the pick of a Mac for it.

use std::collections::BTreeMap;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target, TargetList};
use hostkeeper_release::target::check_entry;
use hostkeeper_wire::version::Version;
use serde_json::json;

use crate::builtin;
use crate::macos_resolve::{Source, has_named, is_named, resolve, resolved, sources_of};
use crate::marks_support::{AGENT, WEB, ask_heard, beat_on, rig, who};
use crate::picks::{COORDINATOR_STORE, Pick, TARGET_FEATURE, pick};
use crate::releases::Release;
use crate::repositories::Registry;
use crate::repositories_support::{komira, macos_entry, repository_entry};
use crate::repository_push::PRIVATE_FEATURE;
use crate::testutil::id;

fn sources() -> BTreeMap<String, Source> {
    sources_of(&Registry::default())
}

/// Proves: a named-version entry of the built-in `komira` resolves to the artifact entry daemons
/// install: the installer, digest, size and manifest as CI sent them, no repository, no version,
/// no attestation, and one location in the `coordinator` store whose locator is
/// `repositories/komira/pkg/<dir>/<version>/<package>-<version>-macos-arm64.pkg`.
///
/// Catches: a locator the coordinator does not serve, a pin or a size changed on the way, or an
/// entry that still names a repository (and would be checked as one).
#[test]
fn named_version_resolves_to_a_coordinator_artifact_entry() {
    let named = macos_entry("hostkeeper", "komira-testing", "0.1.306");
    let resolved = resolve(&named, "hostkeeper", "macos-arm64", &sources()).expect("resolves");
    assert_eq!(
        resolved,
        Target {
            repository: None,
            package_version: None,
            locations: vec![Location {
                store: "coordinator".to_owned(),
                locator:
                    "repositories/komira/pkg/hostkeeper/0.1.306/hostkeeper-0.1.306-macos-arm64.pkg"
                        .to_owned(),
            }],
            ..named.clone()
        }
    );
    assert_eq!(resolved.digest, named.digest);
    assert_eq!(resolved.bytes, 1000);
    assert!(resolved.attestation.is_none());
    assert_eq!(COORDINATOR_STORE, resolved.locations[0].store);
}

/// Proves: hello's installer is found in hostkeeper's directory (the descriptor maps it), and a
/// package the descriptor does not list is found in a directory of its own name.
///
/// Catches: the directory taken from the package's name when the repository keeps it elsewhere.
#[test]
fn hello_resolves_into_the_hostkeeper_dir_and_an_unlisted_package_uses_its_own() {
    let hello = macos_entry("hello", "komira-stable", "0.1.306");
    let found = resolve(&hello, "hello", "macos-arm64", &sources()).expect("resolves");
    assert_eq!(
        found.locations[0].locator,
        "repositories/komira/pkg/hostkeeper/0.1.306/hello-0.1.306-macos-arm64.pkg"
    );
    let tierstore = macos_entry("tierstore", "komira-stable", "0.3.96");
    let found = resolve(&tierstore, "tierstore", "macos-arm64", &sources()).expect("resolves");
    assert_eq!(
        found.locations[0].locator,
        "repositories/komira/pkg/tierstore/0.3.96/tierstore-0.3.96-macos-arm64.pkg"
    );
}

/// Proves: an entry resolves to nothing when its channel is no repository's, when the repository
/// does not build for the platform, when the entry names no version, and when it names no
/// repository.
///
/// Catches: an entry ordered with a locator the coordinator could not serve.
#[test]
fn an_entry_with_no_source_resolves_to_nothing() {
    let named = macos_entry("hostkeeper", "komira-testing", "0.1.306");
    let none = |entry: &Target, platform: &str| resolve(entry, "hostkeeper", platform, &sources());
    assert!(
        none(
            &macos_entry("hostkeeper", "other-testing", "1"),
            "macos-arm64"
        )
        .is_none()
    );
    assert!(none(&named, "linux-x86_64").is_none());
    let unversioned = Target {
        package_version: None,
        ..named.clone()
    };
    assert!(none(&unversioned, "macos-arm64").is_none());
    let unnamed = Target {
        repository: None,
        ..named
    };
    assert!(none(&unnamed, "macos-arm64").is_none());
    assert!(is_named(&macos_entry("hostkeeper", "komira-testing", "1")));
    assert!(!is_named(&repository_entry("web", "komira-testing")));
}

/// Proves: the resolved entry passes the artifact checks the daemon runs on an entry of its
/// installer for a Mac: `check_entry` on it, with the service and platform it is ordered for.
///
/// Catches: a resolved entry the daemon would refuse (`target_invalid`) after the coordinator
/// ordered it.
#[test]
fn a_resolved_entry_passes_the_daemons_artifact_checks() {
    let named = macos_entry("web", "komira-testing", "2.0");
    let service = ServiceName::new("web").expect("a name");
    assert_eq!(check_entry(&service, Platform::MacosArm64, &named), Ok(()));
    let resolved = resolve(&named, "web", "macos-arm64", &sources()).expect("resolves");
    assert_eq!(
        check_entry(&service, Platform::MacosArm64, &resolved),
        Ok(())
    );
}

/// Proves: the registry's sources are one per channel of a public repository whose descriptor has
/// a macOS format (the built-in `komira`'s two), and none for an entry put without a descriptor,
/// for a private one, or once an entry without a descriptor is put over the built-in one.
///
/// Catches: a legacy apt entry claiming a macOS source it was never given.
#[test]
fn only_a_repository_with_a_macos_format_is_a_source() {
    let fresh = sources();
    assert_eq!(
        fresh.keys().collect::<Vec<_>>(),
        ["komira-stable", "komira-testing"]
    );
    assert_eq!(fresh["komira-testing"].repository, builtin::NAME);
    assert_eq!(fresh["komira-testing"].platforms, ["macos-arm64"]);
    let legacy = Registry::default().put(komira(1)).expect("put");
    assert!(sources_of(&legacy).is_empty());
    let mut with = komira(1);
    with.base_uri = "https://packages.example.com".to_owned();
    with.descriptor = serde_json::to_vec(&json!({
        "version": 1,
        "channels": ["testing", "stable"],
        "key": {"keyring": "k.gpg", "fingerprints": [crate::repositories_support::fingerprint(&with.keyring)]},
        "formats": {
            "apt": {"path": "apt", "components": ["main"]},
            "macos-pkg": {"path": "pkg", "platforms": ["macos-arm64"]}
        }
    }))
    .expect("json");
    let put = Registry::default().put(with.clone()).expect("put");
    assert_eq!(sources_of(&put)["komira-testing"].repository, "komira");
    with.access = "hosts".to_owned();
    with.credential_file = "/etc/cred".to_owned();
    let private = Registry::default().put(with).expect("put");
    assert!(sources_of(&private).is_empty());
}

fn release_of(entries: Vec<Target>) -> Arc<Release> {
    Arc::new(Release::new(
        BTreeMap::from([("macos-arm64".to_owned(), TargetList { entries })]),
        None,
    ))
}

/// Proves: a release that names no Mac's `.pkg` by version is returned as it is (the same
/// allocation, explicit-location entries included); one that does has each such entry resolved,
/// the others kept in order, and an entry that does not resolve left out.
///
/// Catches: a release with explicit `.pkg` locations rewritten, an entry that does not resolve
/// ordered anyway, or a list reordered.
#[test]
fn a_release_is_resolved_entry_by_entry_and_an_explicit_one_is_left_alone() {
    let explicit = Target {
        installer: "macos-pkg".to_owned(),
        locations: vec![Location {
            store: "komira-testing".to_owned(),
            locator: "pkg/hostkeeper/1/x.pkg".to_owned(),
        }],
        bytes: 5,
        digest: "b".repeat(64),
        ..Target::default()
    };
    let plain = release_of(vec![explicit.clone()]);
    assert!(!has_named(&plain));
    assert!(Arc::ptr_eq(&plain, &resolved("web", &plain, &sources())));

    let named = macos_entry("web", "komira-testing", "2.0");
    let lost = macos_entry("web", "other-testing", "2.0");
    let mixed = release_of(vec![explicit.clone(), named.clone(), lost]);
    assert!(has_named(&mixed));
    let kept = resolved("web", &mixed, &sources());
    let entries = &kept.targets()["macos-arm64"].entries;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], explicit);
    assert_eq!(
        entries[1].locations[0].locator,
        "repositories/komira/pkg/web/2.0/web-2.0-macos-arm64.pkg"
    );
    // The release as stored is as it was.
    assert_eq!(mixed.targets()["macos-arm64"].entries.len(), 3);
}

/// Proves: a Mac whose daemon announced `private-repository-v1` is picked the resolved entry of a
/// release that names its `.pkg` by version (its location in the `coordinator` store); a Mac whose
/// daemon did not is picked nothing (`no_target`, the rule for an entry the coordinator serves).
///
/// Catches: a Mac ordered a `.pkg` it cannot fetch, or one that is ordered no `.pkg` though the
/// repository holds it.
#[tokio::test]
async fn a_mac_is_picked_the_resolved_entry_only_if_its_daemon_reads_the_coordinator() {
    let rig = rig("macos-pick", "stages = [100]");
    let set = |version: &str, channel: &str| {
        let version = version.to_owned();
        let channel = channel.to_owned();
        let rig = &rig;
        async move {
            let targets = BTreeMap::from([(
                "macos-arm64".to_owned(),
                TargetList {
                    entries: vec![macos_entry(WEB, &channel, &version)],
                },
            )]);
            let body = format!(r#"{{"service":"web","version":"{version}"}}"#);
            rig.coordinator
                .set_desired_with(
                    &crate::service::ci_calls::Setter::Operator("alice"),
                    body.as_bytes(),
                    &crate::write_record::WriteAsk::default(),
                    Some(targets),
                )
                .await
                .expect("set");
        }
    };
    let beats = |host: &str, private: bool| {
        let mut features: Vec<String> = AGENT.iter().map(|f| (*f).to_owned()).collect();
        features.push(TARGET_FEATURE.to_owned());
        features.extend(private.then(|| PRIVATE_FEATURE.to_owned()));
        rig.coordinator.hear_installers(
            &who(host, "daemon"),
            &agent::HeartbeatRequest {
                features: features.clone(),
                installers: vec!["macos-pkg".to_owned()],
                ..agent::HeartbeatRequest::default()
            },
        );
        let mut beat = beat_on(hostkeeper_wire::Platform::DarwinArm64, Vec::new());
        beat.agent_features = features;
        ask_heard(&rig, who(host, "daemon"), beat);
    };
    let found = |host: &str, version: &str| {
        let fleet = rig.coordinator.lock_fleet();
        let version = Version::parse(version).expect("a version");
        match pick(&fleet, WEB, &version, &id(host)) {
            Pick::Entry(entry) => Ok(entry.locations[0].locator.clone()),
            other => Err(format!("{other:?}")),
        }
    };

    set("2.0", "komira-testing").await;
    beats("m1", true);
    beats("m2", false);
    assert_eq!(
        found("m1", "2.0"),
        Ok("repositories/komira/pkg/web/2.0/web-2.0-macos-arm64.pkg".to_owned())
    );
    assert_eq!(found("m2", "2.0"), Err("NoTarget".to_owned()));
}
