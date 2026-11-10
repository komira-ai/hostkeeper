//! `Hosts.Get` and `Hosts.List` of a host in a host group: the services shown, and the services of
//! the heartbeat shown, are its group's packages, those that only a group lists among them.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::group_decision_support::Farm;
use crate::marks_support::operator;

/// The host `host` as `Hosts.Get` answers it.
fn got(farm: &Farm, host: &str) -> proto::Host {
    farm.world
        .coordinator()
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: host.to_owned(),
            },
        )
        .expect("answered")
        .host
        .expect("a host")
}

/// Each service of the host's heartbeat as shown: its name, version and health.
fn beats(host: &proto::Host) -> Vec<(String, String, i32)> {
    host.heartbeat
        .as_ref()
        .expect("a heartbeat")
        .services
        .iter()
        .map(|one| (one.name.clone(), one.version.clone(), one.health))
        .collect()
}

/// Proves: `Hosts.Get` of a host in a group shows each package of its group that the host runs,
/// in its services and in its heartbeat's services, with the version and health it reported,
/// once each and in name order: `cfg-alpha`, which only the group lists (the coordinator defines
/// no such service, as `serve` defines none), as well as `web`, which the coordinator's own
/// definition also has; and `Hosts.List` shows the same services.
///
/// Catches: a package only a group lists missing from the host's page (its heartbeat said it runs
/// it healthy, and the operator is shown nothing of it), or a service shown twice, once by its
/// group and once by the coordinator's definition.
#[test]
fn a_host_in_a_group_is_shown_with_the_packages_only_its_group_lists() {
    let farm = Farm::new("hosts-group-packages");
    farm.group("alpha", &["web", "cfg-alpha"], None);
    farm.host("a1", "alpha");
    farm.want("alpha", "web", "1.0");
    farm.want("alpha", "cfg-alpha", "1.0");
    farm.tell("a1", &[("web", "1.0"), ("cfg-alpha", "1.0")]);

    let host = got(&farm, "a1");
    let healthy = hostkeeper_proto::hostkeeper::agent::v1::Health::Healthy as i32;
    assert_eq!(
        beats(&host),
        [
            ("cfg-alpha".to_owned(), "1.0".to_owned(), healthy),
            ("hostkeeper".to_owned(), "1.0".to_owned(), healthy),
            ("web".to_owned(), "1.0".to_owned(), healthy),
        ]
    );
    let services: Vec<&str> = host
        .services
        .iter()
        .map(|one| one.service.as_str())
        .collect();
    assert_eq!(services, ["cfg-alpha", "hostkeeper", "web"]);

    let listed = farm
        .world
        .coordinator()
        .list_hosts(&operator(), proto::ListHostsRequest::default())
        .expect("answered")
        .hosts;
    let a1 = listed
        .iter()
        .find(|one| one.node_id == "a1")
        .expect("a1 is listed");
    let services: Vec<&str> = a1.services.iter().map(|one| one.service.as_str()).collect();
    assert_eq!(services, ["cfg-alpha", "hostkeeper", "web"]);
}
