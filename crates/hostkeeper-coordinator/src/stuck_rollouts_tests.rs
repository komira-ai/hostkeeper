//! A rollout that cannot make progress holds no other service's rollout ([`crate::queue`],
//! [`crate::live`]): the four ways a fleet of several groups froze behind one service, each driven
//! by the heartbeats of enrolled hosts placed in groups. In each, one service set first has hosts
//! left to order that no rule of the decision lets go now (a host's own halt, an install that
//! waits for a package listed before it, a host that failed, hosts that refused for a moment), and
//! a service of another group set after it is ordered all the same.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{
    Class, LocalHalt, Outcome, Phase, ServiceBeat, ServiceReply, Token, UpdateResult,
};

use crate::decide::{Gate, Skip, Why};
use crate::group_decision_support::{Farm, policy, stages};
use crate::marks_support::{INSTALLER, beat_of, runs, update};
use crate::orders::Order;
use crate::testutil::{id, version};

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn at(farm: &Farm, secs: u64) {
    farm.world.rig.at(secs);
}

/// Takes the fleet's lock once, so the cluster stamps the desired versions set so far.
fn stamp(farm: &Farm) {
    drop(farm.world.coordinator().lock_fleet());
}

/// The plan of `package` as `Releases.Status` says it, for the group of `host`.
pub(crate) fn plan_of(farm: &Farm, host: &str, package: &str) -> proto::RolloutPlan {
    let coordinator = farm.world.coordinator();
    let basis = coordinator.basis_of(&id(host));
    let all = coordinator.desired_for(&basis.services);
    let fleet = coordinator.lock_fleet();
    let now: Duration = (coordinator.clock)();
    let def = basis.services.get(package).expect("a def");
    crate::rollouts::plan_proto(
        &basis.services,
        &all,
        def,
        &all[package],
        false,
        &fleet,
        now,
    )
}

/// What a host that holds a halt of its own of `service` reports: it runs `running`, and holds
/// the generation `generation` of a halt for `reason`, written `age_secs` ago.
pub(crate) fn halted_here(
    service: &str,
    running: &str,
    reason: &str,
    generation: u64,
    age_secs: u64,
) -> ServiceBeat {
    let mut beat = runs(service, running);
    beat.local_halt = Some(LocalHalt {
        reason: Token::parse(reason),
        generation,
        age_secs: Some(age_secs),
    });
    beat
}

/// A failed report of `target`: `phase` and `class` as given, with `reason` and `detail`.
pub(crate) fn failure(
    target: &str,
    phase: Option<Phase>,
    class: Option<Class>,
    reason: &str,
    detail: &str,
) -> UpdateResult {
    UpdateResult {
        target: version(target),
        outcome: Outcome::Failed,
        phase,
        class,
        reason: Token::parse(reason),
        order_id: None,
        order_try: Some(1),
        detail: Some(detail.to_owned()),
    }
}

/// What a host whose first install of `service` failed reports: the package is staged, never run
/// (`absent`), and its last update is `report`.
pub(crate) fn install_failed(service: &str, report: UpdateResult) -> ServiceBeat {
    let mut beat: ServiceBeat = serde_json::from_value(serde_json::json!({
        "name": service, "version": report.target.as_str(), "health": "absent"
    }))
    .expect("a service beat");
    beat.last_update = Some(report);
    beat
}

/// What a host that runs `running` of `service` and failed its update to the report's target
/// reports.
pub(crate) fn update_failed(service: &str, running: &str, report: UpdateResult) -> ServiceBeat {
    let mut beat = runs(service, running);
    beat.last_update = Some(report);
    beat
}

/// The typed order a daemon that takes installs is given for a heartbeat that lists hostkeeper
/// and `services`, each as given.
pub(crate) fn order_with(farm: &Farm, host: &str, services: Vec<ServiceBeat>) -> Option<Order> {
    let listed = std::iter::once(runs("hostkeeper", "1.0"))
        .chain(services)
        .collect();
    farm.order_on(host, beat_of(INSTALLER, listed))
}

/// The group `two` with `db` on one host `t1` that runs `1.0`, and `db` set to `2.0` after every
/// version set before: the service of another group that must not wait.
fn db_set_last(farm: &Farm, secs: u64) {
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(farm, secs);
    farm.want("two", "db", "2.0");
    stamp(farm);
}

/// Proves: a service whose stage-1 host is done and whose one host left to order holds a halt of
/// its own (`halted_here`) does not hold another group's service: `db`, set after it, is ordered,
/// and `web`'s plan says it has nothing to order (`nothing_to_order`) and waits on no service.
/// Once the host's halt is lifted, `web` may order it again: the host is ordered `web` once `db`
/// is no longer live.
///
/// Catches: a plan "under way" whose only host left is one the decision never orders (it is held
/// `halted_here` before the plan's gate), which stays live and keeps every other service of every
/// group at `rollout_elsewhere` for as long as the host keeps its halt.
#[test]
fn a_host_that_holds_its_own_halt_holds_no_other_service() {
    let farm = Farm::new("stuck-local-halt");
    farm.group(
        "one",
        &["web"],
        Some(policy(|p| p.stages = stages(&[50, 100]))),
    );
    farm.group("two", &["db"], None);
    farm.host("o1", "one");
    farm.host("o2", "one");
    farm.host("t1", "two");
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), NOTHING);
    assert_eq!(
        farm.tell_with(
            "o2",
            vec![
                runs("hostkeeper", "1.0"),
                halted_here("web", "1.0", "tainted", 0, 60),
            ]
        ),
        NOTHING
    );
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("o1", &[("web", "2.0")]), NOTHING);
    db_set_last(&farm, 2_010);
    assert_eq!(
        farm.tell_with(
            "o2",
            vec![
                runs("hostkeeper", "1.0"),
                halted_here("web", "1.0", "tainted", 0, 120),
            ]
        ),
        NOTHING
    );
    assert_eq!(farm.why("o2", "web"), Some(Why::Gate(Gate::HaltedHere)));

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let web = plan_of(&farm, "o2", "web");
    assert_eq!(
        (web.waiting, web.waiting_on),
        (i32::from(proto::PlanWait::NothingToOrder), vec![])
    );

    assert_eq!(farm.tell("t1", &[("db", "2.0")]), NOTHING);
    assert_eq!(farm.tell("o2", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: in a group listing `tierstore` before `js`, a host whose install of `tierstore` failed
/// (`failed_here`) and a host that waits behind it (`stage_failed`) leave `js` with no host it can
/// order: both are `install_order`. `js` takes no place in the queue (`hostkeeper` of another group,
/// set after `js`, is ordered, and waits on no service), and its plan says `nothing_to_order`.
///
/// Catches: a queued service whose every host waits `install_order` behind a package that does
/// not install, held at position 1 of the queue, and every service of every group set after it
/// waiting on it (`waiting_on: [js]`) for as long as the earlier package fails.
#[tokio::test]
async fn a_service_whose_hosts_all_wait_for_an_earlier_package_does_not_hold_the_queue() {
    let farm = Farm::new("stuck-install-order");
    farm.flag(1);
    farm.group("mac", &["tierstore", "js"], None);
    farm.group("two", &["db"], None);
    farm.host("m1", "mac");
    farm.host("m2", "mac");
    farm.host("t1", "two");
    assert_eq!(order_with(&farm, "m1", vec![]), None);
    assert_eq!(order_with(&farm, "m2", vec![]), None);
    at(&farm, 2_005);
    farm.deploy("mac", "tierstore", "2.0", 1).await;
    stamp(&farm);
    at(&farm, 2_006);
    farm.deploy("mac", "js", "2.0", 2).await;
    stamp(&farm);
    let missing = failure(
        "2.0",
        Some(Phase::BeforeCommit),
        Some(Class::Environment),
        "config_missing",
        "there is no configuration file",
    );
    assert!(matches!(
        order_with(&farm, "m1", vec![]),
        Some(Order::Install { .. })
    ));
    assert_eq!(
        order_with(&farm, "m1", vec![install_failed("tierstore", missing)]),
        None
    );
    assert_eq!(order_with(&farm, "m2", vec![]), None);
    assert_eq!(
        farm.why("m1", "tierstore"),
        Some(Why::Skip(Skip::FailedHere))
    );
    assert_eq!(
        farm.why("m2", "tierstore"),
        Some(Why::Gate(Gate::StageFailed))
    );
    for host in ["m1", "m2"] {
        assert_eq!(farm.why(host, "js"), Some(Why::Gate(Gate::InstallOrder)));
    }
    db_set_last(&farm, 2_010);

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let js = plan_of(&farm, "m2", "js");
    assert_eq!(
        (js.waiting, js.waiting_on, js.queue_position),
        (i32::from(proto::PlanWait::NothingToOrder), vec![], None)
    );
}

/// Proves: a new package whose first install failed on the first host of its group
/// (`manifest_invalid`: the host `failed_here`, the other `stage_failed`) holds no service of
/// another group, though the package listed after it in its group has no host it can order (each
/// waits `install_order`): `db` in group `two`, set after both, is ordered, and the later package's
/// plan says `nothing_to_order`.
///
/// Catches: a failed first install that keeps the rest of the fleet waiting behind the package
/// listed after it, until an operator holds something by hand.
#[tokio::test]
async fn a_package_that_failed_on_its_first_host_holds_nobody_else() {
    let farm = Farm::new("stuck-failed-first");
    farm.flag(1);
    farm.group("one", &["cfg", "svc"], None);
    farm.group("two", &["db"], None);
    farm.host("o1", "one");
    farm.host("o2", "one");
    farm.host("t1", "two");
    assert_eq!(order_with(&farm, "o1", vec![]), None);
    assert_eq!(order_with(&farm, "o2", vec![]), None);
    at(&farm, 2_005);
    farm.deploy("one", "cfg", "2.0", 1).await;
    stamp(&farm);
    at(&farm, 2_006);
    farm.deploy("one", "svc", "2.0", 2).await;
    stamp(&farm);
    assert!(matches!(
        order_with(&farm, "o1", vec![]),
        Some(Order::Install { .. })
    ));
    let invalid = failure(
        "2.0",
        Some(Phase::BeforeCommit),
        Some(Class::Release),
        "manifest_invalid",
        "the manifest does not parse",
    );
    assert_eq!(
        order_with(&farm, "o1", vec![install_failed("cfg", invalid)]),
        None
    );
    assert_eq!(order_with(&farm, "o2", vec![]), None);
    assert_eq!(farm.why("o1", "cfg"), Some(Why::Skip(Skip::FailedHere)));
    assert_eq!(farm.why("o2", "cfg"), Some(Why::Gate(Gate::StageFailed)));
    assert_eq!(farm.why("o2", "svc"), Some(Why::Gate(Gate::InstallOrder)));
    db_set_last(&farm, 2_010);

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let svc = plan_of(&farm, "o2", "svc");
    assert_eq!(
        (svc.waiting, svc.waiting_on),
        (i32::from(proto::PlanWait::NothingToOrder), vec![])
    );
}

/// Proves: two hosts of six that refused an update of `web` for a moment (`busy`, a refusal with
/// no phase and no class) leave its plan held by its failed hosts (`failed_host`), a rule of the
/// plan that is no halt: the version is not marked bad, a service of another group set after it is
/// ordered, and the plan waits on no service.
///
/// Catches: a refusal for a moment read as a release failure that marks the version bad, or a
/// plan held by its failed hosts that holds every other service of the cluster.
#[test]
fn hosts_that_refused_for_a_moment_hold_nobody_else() {
    let farm = Farm::new("stuck-refused");
    farm.flag(1);
    farm.group(
        "six",
        &["web"],
        Some(policy(|p| {
            p.stages = stages(&[100]);
            p.rollback = proto::RollbackMode::Automatic.into();
        })),
    );
    farm.group("two", &["db"], None);
    let hosts = ["s1", "s2", "s3", "s4", "s5", "s6"];
    for host in hosts {
        farm.host(host, "six");
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    farm.host("t1", "two");
    at(&farm, 2_005);
    farm.want("six", "web", "2.0");
    stamp(&farm);
    for host in hosts {
        assert_eq!(farm.tell(host, &[("web", "1.0")]), update("web", "2.0"));
    }
    let busy = failure(
        "2.0",
        None,
        None,
        "busy",
        "another order of the service holds it",
    );
    for host in ["s1", "s2"] {
        assert_eq!(
            farm.tell_with(
                host,
                vec![
                    runs("hostkeeper", "1.0"),
                    update_failed("web", "1.0", busy.clone()),
                ]
            ),
            NOTHING
        );
    }
    for host in ["s3", "s4", "s5", "s6"] {
        assert_eq!(farm.tell(host, &[("web", "2.0")]), NOTHING);
    }
    db_set_last(&farm, 2_010);

    assert_eq!(farm.why("s1", "web"), Some(Why::Skip(Skip::FailedHere)));
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let web = plan_of(&farm, "s3", "web");
    assert_eq!(
        (web.waiting, web.waiting_on),
        (i32::from(proto::PlanWait::FailedHost), vec![])
    );
    assert_eq!(farm.wanted("six", "web"), Some(version("2.0")));
}

/// Proves: a service whose one host left to order is moving into its group and not admitted yet
/// (`moving`: another package it must change, `cfg`, has a paused plan) holds no other group's
/// service: `db`, set after it, is ordered, and its plan says `nothing_to_order`.
///
/// Catches: a plan under way that waits on a host's move, which lasts as long as any plan the host
/// joins cannot take it, and holds every other service meanwhile.
#[test]
fn a_host_that_is_moving_into_its_group_holds_no_other_service() {
    let farm = Farm::new("stuck-moving");
    farm.group(
        "one",
        &["web", "cfg"],
        Some(policy(|p| p.stages = stages(&[50, 100]))),
    );
    farm.group("two", &["db"], None);
    farm.host("o1", "one");
    farm.moving("o2", "one");
    farm.host("t1", "two");
    let both = [("web", "1.0"), ("cfg", "1.0")];
    assert_eq!(farm.tell("o1", &both), NOTHING);
    farm.want("one", "cfg", "2.0");
    farm.world.seed_marks("one", "cfg", |marks| {
        marks.rollout.version = Some(version("2.0"));
        marks.rollout.paused = Some(crate::rollouts::Pause {
            by: "ops".to_owned(),
            reason: "look".to_owned(),
        });
    });
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    assert_eq!(farm.tell("o1", &both), update("web", "2.0"));
    assert_eq!(farm.tell("o1", &[("web", "2.0"), ("cfg", "1.0")]), NOTHING);
    db_set_last(&farm, 2_010);
    assert_eq!(farm.tell("o2", &both), NOTHING);
    assert_eq!(farm.why("o2", "web"), Some(Why::Gate(Gate::Moving)));

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let web = plan_of(&farm, "o2", "web");
    assert_eq!(
        (web.waiting, web.waiting_on),
        (i32::from(proto::PlanWait::NothingToOrder), vec![])
    );
}
