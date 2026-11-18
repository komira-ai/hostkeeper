//! The services' identities: reported only by a daemon that keeps them, as the source words them
//! at each heartbeat, with no feature of their own.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    ANNOUNCED, FACTS, Feature, PRIVATE_REPOSITORY, REPOSITORIES, ServiceIdentityReport,
};

use hostkeeper_wire::wire::{MAX_FEATURES, read_heartbeat};

use crate::Rig;

fn report(service: &str, not_after: Option<u64>, error: Option<&str>) -> ServiceIdentityReport {
    ServiceIdentityReport::new(service.to_owned(), not_after, error.map(str::to_owned))
}

/// Proves a daemon with no source announces exactly [`ANNOUNCED`] and sends no report.
#[test]
fn a_host_with_no_service_identities_announces_none_and_sends_no_report() {
    let rig = Rig::new("service-identity-none");
    rig.heartbeater().beat_once();
    let beat = &rig.world.link.beats()[0];
    assert_eq!(beat.features, ANNOUNCED.to_vec());
    assert_eq!(beat.service_identities, []);
    assert_eq!(beat.to_json().get("service_identities"), None);
}

/// Proves a daemon with every port present announces the features of its ports and none for the
/// services' identities, at most [`MAX_FEATURES`] names, and that the coordinator's reader takes
/// the heartbeat whole, with its features and its identity report.
///
/// Catches: a feature added over the bound, which every released coordinator answers by dropping
/// the whole list (and the host then gets no repository or key orders).
#[test]
fn a_host_with_every_port_announces_no_more_than_the_wire_bound_and_is_read_whole() {
    let rig = Rig::with_world(
        "service-identity-announce",
        World::default()
            .managing_repositories()
            .keeping_facts()
            .keeping_operator_keys()
            .keeping_prerequisites()
            .keeping_service_identities(),
    );
    rig.world
        .service_identities
        .set(vec![report("js-agent", Some(1_900_028_800), None)]);
    rig.heartbeater().beat_once();
    let beat = rig.world.link.beats()[0].clone();
    assert_eq!(
        beat.features[ANNOUNCED.len()..],
        [
            REPOSITORIES,
            FACTS,
            PRIVATE_REPOSITORY,
            Feature::OperatorKeysV1,
            Feature::PrerequisitesV1,
        ]
    );
    assert!(beat.features.len() <= MAX_FEATURES);
    // The wire's reader still asks for the top-level state of the pre-order shape.
    let mut json = beat.to_json();
    json["version"] = "1.0".into();
    json["health"] = "healthy".into();
    let bytes = serde_json::to_vec(&json).expect("JSON");
    let read = read_heartbeat(&bytes, None).expect("read");
    assert_eq!(read.extras_dropped, None);
    let names: Vec<&str> = beat.features.iter().map(|f| f.as_str()).collect();
    assert_eq!(read.beat.agent_features, names);
    assert_eq!(beat.service_identities.len(), 1);
}

/// Proves the source's reports are in every heartbeat as it words them, and follow it when they
/// change: first none, then a certificate and a refusal.
#[test]
fn the_sources_reports_are_in_every_heartbeat() {
    let rig = Rig::with_world(
        "service-identity-reports",
        World::default().keeping_service_identities(),
    );
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    let reports = vec![
        report("js-agent", Some(1_900_028_800), None),
        report("js-worker", None, Some("denied")),
    ];
    rig.world.service_identities.set(reports.clone());
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].service_identities, []);
    assert_eq!(beats[1].service_identities, reports);
}
