//! The services' identities of `service-identity-v1`: announced and reported only by a daemon that
//! keeps them, and reported as the source words them at each heartbeat.

use hostkeeper_link::testing::World;
use hostkeeper_link::{
    ANNOUNCED, FACTS, Feature, PRIVATE_REPOSITORY, REPOSITORIES, SERVICE_IDENTITY,
    ServiceIdentityReport,
};

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

/// Proves a daemon with a source announces `service-identity-v1` after every other feature a
/// daemon may announce.
#[test]
fn a_host_with_service_identities_announces_the_feature_last() {
    let all = Rig::with_world(
        "service-identity-announce",
        World::default()
            .managing_repositories()
            .keeping_facts()
            .keeping_operator_keys()
            .keeping_service_identities(),
    );
    all.heartbeater().beat_once();
    let announced = all.world.link.beats()[0].features.clone();
    assert_eq!(
        announced[ANNOUNCED.len()..],
        [
            REPOSITORIES,
            FACTS,
            PRIVATE_REPOSITORY,
            Feature::OperatorKeysV1,
            SERVICE_IDENTITY,
        ]
    );
    assert_eq!(
        announced.last().map(|f| f.as_str()),
        Some("service-identity-v1")
    );
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
