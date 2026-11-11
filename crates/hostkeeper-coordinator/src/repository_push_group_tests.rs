//! The repositories a host is sent in group mode ([`crate::repository_push`]): the ones named by
//! the packages of its group, whatever the rollout file's roles and the assignments say.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::group_decision_support::Farm;
use crate::marks_support::{ask, beat_of, operator, runs, who};
use crate::repositories_support::komira;
use crate::repository_push::FEATURE;
use crate::testutil::id;

/// `host` heartbeats as a daemon that announced `repositories-v1` and runs hostkeeper alone, and
/// the coordinator hears what it said of its repositories.
fn announces(farm: &Farm, host: &str) {
    let features = ["heartbeat-v2", FEATURE];
    ask(
        &farm.world.rig,
        host,
        "storage",
        beat_of(&features, vec![runs("hostkeeper", "1.0")]),
    );
    farm.world.coordinator().hear_repositories(
        &who(host, "storage"),
        &agent::HeartbeatRequest {
            features: vec![FEATURE.to_owned()],
            ..agent::HeartbeatRequest::default()
        },
    );
}

/// Proves: with a host group in the store, a host is sent the repository that a package of its
/// group names (`web`, of the rollout file, names `komira`; the file's roles `daemon` and
/// `storage` do not include the group `alpha`), the entry of the registry's repository `komira`;
/// a host in no group is sent none; the host shows the repository as named.
///
/// Catches: a host placed by group that is never sent the repository of the package it is ordered
/// to install, because the set was made from the rollout file's roles.
#[test]
fn a_host_in_a_group_is_sent_the_repositories_of_its_groups_packages() {
    let farm = Farm::new("repository-push-group");
    farm.group("alpha", &["web"], None);
    farm.host("m1", "alpha");
    farm.lone("m2");
    farm.world
        .coordinator()
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(komira(2)),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    announces(&farm, "m1");
    announces(&farm, "m2");

    let set = farm
        .world
        .coordinator()
        .repository_set(&id("m1"))
        .expect("a set");
    assert_eq!(set.entries, [komira(2)]);
    assert_eq!(
        farm.world.coordinator().repository_set(&id("m2")),
        None,
        "a host in no group is placed nothing"
    );
    let shown = farm
        .world
        .coordinator()
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "m1".to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
        .repositories
        .expect("shown");
    assert_eq!(shown.named, ["komira".to_owned()]);
}
