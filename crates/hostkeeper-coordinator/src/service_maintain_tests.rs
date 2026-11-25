//! A heartbeat's maintenance ([`Coordinator::maintenance_next`], [`Coordinator::fold_maintenance`]):
//! the `maintain` a host is sent, one thing at a time on a host (a package update holds a turn
//! back, and a turn holds the package orders back), the same order and no new one at the next
//! heartbeat and after a restart, the step the host reports stored and audited, and the JSON route.
//! The builders are in `decide_maintenance_tests.rs`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceKind, MaintenanceStep};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};

use super::*;
use crate::decide_maintenance::tests::{
    ORDER, RUN, at_step, beat, in_turn, maintain_id, scheduled, starts, steps, table, tell,
};
use crate::group_decision_support::{Farm, policy, stages};
use crate::host_groups_support::{SELF, UNIX, coordinator_on};
use crate::maintenance_calls_tests::{GROUP, LINUX, LINUX_REBOOT, edit_runs, host_at, run_of};
use crate::maintenance_runs::Kind;
use crate::marks_support::{ask_json, ask_typed, runs, update};
use crate::ops::AuditEntry;
use crate::orders::Maintain;
use crate::testutil::{Rig, version};

/// What the tests compare of an audit entry: who, the action, the host, the group and the reason.
type Said = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// [`Said`] of `entry`.
fn said(entry: &AuditEntry) -> Said {
    (
        entry.by.clone(),
        entry.action.clone(),
        entry.node_id.clone(),
        entry.host_group.clone(),
        entry.reason.clone(),
    )
}

/// An audit entry of the coordinator's, `action`, about `host` of [`GROUP`], with `reason`.
fn coordinator_said(action: &str, host: &str, reason: &str) -> Said {
    (
        "coordinator".to_owned(),
        action.to_owned(),
        Some(host.to_owned()),
        Some(GROUP.to_owned()),
        Some(reason.to_owned()),
    )
}

/// The maintenance entries of the world's audit, as [`said`] says them.
fn maintenance_audit(world: &crate::host_groups_support::World) -> Vec<Said> {
    world
        .audit()
        .iter()
        .filter(|entry| entry.action.starts_with("maintenance_") && entry.by == "coordinator")
        .map(said)
        .collect()
}

/// Proves: the order that starts a host's turn is a `maintain` with the action `RUN`, the order id
/// `maintain-<run id>`, try 1, the kinds the run holds for the host in the order its report listed
/// them, the group's Linux scope, the group's drain deadline when the run names none (none when the
/// run names 0), the `install` path and the group's `return_secs` for a Linux host (1200 by
/// default), and no Xcode; and it reaches the wire as exactly that.
///
/// Catches: an order whose fields are taken from the wrong place (the run's deadline over the
/// group's or the reverse, a scope left unspecified), which the daemon would refuse or run wrongly.
#[test]
fn the_maintain_order_carries_the_run_and_the_groups_settings() {
    let mut settings = table(2, 0);
    settings.linux_scope = proto::LinuxScope::All as i32;
    settings.drain_deadline_secs = 600;
    settings.macos_major = true;
    settings.batch = Some(3);
    let world = crate::host_groups_support::World::new("sm-order");
    crate::decide_maintenance::tests::group_with(&world, GROUP, settings, &["h1", "h2", "h3"]);
    for host in ["h1", "h2", "h3"] {
        tell(&world, host, beat(LINUX_REBOOT, None));
    }
    let mut asked = crate::maintenance_calls_tests::group_request(GROUP);
    asked.take_last_host = true;
    crate::maintenance_calls_tests::schedule(&world, asked).expect("scheduled");

    let expected = |drain_deadline_secs| Order::Maintain {
        maintain: Maintain {
            run_id: RUN.to_owned(),
            action: agent::MaintainAction::Run,
            kinds: vec![MaintenanceKind::Linux, MaintenanceKind::Reboot],
            linux_scope: agent::LinuxScope::All,
            drain_deadline_secs,
            macos_path: agent::MacosPath::Install,
            macos_major: true,
            return_secs: 1200,
            xcode: None,
        },
        order_id: ORDER.to_owned(),
        order_try: 1,
    };
    let order = tell(&world, "h1", beat(LINUX_REBOOT, None)).expect("an order");
    assert_eq!(order, expected(Some(600)));
    assert_eq!(
        order.to_proto(),
        agent::Order {
            order_id: ORDER.to_owned(),
            r#try: 1,
            verb: Some(agent::order::Verb::Maintain(agent::Maintain {
                run_id: RUN.to_owned(),
                action: agent::MaintainAction::Run as i32,
                kinds: vec![
                    MaintenanceKind::Linux as i32,
                    MaintenanceKind::Reboot as i32
                ],
                linux_scope: agent::LinuxScope::All as i32,
                drain_deadline_secs: Some(600),
                macos_path: agent::MacosPath::Install as i32,
                xcodes: Vec::new(),
                xcode_remove: Vec::new(),
                xcode_default: String::new(),
                return_secs: 1200,
                macos_major: true,
            })),
        }
    );

    edit_runs(&world, |runs| runs[0].drain_deadline_secs = Some(0));
    assert_eq!(
        tell(&world, "h2", beat(LINUX_REBOOT, None)),
        Some(expected(None)),
        "the run's 0 is no deadline, over the group's 600"
    );
    edit_runs(&world, |runs| runs[0].drain_deadline_secs = Some(30));
    assert_eq!(
        tell(&world, "h3", beat(LINUX_REBOOT, None)),
        Some(expected(Some(30))),
        "the run's own deadline, over the group's"
    );
}

/// A heartbeat of a host of the farm that announces `maintenance-v1`, reports a Linux update due
/// and, when given, its run record at `step`, and lists hostkeeper at 1.0 and `web` as given.
fn farm_beat(web: ServiceBeat, step: Option<MaintenanceStep>) -> Heartbeat {
    let mut said = match step {
        Some(step) => at_step(step),
        None => beat(LINUX, None),
    };
    said.services = Some(vec![runs(SELF, "1.0"), web]);
    said
}

/// A farm with the group [`GROUP`] listing `web` in a plan of one stage, its hosts a1 and a2, and
/// `web` 2.0 wanted.
fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    // One stage of every host, so a2's update takes no place a1 needs.
    farm.group(GROUP, &["web"], Some(policy(|p| p.stages = stages(&[100]))));
    farm.host("a1", GROUP);
    farm.host("a2", GROUP);
    farm.want(GROUP, "web", "2.0");
    farm
}

/// Proves: a host with a package order outstanding, or a package update running, is not started,
/// and is told its package update instead; once it runs the version the update wanted, its turn
/// starts.
///
/// Catches: a maintenance turn started on a host that is updating a package (two things at once on
/// a host).
#[test]
fn a_host_in_a_package_update_is_given_no_maintenance() {
    let farm = farm("sm-package-first");
    let world = &farm.world;
    let (reply, _) = ask_typed(
        &world.rig,
        "a1",
        "storage",
        farm_beat(runs("web", "1.0"), None),
    );
    assert_eq!(
        reply,
        update("web", "2.0"),
        "no run yet: the update is ordered"
    );
    tell(world, "a2", farm_beat(runs("web", "2.0"), None));
    edit_runs(world, |held| {
        let mut run = run_of(
            RUN,
            vec![host_at("a1", GROUP, Step::Waiting, 0, &[Kind::Linux])],
        );
        run.take_last_host = true;
        held.push(run);
    });

    let (reply, order) = ask_typed(
        &world.rig,
        "a1",
        "storage",
        farm_beat(runs("web", "1.0"), None),
    );
    assert_eq!(reply, update("web", "2.0"), "its order is outstanding");
    assert_eq!(maintain_id(order.as_ref()), None);
    let mut updating = runs("web", "1.0");
    updating.updating = Some(version("2.0"));
    assert_eq!(
        maintain_id(tell(world, "a1", farm_beat(updating, None)).as_ref()),
        None
    );
    assert_eq!(steps(world), vec![("a1".to_owned(), Step::Waiting)]);

    let order = tell(world, "a1", farm_beat(runs("web", "2.0"), None));
    assert_eq!(maintain_id(order.as_ref()), Some(ORDER));
}

/// Proves: a host in its turn is given no package order, while a host of its group outside the run
/// is ordered the same update; once its daemon reports its turn done, it is ordered the update.
///
/// Catches: a package update ordered to a host that is draining or rebooting for maintenance.
#[test]
fn a_host_in_its_turn_is_given_no_package_order() {
    let farm = farm("sm-turn-first");
    let world = &farm.world;
    edit_runs(world, |held| {
        held.push(run_of(
            RUN,
            vec![host_at("a1", GROUP, Step::Staging, UNIX, &[Kind::Linux])],
        ));
    });

    let draining = farm_beat(runs("web", "1.0"), Some(MaintenanceStep::Draining));
    assert_eq!(
        ask_typed(&world.rig, "a1", "storage", draining),
        (ServiceReply::Nothing, None)
    );
    let (reply, _) = ask_typed(
        &world.rig,
        "a2",
        "storage",
        farm_beat(runs("web", "1.0"), None),
    );
    assert_eq!(reply, update("web", "2.0"), "a2 is in no turn");

    let done = farm_beat(runs("web", "1.0"), Some(MaintenanceStep::Done));
    let (reply, _) = ask_typed(&world.rig, "a1", "storage", done);
    assert_eq!(reply, update("web", "2.0"), "a1's turn is done");
}

/// Proves: a host that asks again before its daemon reports the order is sent the same order (the
/// same id and try 1), no second host takes a place and the start is audited once; once its daemon
/// reports the order (waiting, then staging) nothing more is sent and its step stays staging; a
/// run record of another order id is not its order, so the order is sent again.
///
/// Catches: a new order, or a new try, at every heartbeat; or an order sent forever to a host that
/// holds it.
#[test]
fn a_second_heartbeat_does_not_issue_a_new_order() {
    let world = scheduled("sm-again", table(1, 0), &["h1", "h2"], |_| {});
    let first = tell(&world, "h1", beat(LINUX, None)).expect("an order");
    assert_eq!(maintain_id(Some(&first)), Some(ORDER));

    assert_eq!(tell(&world, "h1", beat(LINUX, None)), Some(first.clone()));
    let mut no_report = beat(LINUX, None);
    no_report.maintenance = None;
    assert_eq!(
        tell(&world, "h1", no_report),
        Some(first.clone()),
        "a beat that carries no report names no run: the order is sent again"
    );
    assert!(!starts(&world, "h2"));
    assert_eq!(
        maintenance_audit(&world),
        vec![coordinator_said(
            "maintenance_start",
            "h1",
            &format!("run {RUN}")
        )]
    );

    for step in [MaintenanceStep::Waiting, MaintenanceStep::Staging] {
        assert_eq!(tell(&world, "h1", at_step(step)), None, "{step:?}");
        assert_eq!(in_turn(&world), vec!["h1"]);
        assert_eq!(steps(&world)[0], ("h1".to_owned(), Step::Staging));
    }
    let another_run = beat(
        LINUX,
        Some(("run-1-1", "maintain-run-1-1", MaintenanceStep::Draining)),
    );
    assert_eq!(
        tell(&world, "h1", another_run),
        Some(first.clone()),
        "a record of another run is not this order"
    );
    let other = beat(
        LINUX,
        Some((RUN, "maintain-another", MaintenanceStep::Draining)),
    );
    assert_eq!(tell(&world, "h1", other), Some(first));
    assert_eq!(steps(&world)[0], ("h1".to_owned(), Step::Staging));
}

/// Proves: a coordinator that starts again over the same store and enrollment sends no order to a
/// host whose daemon reports the run's order, keeps the host's place (no other host starts), and
/// starts the next host once the first reports its turn done.
///
/// Catches: a restarted coordinator that forgets which turns it started and orders a host, or the
/// rest of its group, a second time.
#[test]
fn a_restarted_coordinator_does_not_start_an_order_twice() {
    let world = scheduled("sm-restart", table(1, 0), &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));

    let mut rig = Rig::new("sm-restart-again");
    rig.coordinator = coordinator_on(
        &world.fake,
        &rig.clock,
        Some(Arc::clone(&world.enrollment.enrollment)),
    );
    rig.at(2_000);
    let ask = |host: &str, said: Heartbeat| ask_typed(&rig, host, "storage", said).1;
    let ops = || rig.coordinator.ops().state().maintenance_runs;

    assert_eq!(ask("h1", at_step(MaintenanceStep::Staging)), None);
    assert_eq!(maintain_id(ask("h2", beat(LINUX, None)).as_ref()), None);
    assert_eq!(ops()[0].hosts[0].step, Step::Staging);

    assert_eq!(ask("h1", at_step(MaintenanceStep::Done)), None);
    assert_eq!(ops()[0].hosts[0].step, Step::Done);
    assert_eq!(
        maintain_id(ask("h2", beat(LINUX, None)).as_ref()),
        Some(ORDER)
    );
}

/// Proves: each step a host's daemon reports of its turn is stored with the time it was read, the
/// kinds applied and the reason, and audited `maintenance_step` with `run <id> <step>`; a report
/// that says what is held already writes nothing.
///
/// Catches: a run whose hosts never leave staging (so its batch never frees and it never ends), or
/// a write and an audit entry at every heartbeat.
#[test]
fn the_step_a_host_reports_is_stored_and_audited() {
    let world = scheduled("sm-fold", table(1, 0), &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Draining));
    let mut halted = at_step(MaintenanceStep::Halted);
    let report = agent::MaintenanceReport {
        run: Some(agent::MaintenanceRunState {
            run_id: RUN.to_owned(),
            order_id: ORDER.to_owned(),
            step: MaintenanceStep::Halted as i32,
            applied: vec![MaintenanceKind::Linux as i32],
            halted_reason: "check: exit 1".to_owned(),
        }),
        ..agent::MaintenanceReport::default()
    };
    halted.maintenance = Some(crate::maintenance::text_of(&report));
    tell(&world, "h1", halted.clone());
    tell(&world, "h1", halted);

    let host = world.ops().maintenance_runs[0].hosts[0].clone();
    assert_eq!(
        (host.step, host.since_unix, host.applied, host.reason),
        (
            Step::Halted,
            UNIX,
            vec![Kind::Linux],
            "check: exit 1".to_owned()
        )
    );
    let run = format!("run {RUN}");
    assert_eq!(
        maintenance_audit(&world),
        vec![
            coordinator_said("maintenance_start", "h1", &run),
            coordinator_said("maintenance_step", "h1", &format!("{run} draining")),
            coordinator_said("maintenance_step", "h1", &format!("{run} halted")),
        ]
    );
}

/// Proves: a heartbeat over the JSON route, which carries no typed order, starts no turn and writes
/// nothing; the same host's next heartbeat over gRPC starts it.
///
/// Catches: a turn stored as started for a host that was never sent its order.
#[test]
fn the_json_route_starts_no_turn() {
    let world = scheduled("sm-json", table(1, 0), &["h1"], |_| {});
    assert_eq!(
        ask_json(&world.rig, "h1", "storage", beat(LINUX, None)),
        ServiceReply::Nothing
    );
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::Waiting)]);
    assert!(maintenance_audit(&world).is_empty());
    assert!(starts(&world, "h1"));
    assert_eq!(
        ask_json(&world.rig, "h1", "storage", beat(LINUX, None)),
        ServiceReply::Nothing,
        "in its turn, unreported: nothing over a route that cannot carry the order"
    );
}

/// A heartbeat of a host that reports a Linux update due and the run record `(run, order)` at the
/// step numbered `step`, with `applied` and `reason`.
fn record(
    run: &str,
    order: &str,
    step: i32,
    applied: &[MaintenanceKind],
    reason: &str,
) -> Heartbeat {
    let mut said = beat(LINUX, None);
    let report = agent::MaintenanceReport {
        due: vec![agent::DueItem {
            kind: MaintenanceKind::Linux as i32,
            label: "pkg".to_owned(),
            ..agent::DueItem::default()
        }],
        run: Some(agent::MaintenanceRunState {
            run_id: run.to_owned(),
            order_id: order.to_owned(),
            step,
            applied: applied.iter().map(|kind| *kind as i32).collect(),
            halted_reason: reason.to_owned(),
        }),
        ..agent::MaintenanceReport::default()
    };
    said.maintenance = Some(crate::maintenance::text_of(&report));
    said
}

/// Proves: each step a daemon reports maps to the host's step (its waiting to staging, deferred and
/// halted as themselves), a step it does not name (unspecified, or a number no release has)
/// changes nothing, a done turn stays done whatever is reported after, a host that waits and
/// reports the run's order (one another coordinator started) takes the step reported, and a report
/// of a run the coordinator does not hold, or from a host the run does not hold, changes nothing.
///
/// Catches: a step mapped to the wrong one (a rebooting host read as done frees its batch place
/// while it is down), or a stale report that reopens a done turn.
#[test]
fn every_step_a_daemon_reports_maps_to_the_hosts_step() {
    let world = scheduled("sm-steps", table(2, 0), &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));
    let host =
        |world: &crate::host_groups_support::World| world.ops().maintenance_runs[0].hosts[0].step;
    for (said, held) in [
        (MaintenanceStep::Waiting as i32, Step::Staging),
        (MaintenanceStep::Draining as i32, Step::Draining),
        (MaintenanceStep::Unspecified as i32, Step::Draining),
        (99, Step::Draining),
        (MaintenanceStep::Applying as i32, Step::Applying),
        (MaintenanceStep::Rebooting as i32, Step::Rebooting),
        (MaintenanceStep::Checking as i32, Step::Checking),
        (MaintenanceStep::PuttingBack as i32, Step::PuttingBack),
        (MaintenanceStep::Deferred as i32, Step::Deferred),
        (MaintenanceStep::Staging as i32, Step::Staging),
        (MaintenanceStep::Halted as i32, Step::Halted),
        (MaintenanceStep::Done as i32, Step::Done),
        (MaintenanceStep::Draining as i32, Step::Done),
    ] {
        tell(&world, "h1", record(RUN, ORDER, said, &[], ""));
        assert_eq!(host(&world), held, "{said}");
    }

    tell(
        &world,
        "h2",
        record(RUN, ORDER, MaintenanceStep::Draining as i32, &[], ""),
    );
    assert_eq!(steps(&world)[1], ("h2".to_owned(), Step::Draining));
    let before = world.ops().maintenance_runs;
    tell(
        &world,
        "h2",
        record(
            "run-1-1",
            "maintain-run-1-1",
            MaintenanceStep::Done as i32,
            &[],
            "",
        ),
    );
    tell(
        &world,
        "h9",
        record(RUN, ORDER, MaintenanceStep::Done as i32, &[], ""),
    );
    assert_eq!(world.ops().maintenance_runs, before);
}

/// Proves: a start that cannot be stored sends no order and leaves the host waiting, and a step
/// that cannot be stored leaves the step held; once the store takes writes again, the same
/// heartbeat starts the host, and the same report stores the step.
///
/// Catches: an order sent for a turn the store does not hold (a restart would start it again), or
/// a lost step that is never read again.
#[test]
fn a_start_or_a_step_that_cannot_be_stored_changes_nothing() {
    let world = scheduled("sm-store-fails", table(1, 0), &["h1"], |_| {});
    world.fake.refuse(|_| Some(crate::store::store_failure()));
    assert_eq!(
        maintain_id(tell(&world, "h1", beat(LINUX, None)).as_ref()),
        None
    );
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::Waiting)]);
    world.fake.heal();
    assert!(starts(&world, "h1"));

    world.fake.refuse(|_| Some(crate::store::store_failure()));
    assert_eq!(tell(&world, "h1", at_step(MaintenanceStep::Draining)), None);
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::Staging)]);
    world.fake.heal();
    tell(&world, "h1", at_step(MaintenanceStep::Draining));
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::Draining)]);
}

/// Proves: a coordinator in its quiet start starts no turn, and starts it once the quiet start is
/// over.
///
/// Catches: a turn started from a fleet still being rebuilt from heartbeats, whose serving count
/// is not yet known.
#[test]
fn no_turn_starts_in_the_quiet_start() {
    let world = scheduled("sm-quiet", table(1, 0), &["h1"], |_| {});
    let mut rig = Rig::new("sm-quiet-again");
    rig.coordinator = coordinator_on(
        &world.fake,
        &rig.clock,
        Some(Arc::clone(&world.enrollment.enrollment)),
    );
    let ask = |rig: &Rig| ask_typed(rig, "h1", "storage", beat(LINUX, None)).1;
    assert_eq!(maintain_id(ask(&rig).as_ref()), None, "in the quiet start");
    rig.at(2_000);
    assert_eq!(maintain_id(ask(&rig).as_ref()), Some(ORDER));
}

/// Proves: a start decided before an operator's `Cancel` and stored after it stores nothing, sends
/// no order and writes no audit entry: the host stays not reached.
///
/// Catches: an order sent for a turn of a cancelled run, which the coordinator would not hold.
#[test]
fn a_start_after_the_run_was_cancelled_stores_and_sends_nothing() {
    let world = scheduled("sm-cancelled", table(1, 0), &["h1"], |_| {});
    let ordered = crate::decide_maintenance::Ordered {
        run_id: RUN.to_owned(),
        host_group: GROUP.to_owned(),
        order_id: ORDER.to_owned(),
        order_try: 1,
        maintain: Maintain {
            run_id: RUN.to_owned(),
            action: agent::MaintainAction::Run,
            kinds: vec![MaintenanceKind::Linux],
            linux_scope: agent::LinuxScope::Security,
            drain_deadline_secs: None,
            macos_path: agent::MacosPath::Install,
            macos_major: false,
            return_secs: 1200,
            xcode: None,
        },
    };
    edit_runs(&world, |runs| {
        runs[0].cancel(UNIX);
        runs.push(run_of("run-1-1", Vec::new()));
    });

    let next = world
        .coordinator()
        .start_turn(&crate::testutil::id("h1"), ordered, UNIX);
    assert!(matches!(next, Next::Busy));
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::NotReached)]);
    assert!(maintenance_audit(&world).is_empty());
}
