//! The repository sets of a reply and the report of a heartbeat: announced and applied only by a
//! daemon that manages its repository configuration, and applied only while the coordinator
//! advertises `repositories-v1`.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    ANNOUNCED, Feature, PRIVATE_REPOSITORY, PushedSet, REPOSITORIES, Reply, RepositoriesReport,
    RepositoryRefusal,
};
use hostkeeper_proto::hostkeeper::release::v1::Repository;

use crate::{Rig, recorded, reply, update};

fn set(generation: u64) -> PushedSet {
    PushedSet {
        generation,
        digest: "ab".repeat(32),
        entries: vec![Repository {
            name: "komira".to_owned(),
            kind: "apt".to_owned(),
            uri: "https://repo.example.com/debian".to_owned(),
            suites: vec!["stable".to_owned()],
            components: vec!["main".to_owned()],
            fingerprints: vec!["AB".repeat(20)],
            keyring: vec![0x99, 0, 1, 4],
            ..Default::default()
        }],
    }
}

fn pushed(features: &[Feature], generation: u64) -> Reply {
    Reply {
        features: features.to_vec(),
        repositories: Some(set(generation)),
        ..Reply::default()
    }
}

fn managing(name: &str) -> Rig {
    Rig::with_world(name, World::default().managing_repositories())
}

/// Proves a daemon with no repository configuration (a Mac) announces exactly [`ANNOUNCED`],
/// sends no report, lists the installers it was given at its start, and applies no set.
#[test]
fn a_host_with_no_repository_configuration_announces_none_and_sends_no_report() {
    let rig = Rig::new("repositories-none");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(pushed(&[Feature::RepositoriesV1], 3)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].features, ANNOUNCED.to_vec());
    assert_eq!(beats[0].repositories, None);
    assert_eq!(beats[1].repositories, None);
    assert!(rig.world.repositories.applied().is_empty());
}

/// Proves a daemon with a repository configuration announces `repositories-v1` after the others,
/// sends its report in every heartbeat, and lists the installers its configuration gives at that
/// heartbeat in place of the list given at the start.
#[test]
fn a_host_with_a_repository_configuration_announces_the_feature_and_sends_its_report() {
    let rig = managing("repositories-report");
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    let report = RepositoriesReport {
        generation: 4,
        digest: "cd".repeat(32),
        refused: Some(RepositoryRefusal {
            generation: 5,
            digest: "ef".repeat(32),
            reason: "no".to_owned(),
        }),
    };
    rig.world.repositories.set_report(report.clone());
    rig.world
        .repositories
        .set_installers(&["hostkeeper-archive", "apt"]);
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    let mut features = ANNOUNCED.to_vec();
    features.push(REPOSITORIES);
    features.push(PRIVATE_REPOSITORY);
    assert_eq!(beats[0].features, features);
    assert_eq!(
        beats[0].repositories,
        Some(RepositoriesReport::default()),
        "a daemon that holds no pushed set reports generation 0 and no digest"
    );
    assert_eq!(beats[0].installers, Vec::<String>::new());
    assert_eq!(beats[1].repositories, Some(report));
    assert_eq!(beats[1].installers, ["hostkeeper-archive", "apt"]);
}

/// Proves the set of a reply goes to the daemon's configuration only while that reply advertises
/// `repositories-v1`, whole and unchanged, once per reply, and that a later reply without the set
/// applies none.
#[test]
fn a_set_is_applied_only_when_the_reply_advertises_the_feature() {
    let rig = managing("repositories-apply");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(pushed(&[], 1)));
    rig.world
        .link
        .answer(Ok(pushed(&[Feature::HaltV1, Feature::RepositoriesV1], 2)));
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::RepositoriesV1])));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(rig.world.repositories.applied(), vec![set(2)]);
}

/// Proves a reply that carries a set and an order has both taken: the set goes to the daemon's
/// configuration and the order is started.
#[test]
fn a_reply_with_a_set_and_an_order_has_both_taken() {
    let rig = managing("repositories-before-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut answer = pushed(&[Feature::RepositoriesV1], 1);
    answer.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    assert_eq!(rig.world.repositories.applied(), vec![set(1)]);
    assert_eq!(
        rig.world.dispatch.pending(),
        1,
        "the order of the reply was started"
    );
}

/// Proves a reply's `repositories-v1` is a feature the daemon knows by its name, though it is not
/// among those every daemon announces.
#[test]
fn the_feature_reads_back_from_its_name_and_is_not_always_announced() {
    assert_eq!(Feature::RepositoriesV1.as_str(), "repositories-v1");
    assert_eq!(
        Feature::parse("repositories-v1"),
        Some(Feature::RepositoriesV1)
    );
    assert!(!ANNOUNCED.contains(&Feature::RepositoriesV1));
    assert!(ANNOUNCED.len() < hostkeeper_wire::wire::MAX_FEATURES);
}

/// Proves the report counts toward the heartbeat's bound: its JSON has it with a refusal's
/// reason, and the heartbeat of a host with no configuration has none.
#[test]
fn the_report_is_in_the_json_the_bound_measures() {
    let rig = managing("repositories-json");
    let mut heartbeater = rig.heartbeater();
    rig.world.repositories.set_report(RepositoriesReport {
        generation: 2,
        digest: "cd".repeat(32),
        refused: Some(RepositoryRefusal {
            generation: 3,
            digest: "ef".repeat(32),
            reason: "bad keyring".to_owned(),
        }),
    });
    heartbeater.beat_once();
    let beat = rig.world.link.beats().remove(0);
    let json = beat.to_json();
    assert_eq!(
        json["repositories"],
        serde_json::json!({
            "generation": 2,
            "digest": "cd".repeat(32),
            "refused": {"generation": 3, "digest": "ef".repeat(32), "reason": "bad keyring"},
        })
    );
    let mut plain = beat.clone();
    plain.repositories = None;
    assert!(plain.to_json().get("repositories").is_none());
    assert!(beat.encoded_len() > plain.encoded_len());
    let mut unrefused = beat;
    if let Some(report) = unrefused.repositories.as_mut() {
        report.refused = None;
    }
    assert_eq!(
        unrefused.to_json()["repositories"]["refused"],
        serde_json::Value::Null
    );
}

/// Proves `private-repository-v1` is announced by a daemon with a repository sink and by one the
/// wiring says reads private repositories another way, and by no other; and that its name reads
/// back.
#[test]
fn the_private_repository_feature_is_announced_with_a_sink_or_by_the_flag() {
    assert_eq!(
        Feature::PrivateRepositoryV1.as_str(),
        "private-repository-v1"
    );
    assert_eq!(
        Feature::parse("private-repository-v1"),
        Some(PRIVATE_REPOSITORY)
    );
    assert!(!ANNOUNCED.contains(&PRIVATE_REPOSITORY));

    let plain = Rig::new("private-announce-none");
    let mut heartbeater = plain.heartbeater();
    heartbeater.beat_once();
    assert_eq!(plain.world.link.beats()[0].features, ANNOUNCED.to_vec());

    let flagged = Rig::with_world(
        "private-announce-flag",
        World::default().reading_private_repositories(),
    );
    let mut heartbeater = flagged.heartbeater();
    heartbeater.beat_once();
    let mut expected = ANNOUNCED.to_vec();
    expected.push(PRIVATE_REPOSITORY);
    assert_eq!(flagged.world.link.beats()[0].features, expected);
}
