//! Packages wait for a host in its turn of a maintenance run, not the reverse
//! (`docs/design/maintenance-updates.md`, section 5.1): a host between the order that starts its
//! turn and its end is left out of the hosts a package plan counts for its stage (neither a failed
//! host nor one left to order), `min_up` counts it down, a run takes no place of `--live-rollouts`,
//! and the host is given its package orders once its turn is over. The tests drive heartbeats of
//! enrolled hosts of one group, with the hosts' turns seeded in the store as a run leaves them.

use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceStep;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceReply};

use crate::decide::{Gate, Why};
use crate::decide_maintenance::tests::{RUN, at_step};
use crate::group_decision_support::{Farm, policy, stages};
use crate::host_groups_support::{SELF, UNIX};
use crate::maintenance_calls_tests::{GROUP, edit_runs, host_at, run_of};
use crate::maintenance_runs::{Kind, Step};
use crate::marks_support::{INSTALLER, ask_typed, beat_of, runs, update};
use crate::orders::Order;

const NOTHING: ServiceReply = ServiceReply::Nothing;

/// What a host in its turn is told of a package it is behind on.
const BUSY: Option<Why> = Some(Why::Gate(Gate::BusyElsewhere));

/// A farm with the group [`GROUP`] listing `packages` under `edit` of a policy of one stage of every
/// host and four update slots, its hosts `hosts`, each heard once running every package at 1.0.
fn farm(
    name: &str,
    packages: &[&str],
    hosts: &[&str],
    edit: impl FnOnce(&mut proto::RolloutPolicy),
) -> Farm {
    let farm = Farm::new(name);
    farm.group(
        GROUP,
        packages,
        Some(policy(|p| {
            p.stages = stages(&[100]);
            p.max_updating = Some(4);
            edit(p);
        })),
    );
    let running: Vec<(&str, &str)> = packages.iter().map(|package| (*package, "1.0")).collect();
    for host in hosts {
        farm.host(host, GROUP);
        assert_eq!(farm.tell(host, &running), NOTHING);
    }
    farm
}

/// Puts the hosts `turns` in [`RUN`] of [`GROUP`], each at its step, as the run's decisions leave
/// them.
fn in_run(farm: &Farm, turns: &[(&str, Step)]) {
    let hosts = turns
        .iter()
        .map(|(host, step)| host_at(host, GROUP, *step, UNIX, &[Kind::Linux]))
        .collect();
    edit_runs(&farm.world, |held| held.push(run_of(RUN, hosts)));
}

/// The heartbeat of a daemon that takes installs and announces `maintenance-v1`, lists hostkeeper
/// and `services` healthy at the versions given, and reports its turn of [`RUN`] done.
fn turn_done(farm: &Farm, services: &[(&str, &str)]) -> Heartbeat {
    let mut beat = farm.beat_with(services);
    beat.agent_features
        .push(crate::maintenance::FEATURE.to_owned());
    beat.maintenance = at_step(MaintenanceStep::Done).maintenance;
    beat
}

/// The reply to `beat` of `host`.
fn reply(farm: &Farm, host: &str, beat: Heartbeat) -> ServiceReply {
    ask_typed(&farm.world.rig, host, "storage", beat).0
}

/// How many hosts the plan of `web` in [`GROUP`] counts, and whether it has finished, as
/// `Releases.Status` answers it.
fn plan(farm: &Farm) -> (u32, bool) {
    let coordinator = farm.world.coordinator();
    let now = (coordinator.clock)();
    let status = {
        let fleet = coordinator.lock_fleet();
        coordinator.status_in(&fleet, now)
    };
    let answer = crate::grpc::report::status(&status, "web");
    let web = answer
        .services
        .iter()
        .find(|one| one.host_group == GROUP)
        .expect("the group's web");
    let plan = web.plan.as_ref().expect("a plan");
    (plan.eligible, plan.finished)
}

/// Proves: a stage of four hosts, one of them draining in its turn, is done once the other three
/// run the desired version (the plan counts three hosts and has finished), and the fourth is told
/// `busy_elsewhere` during its turn and ordered the update at the heartbeat that reports its turn
/// done; the plan then counts four and finishes once the fourth runs the version.
///
/// Catches: a stage that waits for a draining host, which holds the package's rollout open for as
/// long as the drain takes.
#[test]
fn a_stage_is_done_without_the_draining_host_which_updates_after_its_turn() {
    let farm = farm("mp-stage", &["web"], &["a1", "a2", "a3", "a4"], |_| {});
    in_run(&farm, &[("a4", Step::Draining)]);
    farm.want(GROUP, "web", "2.0");

    assert_eq!(farm.tell("a4", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("a4", "web"), BUSY);
    for host in ["a1", "a2", "a3"] {
        assert_eq!(farm.tell(host, &[("web", "1.0")]), update("web", "2.0"));
    }
    for host in ["a1", "a2", "a3"] {
        assert_eq!(farm.tell(host, &[("web", "2.0")]), NOTHING);
    }
    assert_eq!(plan(&farm), (3, true), "the draining host is in no stage");

    assert_eq!(
        reply(&farm, "a4", turn_done(&farm, &[("web", "1.0")])),
        update("web", "2.0"),
        "its turn is over: it is ordered at once"
    );
    assert_eq!(plan(&farm), (4, false));
    assert_eq!(farm.tell("a4", &[("web", "2.0")]), NOTHING);
    assert_eq!(plan(&farm), (4, true));
}

/// Proves: a host of the current stage that runs the desired version and went down while it drains
/// in its turn does not hold the stage open: with stages of half and every host, the first stage
/// (it and one other host at the version) is done, and both hosts of the second stage are ordered
/// at once; the drained host, back up at the version after its turn, is told nothing more.
///
/// Catches: a stage that counts a draining host as an update still running, so no later host of the
/// group is ordered until the drain, the reboot and the checks are over.
#[test]
fn a_host_down_in_its_turn_holds_no_stage_open() {
    let farm = farm("mp-held-open", &["web"], &["a1", "a2", "a3", "a4"], |p| {
        p.stages = stages(&[50, 100]);
    });
    farm.want(GROUP, "web", "2.0");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    in_run(&farm, &[("a2", Step::Draining)]);
    let mut drained = runs("web", "2.0");
    drained.health = Health::Unhealthy;
    assert_eq!(
        farm.tell_with("a2", vec![runs(SELF, "1.0"), drained]),
        NOTHING
    );

    assert_eq!(farm.tell("a3", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a4", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(
        reply(&farm, "a2", turn_done(&farm, &[("web", "2.0")])),
        NOTHING
    );
}

/// Proves: a host that took its place in a stage keeps it through its turn: with a first stage of
/// one host and a second an operator enters, the host done in the first stage starts its turn and
/// the next host is still told `advance_wait`, not ordered in its place.
///
/// Catches: a done host left out of its stage when its turn starts, which frees its place and
/// orders a second host past the operator's Advance (or past a canary of one).
#[test]
fn a_host_keeps_the_place_it_took_through_its_turn() {
    let farm = farm("mp-kept-place", &["web"], &["a1", "a2", "a3", "a4"], |p| {
        p.stages = vec![
            proto::PolicyStage {
                percent: 25,
                advance: proto::StageAdvance::Auto.into(),
            },
            proto::PolicyStage {
                percent: 100,
                advance: proto::StageAdvance::Manual.into(),
            },
        ];
    });
    farm.want(GROUP, "web", "2.0");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.why("a2", "web"), Some(Why::Gate(Gate::AdvanceWait)));

    in_run(&farm, &[("a1", Step::Draining)]);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("a2", "web"), Some(Why::Gate(Gate::AdvanceWait)));
}

/// Proves: `min_up` counts a host in its turn as down, though it still reports the package healthy:
/// of four hosts that must keep two up, one draining, the first is ordered and the second is told
/// `below_min_up`; once the first runs the version again, the second is ordered; and the drained
/// host, its turn over, is ordered under the same count.
///
/// Catches: a draining host counted as serving, which lets a package update take the last hosts of
/// the group that serve.
#[test]
fn min_up_counts_a_host_in_its_turn_as_down() {
    let farm = farm("mp-min-up", &["web"], &["a1", "a2", "a3", "a4"], |p| {
        p.min_up = 2;
    });
    in_run(&farm, &[("a4", Step::Draining)]);
    farm.want(GROUP, "web", "2.0");

    assert_eq!(farm.tell("a4", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("a2", "web"), Some(Why::Gate(Gate::BelowMinUp)));
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));

    assert_eq!(
        reply(&farm, "a4", turn_done(&farm, &[("web", "1.0")])),
        update("web", "2.0"),
        "a1 and a3 up: the fourth may go"
    );
}

/// Proves: with `--live-rollouts 1`, a package rollout starts while a host of the group drains in
/// its turn: the first package's plan has rolled every host but the draining one, so it takes no
/// place, and the second package is ordered; the drained host, its turn over, is ordered the first
/// package.
///
/// Catches: a run counted as a live rollout, or a plan held under way by a draining host, which
/// keeps every other package of the cluster waiting `rollout_elsewhere` for the whole drain.
#[test]
fn a_maintenance_run_takes_no_place_of_the_live_rollouts() {
    let farm = farm("mp-live", &["web", "db"], &["a1", "a2"], |_| {});
    farm.flag(1);
    in_run(&farm, &[("a2", Step::Draining)]);
    farm.want(GROUP, "web", "2.0");
    farm.want(GROUP, "db", "2.0");

    assert_eq!(
        farm.tell("a1", &[("web", "1.0"), ("db", "1.0")]),
        update("web", "2.0")
    );
    assert_eq!(
        farm.tell("a1", &[("web", "2.0"), ("db", "1.0")]),
        update("db", "2.0"),
        "web has rolled every host but the draining one"
    );

    assert_eq!(
        reply(
            &farm,
            "a2",
            turn_done(&farm, &[("web", "1.0"), ("db", "1.0")])
        ),
        update("web", "2.0")
    );
}

/// Proves: only a host in its turn is left out: a host that waits in a run (not yet started) and a
/// host whose turn is done are ordered the update and counted by the plan, while a host whose turn
/// halted (cordoned until an operator puts it back) is told `busy_elsewhere` and is in no stage.
///
/// Catches: a host left out of its plans for as long as a run lists it, or a halted host's update
/// ordered while it is still cordoned.
#[test]
fn only_a_host_in_its_turn_is_left_out_of_the_plan() {
    let farm = farm("mp-steps", &["web"], &["a1", "a2", "a3", "a4"], |_| {});
    in_run(
        &farm,
        &[
            ("a1", Step::Waiting),
            ("a2", Step::Done),
            ("a3", Step::Halted),
        ],
    );
    farm.want(GROUP, "web", "2.0");

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a3", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("a3", "web"), BUSY);
    assert_eq!(farm.tell("a4", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(plan(&farm), (3, false));
}

/// Proves: an uninstall of a package the group took out waits for the host's turn as an update
/// does: the host in its turn is told `busy_elsewhere`, the other host is ordered the uninstall,
/// and the first is ordered it at the heartbeat that reports its turn done.
///
/// Catches: a package taken off a host while it drains or reboots for maintenance.
#[test]
fn an_uninstall_waits_for_the_hosts_turn() {
    let farm = Farm::new("mp-uninstall");
    let wide = || Some(policy(|p| p.max_updating = Some(2)));
    farm.group(GROUP, &["cfg"], wide());
    for host in ["a1", "a2"] {
        farm.host(host, GROUP);
        assert_eq!(farm.order(host, &[("cfg", "1.0")]), None);
    }
    in_run(&farm, &[("a1", Step::Draining)]);
    farm.world
        .put_with(GROUP, &[SELF], wide(), Some(1))
        .expect("put");

    assert_eq!(farm.why("a1", "cfg"), BUSY);
    assert!(matches!(
        farm.order("a2", &[("cfg", "1.0")]),
        Some(Order::Uninstall { service, .. }) if service == "cfg"
    ));
    let done = turn_done(&farm, &[("cfg", "1.0")]);
    assert!(matches!(
        farm.order_on("a1", done),
        Some(Order::Uninstall { service, .. }) if service == "cfg"
    ));
}

/// Proves: a host moved into a group is not admitted to the group's plans while it is in its turn
/// (it is told `moving`, its old group's packages included), and is admitted, and told its first
/// uninstall, at the heartbeat that reports its turn done.
///
/// Catches: a moving host admitted while no plan counts it, which skips the check of a place and
/// lets it update past the plan's stage, `max_updating` and `min_up` after its turn.
#[tokio::test]
async fn a_moving_host_is_not_admitted_during_its_turn() {
    use crate::moving_tests::{MOVING, OLD, farm_of, told, wide};
    let farm = farm_of("mp-moving", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    in_run(&farm, &[("m1", Step::Draining)]);

    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "svc"), MOVING);
    assert_eq!(farm.why("m1", "oldcfg"), MOVING);

    let listed = std::iter::once((SELF, "1.0"))
        .chain(OLD)
        .map(|(name, at)| runs(name, at))
        .collect();
    let mut done = beat_of(INSTALLER, listed);
    done.agent_features
        .push(crate::maintenance::FEATURE.to_owned());
    done.maintenance = at_step(MaintenanceStep::Done).maintenance;
    let order = farm.order_on("m1", done);
    assert!(
        matches!(&order, Some(Order::Uninstall { service, .. }) if service == "oldcfg"),
        "{order:?}"
    );
}
