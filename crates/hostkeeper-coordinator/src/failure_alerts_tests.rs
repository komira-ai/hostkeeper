//! Every failed update and every halt a host holds of its own is an alert `Alerts.List` answers
//! ([`crate::alerts`]), on a coordinator of host groups driven by the heartbeats of its hosts: it
//! appears with the beat that reports it and clears with the beat that no longer does. And a
//! canary that fails still stops its own group's roll, while a service of another group goes on.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Class, Outcome, Phase, ServiceReply, UpdateResult};

use crate::decide::{Gate, Why};
use crate::group_decision_support::{Farm, policy, stages};
use crate::marks_support::{operator, runs, update};
use crate::orders::Order;
use crate::stuck_rollouts_tests::{
    failure, halted_here, install_failed, order_with, update_failed,
};
use crate::testutil::version;

const NOTHING: ServiceReply = ServiceReply::Nothing;

/// The alerts `Alerts.List` answers now, of `kind`.
fn listed(farm: &Farm, kind: proto::AlertKind) -> Vec<proto::Alert> {
    farm.world
        .coordinator()
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("listed")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind == i32::from(kind))
        .collect()
}

/// Proves: a host of a group whose first install of a package failed is a `failed_update` alert
/// of `Alerts.List`, critical, naming the host, the package, the version, its group, the report's
/// reason, detail and try; it stands while the host reports the failure and clears once the host
/// runs the version.
///
/// Catches: a failed install, held `failed_here` for hours, that `hk alerts list` answers `{}`
/// for, or an alert that stays once the host has the version.
#[tokio::test]
async fn a_failed_install_is_listed_until_the_host_runs_the_version() {
    let farm = Farm::new("fa-failed-install");
    farm.group("mac", &["tierstore"], None);
    farm.host("m1", "mac");
    farm.host("m2", "mac");
    assert_eq!(order_with(&farm, "m1", vec![]), None);
    farm.deploy("mac", "tierstore", "2.0", 1).await;
    assert!(matches!(
        order_with(&farm, "m1", vec![]),
        Some(Order::Install { .. })
    ));
    let missing = failure(
        "2.0",
        Some(Phase::BeforeCommit),
        Some(Class::Environment),
        "config_missing",
        "there is no configuration file",
    );
    assert_eq!(
        order_with(
            &farm,
            "m1",
            vec![install_failed("tierstore", missing.clone())]
        ),
        None
    );

    let alerts = listed(&farm, proto::AlertKind::FailedUpdate);
    assert_eq!(alerts.len(), 1);
    let alert = &alerts[0];
    assert_eq!(
        (
            alert.severity,
            alert.host_id.as_str(),
            alert.service.as_str(),
            alert.version.as_str(),
            alert.reason.as_str(),
            alert.detail.as_str(),
            alert.tries,
        ),
        (
            i32::from(proto::AlertSeverity::Critical),
            "m1",
            "tierstore",
            "2.0",
            "config_missing",
            "there is no configuration file",
            1,
        )
    );
    assert_eq!(
        alert.message,
        "m1 failed tierstore 2.0 in host group mac: config_missing (\"there is no configuration \
         file\"); it is not ordered 2.0 again until an operator retries it (Hosts.Retry), holds or \
         excludes it, or another version is set"
    );
    assert_eq!(
        order_with(&farm, "m1", vec![install_failed("tierstore", missing)]),
        None
    );
    assert_eq!(listed(&farm, proto::AlertKind::FailedUpdate).len(), 1);

    let mut installed = runs("tierstore", "2.0");
    installed.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Succeeded));
    assert_eq!(order_with(&farm, "m1", vec![installed]), None);
    assert_eq!(listed(&farm, proto::AlertKind::FailedUpdate), vec![]);
}

/// Proves: a host of a group that holds a halt of a package of its own is a `host_halt` alert of
/// `Alerts.List`, critical, naming the host, the package, the version it runs, its group, the
/// halt's reason and when the host wrote it; it clears at the beat that no longer reports the halt.
///
/// Catches: a host-local halt left for nine hours after its cause was gone that nobody is told
/// of.
#[test]
fn a_hosts_own_halt_is_listed_until_the_host_no_longer_holds_it() {
    let farm = Farm::new("fa-host-halt");
    farm.group("one", &["web"], None);
    farm.host("o1", "one");
    farm.want("one", "web", "2.0");
    assert_eq!(
        farm.tell_with(
            "o1",
            vec![
                runs("hostkeeper", "1.0"),
                halted_here("web", "1.0", "tainted", 0, 32_400),
            ]
        ),
        NOTHING
    );

    let alerts = listed(&farm, proto::AlertKind::HostHalt);
    assert_eq!(alerts.len(), 1);
    let alert = &alerts[0];
    assert_eq!(
        (
            alert.severity,
            alert.host_id.as_str(),
            alert.service.as_str(),
            alert.version.as_str(),
            alert.reason.as_str(),
            alert.since_unix,
        ),
        (
            i32::from(proto::AlertSeverity::Critical),
            "o1",
            "web",
            "1.0",
            "tainted",
            alert.first_observed_unix - 32_400,
        )
    );
    assert_eq!(
        alert.message,
        "o1 holds its own halt of web in host group one (tainted, generation 0, written 9 h 0 \
         min ago): it refuses every order of the service until an operator clears the halt \
         (Releases.ClearHalt)"
    );

    assert_eq!(farm.tell("o1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(listed(&farm, proto::AlertKind::HostHalt), vec![]);
}

/// Proves: with a canary of one, a canary that fails the version stops its own group's roll (the
/// other hosts wait `stage_failed`, and are ordered nothing), is a `failed_update` alert, and a
/// service of another group set after it is ordered.
///
/// Catches: a change that lets a group's roll go past its failed canary, which is how a bad
/// release is caught, or a failed canary that holds the rest of the cluster.
#[test]
fn a_failed_canary_stops_its_own_roll_and_holds_no_other_group() {
    let farm = Farm::new("fa-canary");
    farm.flag(1);
    farm.group(
        "one",
        &["web"],
        Some(policy(|p| {
            p.canary = Some(proto::Canary { count: 1 });
            p.stages = stages(&[100]);
        })),
    );
    farm.group("two", &["db"], None);
    for host in ["o1", "o2", "o3"] {
        farm.host(host, "one");
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    farm.host("t1", "two");
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    farm.world.rig.at(2_005);
    farm.want("one", "web", "2.0");
    drop(farm.world.coordinator().lock_fleet());
    farm.world.rig.at(2_010);
    farm.want("two", "db", "2.0");
    drop(farm.world.coordinator().lock_fleet());
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), update("web", "2.0"));
    let broken = failure(
        "2.0",
        Some(Phase::AfterCommit),
        Some(Class::Release),
        "not_serving",
        "the service did not answer its health check",
    );
    assert_eq!(
        farm.tell_with(
            "o1",
            vec![
                runs("hostkeeper", "1.0"),
                update_failed("web", "1.0", broken),
            ]
        ),
        NOTHING
    );

    for host in ["o2", "o3"] {
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
        assert_eq!(farm.why(host, "web"), Some(Why::Gate(Gate::StageFailed)));
    }
    let failed = listed(&farm, proto::AlertKind::FailedUpdate);
    assert_eq!(
        failed
            .iter()
            .map(|alert| (alert.host_id.as_str(), alert.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![("o1", "not_serving")]
    );
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// What an order is: its verb and its service.
fn verb(order: Option<Order>) -> Option<(&'static str, String)> {
    match order? {
        Order::Install { service, .. } => Some(("install", service)),
        Order::Update { service, .. } => Some(("update", service)),
        _ => Some(("other", String::new())),
    }
}

fn of(verb: &'static str, service: &str) -> Option<(&'static str, String)> {
    Some((verb, service.to_owned()))
}

/// Proves the example of `docs/operations/rollout-plans.md` ("Example: one host fails, under
/// `stop`"), step by step: a group of six build nodes and a group of four Macs whose failed hosts
/// hold their stage, `tierstore-config`, then
/// `tierstore`, then `hostkeeper` set. The first Mac's install of `tierstore-config` fails
/// (`manifest_invalid`): the other Macs wait `stage_failed`, `tierstore` waits `install_order` on
/// every Mac and its plan says `nothing_to_order`, one `failed_update` alert names the Mac, and
/// `hostkeeper`, set last, rolls in both groups at once: three build nodes and one Mac (the failed
/// one, first to ask), the others waiting for the next stage. A new version of the config clears the
/// alert, and its plan waits for `hostkeeper`'s live rollout.
///
/// Catches: the operations page describing a fleet that the coordinator does not roll as it says.
#[tokio::test]
async fn the_example_of_one_failed_host_rolls_as_the_operations_page_says() {
    let farm = Farm::new("fa-example");
    farm.flag(1);
    farm.group(
        "build",
        &["tierstore"],
        Some(policy(|p| p.stages = stages(&[50, 100]))),
    );
    farm.group(
        "mac",
        &["tierstore-config", "tierstore"],
        Some(policy(|p| {
            p.stages = stages(&[25, 100]);
            p.failed_hosts = proto::FailedHosts::Stop.into();
        })),
    );
    let builds = ["b1", "b2", "b3", "b4", "b5", "b6"];
    let macs = ["m1", "m2", "m3", "m4"];
    for host in builds {
        farm.host(host, "build");
        assert_eq!(order_with(&farm, host, vec![]), None);
    }
    for host in macs {
        farm.host(host, "mac");
        assert_eq!(order_with(&farm, host, vec![]), None);
    }
    for (secs, group, package, n) in [
        (2_005, "mac", "tierstore-config", 1),
        (2_006, "mac", "tierstore", 2),
        (2_010, "", "hostkeeper", 3),
    ] {
        farm.world.rig.at(secs);
        farm.deploy(group, package, "2.0", n).await;
        drop(farm.world.coordinator().lock_fleet());
    }

    assert_eq!(
        verb(order_with(&farm, "m1", vec![])),
        of("install", "tierstore-config")
    );
    let invalid = failure(
        "2.0",
        Some(Phase::BeforeCommit),
        Some(Class::Release),
        "manifest_invalid",
        "the manifest does not parse",
    );
    let failed_config = install_failed("tierstore-config", invalid);
    assert_eq!(
        verb(order_with(&farm, "m1", vec![failed_config.clone()])),
        of("update", "hostkeeper")
    );
    for host in ["m2", "m3", "m4"] {
        assert_eq!(
            farm.why(host, "tierstore-config"),
            Some(Why::Gate(Gate::StageFailed))
        );
    }
    for host in ["m2", "m3", "m4"] {
        assert_eq!(
            farm.why(host, "tierstore"),
            Some(Why::Gate(Gate::InstallOrder))
        );
    }
    assert_eq!(
        farm.why("m1", "tierstore"),
        Some(Why::Gate(Gate::BusyElsewhere))
    );
    let config = crate::stuck_rollouts_tests::plan_of(&farm, "m1", "tierstore-config");
    let tierstore = crate::stuck_rollouts_tests::plan_of(&farm, "m1", "tierstore");
    assert_eq!(
        (config.waiting, tierstore.waiting, tierstore.queue_position),
        (
            i32::from(proto::PlanWait::FailedHost),
            i32::from(proto::PlanWait::NothingToOrder),
            None
        )
    );
    let alerts = listed(&farm, proto::AlertKind::FailedUpdate);
    assert_eq!(
        alerts
            .iter()
            .map(|a| (
                a.host_id.as_str(),
                a.service.as_str(),
                a.reason.as_str(),
                a.tries
            ))
            .collect::<Vec<_>>(),
        vec![("m1", "tierstore-config", "manifest_invalid", 1)]
    );

    for host in ["b1", "b2", "b3"] {
        assert_eq!(
            verb(order_with(&farm, host, vec![])),
            of("update", "hostkeeper"),
            "{host}"
        );
    }
    for host in ["b4", "m2"] {
        assert_eq!(verb(order_with(&farm, host, vec![])), None);
        assert_eq!(
            farm.why(host, "hostkeeper"),
            Some(Why::Gate(Gate::StageWait))
        );
    }

    farm.deploy("mac", "tierstore-config", "2.1", 4).await;
    assert_eq!(listed(&farm, proto::AlertKind::FailedUpdate), vec![]);
    assert_eq!(verb(order_with(&farm, "m3", vec![])), None);
    assert_eq!(
        farm.why("m3", "tierstore-config"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
}
