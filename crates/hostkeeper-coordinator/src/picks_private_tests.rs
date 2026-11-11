//! The pick of an entry that needs a daemon which reads private repositories
//! ([`crate::picks`]): a repository entry naming a channel of a private repository, and an artifact
//! entry all of whose locations are in the store `coordinator`, are picked nothing
//! (`Pick::NoTarget`) for a daemon that did not announce `private-repository-v1`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use hostkeeper_wire::Platform;
use hostkeeper_wire::version::Version;

use crate::marks_support::{AGENT, WEB, ask_heard, beat_on, operator, rig, who};
use crate::picks::{COORDINATOR_STORE, Pick, Picks, TARGET_FEATURE, pick};
use crate::releases::Targets;
use crate::releases_tests::{apt_entry, list};
use crate::repositories_support::{credential_in, komira, private, repository, repository_entry};
use crate::repository_push::PRIVATE_FEATURE;
use crate::service::ci_calls::Setter;
use crate::testutil::{Rig, id};
use crate::write_record::WriteAsk;

/// A rig whose `web` names the repository `priv`, put as a private repository of the group
/// `daemon`.
fn priv_rig(name: &str) -> Rig {
    let rig = rig(name, "repository = \"priv\"\nstages = [100]");
    let file = credential_in(rig.dir.path(), "priv-credential", "Bearer x");
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private(
                    "priv",
                    "https://priv.example.com/apt",
                    &["daemon"],
                    1,
                    &file,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    rig
}

/// Sets `web` at `version` with `targets`, as the operator does.
async fn set(rig: &Rig, version: &str, targets: Targets) {
    let body = format!(
        r#"{{"service":"web","version":"{version}","min_up":{{"storage":0,"frontend":0}}}}"#
    );
    rig.coordinator
        .set_desired_with(
            &Setter::Operator("alice"),
            body.as_bytes(),
            &WriteAsk::default(),
            Some(targets),
        )
        .await
        .expect("set");
}

/// The host `host` heartbeats as a Linux daemon of `target-v1` listing `installers`, also
/// announcing `private-repository-v1` when `private`.
fn beats(rig: &Rig, host: &str, installers: &[&str], private: bool) {
    let mut features: Vec<String> = AGENT.iter().map(|f| (*f).to_owned()).collect();
    features.push(TARGET_FEATURE.to_owned());
    features.extend(private.then(|| PRIVATE_FEATURE.to_owned()));
    rig.coordinator.hear_installers(
        &who(host, "daemon"),
        &agent::HeartbeatRequest {
            features: features.clone(),
            installers: installers.iter().map(|i| (*i).to_owned()).collect(),
            ..agent::HeartbeatRequest::default()
        },
    );
    let mut beat = beat_on(Platform::LinuxX86_64, Vec::new());
    beat.agent_features = features;
    ask_heard(rig, who(host, "daemon"), beat);
}

/// What the pick finds for `host` of `web` at `version`.
fn picked(rig: &Rig, host: &str, version: &str) -> &'static str {
    let fleet = rig.coordinator.lock_fleet();
    let version = Version::parse(version).expect("a version");
    match pick(&fleet, WEB, &version, &id(host)) {
        Pick::Entry(_) => "entry",
        Pick::NoTarget => "no_target",
        other => panic!("{host} {version}: {other:?}"),
    }
}

/// An artifact entry of `web` for a `macos-pkg` whose every location is in `store`.
fn artifact_in(store: &str, locations: usize) -> Target {
    let mut one = apt_entry(WEB, 3);
    one.installer = "macos-pkg".to_owned();
    one.repository = None;
    one.package_version = None;
    one.bytes = 10;
    one.locations = (0..locations)
        .map(|n| Location {
            store: store.to_owned(),
            locator: format!("repositories/priv/pool/daemon/web-{n}.pkg"),
        })
        .collect();
    one
}

/// Proves: a repository entry naming a channel of a private repository is picked for a daemon that
/// announced `private-repository-v1` and `Pick::NoTarget` for one that did not; the registry's
/// change of a repository from private to public (a new `Put`) lifts it at the next heartbeat.
///
/// Catches: an older daemon ordered an entry it cannot install, or a private channel remembered
/// after the repository stopped being private.
#[tokio::test]
async fn a_private_repository_entry_is_picked_only_for_a_daemon_that_reads_private_repositories() {
    let rig = priv_rig("picks-private-repository");
    set(
        &rig,
        "2.0",
        crate::repositories_support::targets_of(vec![repository_entry(WEB, "priv-daemon")]),
    )
    .await;
    beats(&rig, "d1", &["apt"], true);
    beats(&rig, "d2", &["apt"], false);
    assert_eq!(picked(&rig, "d1", "2.0"), "entry");
    assert_eq!(picked(&rig, "d2", "2.0"), "no_target");

    // The same name put as a public repository: the channel is no longer private.
    let public = repository("priv", "https://priv.example.com/apt", &["daemon"], 1);
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(public),
                reason: "now public".to_owned(),
            },
        )
        .expect("put");
    beats(&rig, "d2", &["apt"], false);
    assert_eq!(picked(&rig, "d2", "2.0"), "entry");
}

/// Proves: an artifact entry all of whose locations are in the store `coordinator` is
/// `Pick::NoTarget` for a daemon that did not announce the feature and picked for one that did; an
/// entry with one location elsewhere, and an ordinary artifact entry, are picked for both.
///
/// Catches: a Mac of the release before ordered a `.pkg` it can only fetch from a store it does
/// not have, or a public entry caught by the rule.
#[tokio::test]
async fn an_artifact_entry_in_the_coordinator_store_needs_the_same_daemon() {
    assert_eq!(COORDINATOR_STORE, "coordinator");
    let rig = priv_rig("picks-private-artifact");
    let one = |target: Target| Targets::from([("linux-x86_64".to_owned(), list(vec![target]))]);
    // Only the desired version's release is read, so each is checked as it is set.
    set(&rig, "2.0", one(artifact_in("coordinator", 2))).await;
    beats(&rig, "d1", &["macos-pkg"], true);
    beats(&rig, "d2", &["macos-pkg"], false);
    assert_eq!(picked(&rig, "d1", "2.0"), "entry");
    assert_eq!(picked(&rig, "d2", "2.0"), "no_target");

    set(&rig, "2.1", one(artifact_in("github", 1))).await;
    beats(&rig, "d2", &["macos-pkg"], false);
    assert_eq!(picked(&rig, "d2", "2.1"), "entry", "a public entry");

    let mut mixed = artifact_in("coordinator", 1);
    mixed.locations.push(Location {
        store: "github".to_owned(),
        locator: "example-org/web/releases/download/v2.2/web.tgz".to_owned(),
    });
    set(&rig, "2.2", one(mixed)).await;
    beats(&rig, "d2", &["macos-pkg"], false);
    assert_eq!(
        picked(&rig, "d2", "2.2"),
        "entry",
        "one location is elsewhere"
    );
}

/// Proves: a release with no private entry is picked as before for a daemon without the feature,
/// and a public registry (`komira`) has no private channel.
///
/// Catches: the rule applied to every pick.
#[tokio::test]
async fn a_public_repository_entry_is_unaffected() {
    let rig = rig("picks-private-public", "stages = [100]");
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(komira(1)),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    set(
        &rig,
        "2.0",
        crate::repositories_support::targets_of(vec![repository_entry(WEB, "komira-testing")]),
    )
    .await;
    beats(&rig, "d2", &["apt"], false);
    assert_eq!(picked(&rig, "d2", "2.0"), "entry");
}

/// Proves: an artifact entry is private only when it has locations and every one of them is in
/// the store `coordinator`: with no location at all it is not, with one `coordinator` location
/// it is, and with one `coordinator` and one `github` location it is not; a repository entry is
/// private only when its channel is a private repository's.
/// Catches: an entry with no location (one a stored release may hold) taken as private, which
/// would pick every daemon without `private-repository-v1` nothing for it.
#[test]
fn an_artifact_entry_with_no_location_is_not_private() {
    let at = |stores: &[&str]| Target {
        installer: "macos-pkg".to_owned(),
        locations: stores
            .iter()
            .map(|store| Location {
                store: (*store).to_owned(),
                locator: "web/2.0/web.pkg".to_owned(),
            })
            .collect(),
        ..Target::default()
    };
    let mut picks = Picks::default();
    picks.keep_private(["priv".to_owned()].into_iter().collect());
    assert!(!picks.is_private(&at(&[])));
    assert!(picks.is_private(&at(&[COORDINATOR_STORE])));
    assert!(!picks.is_private(&at(&[COORDINATOR_STORE, "github"])));
    let channel = |name: &str| Target {
        repository: Some(name.to_owned()),
        ..Target::default()
    };
    assert!(picks.is_private(&channel("priv")));
    assert!(!picks.is_private(&channel("komira-testing")));
}
