//! The rules that start a host's turn of a maintenance run ([`crate::decide_maintenance`]), each
//! through the coordinator's heartbeat with typed orders: the batch, groups in parallel, the
//! serving count, the window, the order of hosts by node id, and the hosts left out. The builders
//! are shared with `service_maintain_tests.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceKind, MaintenanceStep};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Health, Heartbeat};

use crate::host_groups_support::{SELF, UNIX, World};
use crate::maintenance::{FEATURE, text_of};
use crate::maintenance_calls_tests::{
    GROUP, LINUX, edit_runs, group_request, host_at, hosts_request, member, run_of, schedule,
    waiting,
};
use crate::maintenance_runs::{Kind, Step};
use crate::marks_support::{NEW_DAEMON, ask_typed, beat_of, hold, runs, scope_host, unhold};
use crate::orders::Order;

/// The id of the first run a world stores.
pub(crate) const RUN: &str = "run-1900000000-1";

/// The order id of [`RUN`]'s `maintain`.
pub(crate) const ORDER: &str = "maintain-run-1900000000-1";

/// A heartbeat of a daemon that announces `maintenance-v1`, lists `web` healthy at 1.0, and
/// reports `due`, and, when given, the run record `(run, order, step)`.
pub(crate) fn beat(
    due: &[MaintenanceKind],
    record: Option<(&str, &str, MaintenanceStep)>,
) -> Heartbeat {
    let mut features: Vec<&str> = NEW_DAEMON.to_vec();
    features.push(FEATURE);
    let mut beat = beat_of(&features, vec![runs("web", "1.0")]);
    let report = agent::MaintenanceReport {
        due: due
            .iter()
            .map(|kind| agent::DueItem {
                kind: *kind as i32,
                label: "pkg".to_owned(),
                ..agent::DueItem::default()
            })
            .collect(),
        run: record.map(|(run, order, step)| agent::MaintenanceRunState {
            run_id: run.to_owned(),
            order_id: order.to_owned(),
            step: step as i32,
            applied: Vec::new(),
            halted_reason: String::new(),
        }),
        ..agent::MaintenanceReport::default()
    };
    beat.maintenance = Some(text_of(&report));
    beat
}

/// [`beat`] of a host that reports a Linux update due and the run record of [`RUN`] taken by
/// [`ORDER`] at `step`.
pub(crate) fn at_step(step: MaintenanceStep) -> Heartbeat {
    beat(LINUX, Some((RUN, ORDER, step)))
}

/// `beat` with every service it lists unhealthy.
pub(crate) fn unhealthy(mut beat: Heartbeat) -> Heartbeat {
    for service in beat.services.iter_mut().flatten() {
        service.health = Health::Unhealthy;
    }
    beat
}

/// The typed order the coordinator answers a heartbeat `beat` of `host` with.
pub(crate) fn tell(world: &World, host: &str, beat: Heartbeat) -> Option<Order> {
    ask_typed(&world.rig, host, "storage", beat).1
}

/// The order id of `order` when it is a `maintain`.
pub(crate) fn maintain_id(order: Option<&Order>) -> Option<&str> {
    match order? {
        Order::Maintain { order_id, .. } => Some(order_id.as_str()),
        _ => None,
    }
}

/// Whether a heartbeat of `host` that reports a Linux update due and no run is answered with
/// [`RUN`]'s `maintain`.
pub(crate) fn starts(world: &World, host: &str) -> bool {
    let order = tell(world, host, beat(LINUX, None));
    match maintain_id(order.as_ref()) {
        Some(id) => {
            assert_eq!(id, ORDER, "the order id of the first run");
            true
        }
        None => false,
    }
}

/// Each host of the first run the world stores with its step.
pub(crate) fn steps(world: &World) -> Vec<(String, Step)> {
    world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .map(|host| (host.host_id.clone(), host.step))
        .collect()
}

/// The hosts of the first run in their turn.
pub(crate) fn in_turn(world: &World) -> Vec<String> {
    steps(world)
        .into_iter()
        .filter(|(_, step)| step.in_turn())
        .map(|(host, _)| host)
        .collect()
}

/// The maintenance table of a group with `batch` and `min_serving`.
pub(crate) fn table(batch: u32, min_serving: u32) -> proto::MaintenancePolicy {
    proto::MaintenancePolicy {
        batch: Some(batch),
        min_serving: Some(min_serving),
        ..proto::MaintenancePolicy::default()
    }
}

/// The group `group` put with the maintenance table `table` and the hosts `hosts` in it, each heard
/// to report a Linux update due.
pub(crate) fn group_with(
    world: &World,
    group: &str,
    table: proto::MaintenancePolicy,
    hosts: &[&str],
) {
    let policy = proto::RolloutPolicy {
        maintenance: Some(table),
        ..proto::RolloutPolicy::default()
    };
    world
        .put_with(group, &[SELF], Some(policy), None)
        .expect("put");
    for host in hosts {
        member(world, group, host);
        assert!(!starts(world, host), "no run yet: nothing is ordered");
    }
}

/// A world named `name` with the group [`GROUP`] of the table `table` and `hosts`, and a run of
/// the group scheduled now that may take its last host (every host of the group has something
/// due, so `Schedule` asks for it), with `request`'s changes.
pub(crate) fn scheduled(
    name: &str,
    table: proto::MaintenancePolicy,
    hosts: &[&str],
    request: impl FnOnce(&mut proto::ScheduleMaintenanceRequest),
) -> World {
    let world = World::new(name);
    group_with(&world, GROUP, table, hosts);
    let mut asked = group_request(GROUP);
    asked.take_last_host = true;
    request(&mut asked);
    let run = schedule(&world, asked)
        .expect("scheduled")
        .run
        .expect("a run");
    assert_eq!(run.id, RUN);
    world
}

/// Proves: in a group of five with a run of batch 2, the first two hosts are ordered and the
/// other three are not, however often they ask; once a host's daemon reports its turn done, the
/// next host is ordered, and at no heartbeat are more than two hosts between staging and done.
///
/// Catches: a batch that counts only the hosts its own heartbeat sees, or that frees a place
/// before the host's turn has ended.
#[test]
fn a_batch_of_two_never_has_three_hosts_in_their_turn() {
    let hosts = ["h1", "h2", "h3", "h4", "h5"];
    let world = scheduled("dm-batch", table(1, 1), &hosts, |asked| {
        asked.batch = Some(2);
    });

    // In the second round h1 and h2 are sent their own order again (their daemons have not
    // reported it), and nobody else is ordered.
    for round in 0..2 {
        for host in hosts {
            let ordered = starts(&world, host);
            assert_eq!(
                ordered,
                host == "h1" || host == "h2",
                "{host} in round {round}"
            );
            assert!(in_turn(&world).len() <= 2, "{:?}", steps(&world));
        }
    }
    assert_eq!(in_turn(&world), vec!["h1", "h2"]);

    tell(&world, "h1", at_step(MaintenanceStep::Draining));
    assert!(!starts(&world, "h3"), "h1 still holds its place");
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert!(starts(&world, "h3"));
    assert!(!starts(&world, "h4"));
    assert_eq!(in_turn(&world), vec!["h2", "h3"]);
}

/// Proves: a run over hosts of two groups, each of batch 1, orders one host of each group at once:
/// the batch is counted per group, and a group's full batch holds back no host of another group.
///
/// Catches: a batch counted over the whole run, which would leave the second group waiting.
#[test]
fn groups_go_in_parallel_each_under_its_own_batch() {
    let world = World::new("dm-groups");
    group_with(&world, "alpha", table(1, 0), &["a1", "a2"]);
    group_with(&world, "beta", table(1, 0), &["b1", "b2"]);
    let mut asked = hosts_request(&["a1", "a2", "b1", "b2"]);
    asked.take_last_host = true;
    schedule(&world, asked).expect("scheduled");

    assert!(starts(&world, "a1"));
    assert!(!starts(&world, "a2"), "alpha's batch of 1 is taken");
    assert!(starts(&world, "b1"), "beta's batch is its own");
    assert!(!starts(&world, "b2"));
    assert_eq!(in_turn(&world), vec!["a1", "b1"]);
}

/// Proves: in a group of two with `min_serving` 1 and a batch of 2, the first host is taken (two
/// serve) and the second is not while the first is staging, draining or halted (one serves); once
/// the first is done, the second is taken.
///
/// Catches: a serving count that counts a host in its turn as serving because it still
/// heartbeats, or a halted host as serving.
#[test]
fn no_host_is_taken_while_fewer_than_min_serving_plus_one_serve() {
    let world = scheduled("dm-serving", table(2, 1), &["h1", "h2"], |_| {});

    assert!(starts(&world, "h1"));
    assert!(!starts(&world, "h2"), "h1 staging does not serve");
    for step in [MaintenanceStep::Draining, MaintenanceStep::Halted] {
        tell(&world, "h1", at_step(step));
        assert!(!starts(&world, "h2"), "h1 {step:?} does not serve");
    }
    edit_runs(&world, |runs| runs[0].hosts[0].step = Step::Done);
    tell(&world, "h1", beat(LINUX, None));
    assert!(starts(&world, "h2"));
}

/// Proves: a host whose service reports unhealthy does not count as serving: in a group of three
/// with `min_serving` 2, no host is taken while one of the three is unhealthy, and the first is
/// taken once all three are healthy.
///
/// Catches: a serving count that counts every live host, healthy or not.
#[test]
fn an_unhealthy_host_does_not_count_as_serving() {
    let world = scheduled("dm-unhealthy", table(1, 2), &["h1", "h2", "h3"], |_| {});

    tell(&world, "h3", unhealthy(beat(LINUX, None)));
    assert!(!starts(&world, "h1"), "two serve, three are needed");
    assert!(!starts(&world, "h3"));
    tell(&world, "h3", beat(LINUX, None));
    assert!(starts(&world, "h1"));
}

/// Proves: a group of one host, whose `min_serving` 1 holds it back, is taken by a run that may
/// take a group's last host, and not by one that may not.
///
/// Catches: a last host that a run which said so can never take, or one taken without it.
/// (In a larger group the same run still keeps `min_serving`:
/// [`no_host_is_taken_while_fewer_than_min_serving_plus_one_serve`] runs with it set.)
#[test]
fn a_group_of_one_is_taken_only_by_a_run_that_takes_its_last_host() {
    let world = World::new("dm-last-host");
    group_with(&world, GROUP, table(1, 1), &["h1"]);
    edit_runs(&world, |runs| {
        runs.push(run_of(RUN, vec![waiting("h1", GROUP, &[Kind::Linux])]));
    });
    assert!(!starts(&world, "h1"), "the run may not take the last host");

    edit_runs(&world, |runs| runs[0].take_last_host = true);
    assert!(starts(&world, "h1"));
}

/// Proves: no host starts before the run's window opens or after it ends, a host starts inside it,
/// and a host that started before the window ended is still sent its order and still moves through
/// its steps after it ended.
///
/// Catches: a window read only at Schedule, or one that cuts a host already in its turn.
#[test]
fn the_window_starts_no_host_outside_it_and_never_cuts_one_that_started() {
    let world = scheduled("dm-window", table(2, 0), &["h1", "h2"], |asked| {
        asked.window_start_unix = UNIX + 100;
        asked.window_end_unix = UNIX + 200;
    });
    assert!(!starts(&world, "h1"), "before the window opens");

    edit_runs(&world, |runs| {
        runs[0].window_start_unix = UNIX - 100;
        runs[0].window_end_unix = UNIX + 100;
    });
    assert!(starts(&world, "h1"), "inside the window");

    edit_runs(&world, |runs| {
        runs[0].window_start_unix = UNIX - 200;
        runs[0].window_end_unix = UNIX - 100;
    });
    assert!(!starts(&world, "h2"), "after the window ends");
    assert!(starts(&world, "h1"), "h1 started: its order is sent again");
    tell(&world, "h1", at_step(MaintenanceStep::Draining));
    assert_eq!(
        steps(&world),
        vec![
            ("h1".to_owned(), Step::Draining),
            ("h2".to_owned(), Step::Waiting)
        ]
    );
}

/// Proves: hosts go by node id: with a batch of 1, hosts that ask before a host with a lower node id
/// are not ordered, and the next place goes to the next node id; a host with a lower id that cannot
/// go now (held) does not hold back the one after it.
///
/// Catches: places given to whichever host asks first.
#[test]
fn hosts_go_in_node_id_order() {
    let world = scheduled("dm-order", table(1, 0), &["c1", "a1", "b1", "d1"], |_| {});

    assert!(!starts(&world, "c1"));
    assert!(!starts(&world, "b1"));
    assert!(starts(&world, "a1"));
    tell(&world, "a1", at_step(MaintenanceStep::Done));
    assert!(!starts(&world, "c1"));
    hold(&world.rig, "web", scope_host("b1")).expect("held");
    assert!(starts(&world, "c1"), "b1 is held: c1 is next");
    assert_eq!(in_turn(&world), vec!["c1"]);
}

/// Proves: a host that waits in a run is not started while a hold stands on it for any package, nor
/// while it is excluded from one, and is started once the hold is released and the exclusion
/// lifted.
///
/// Catches: holds and exclusions read only at Schedule, so a hold set after it does not stop the
/// host.
#[test]
fn a_held_or_excluded_host_is_skipped_until_released() {
    let world = scheduled("dm-held", table(1, 0), &["h1"], |asked| {
        asked.take_last_host = true;
    });
    hold(&world.rig, "web", scope_host("h1")).expect("held");
    assert!(!starts(&world, "h1"), "held");
    unhold(&world.rig, "web", scope_host("h1")).expect("released");

    world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            |state| {
                state.excluded.push(crate::ops::Exclusion {
                    service: "db".to_owned(),
                    node_id: "h1".to_owned(),
                    reason: "broken".to_owned(),
                });
            },
        )
        .expect("excluded");
    assert!(!starts(&world, "h1"), "excluded");
    world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            |state| state.excluded.clear(),
        )
        .expect("lifted");
    assert!(starts(&world, "h1"));
}

/// Proves: a host whose daemon does not announce `maintenance-v1` is sent no `maintain`, even when
/// a run holds it waiting; and a host the run left out (skipped) is never ordered, even once its
/// daemon announces it.
///
/// Catches: an order to a daemon that cannot read it, or a skipped host ordered later.
#[test]
fn an_older_daemon_and_a_skipped_host_are_sent_no_maintain() {
    let world = World::new("dm-older");
    group_with(&world, GROUP, table(2, 0), &["h1", "h2"]);
    edit_runs(&world, |runs| {
        let mut skipped = waiting("h2", GROUP, &[Kind::Linux]);
        skipped.step = Step::Skipped;
        skipped.skipped_because = Some(crate::maintenance_runs::Skip::NoMaintenanceV1);
        runs.push(run_of(
            RUN,
            vec![waiting("h1", GROUP, &[Kind::Linux]), skipped],
        ));
    });
    let mut older = beat(LINUX, None);
    older.agent_features.retain(|feature| feature != FEATURE);

    assert_eq!(maintain_id(tell(&world, "h1", older).as_ref()), None);
    assert!(!starts(&world, "h2"), "skipped");
    assert!(starts(&world, "h1"), "the same host announcing it");
}

/// Proves: a host moved to another group while it waits is not started, and nor is a host whose
/// group's run halted (a host of the group halted).
///
/// Catches: a host taken by the rules of a group it left, or a run that goes on after a halt.
#[test]
fn a_host_moved_or_in_a_halted_group_is_not_started() {
    let world = scheduled("dm-moved", table(3, 0), &["h1", "h2", "h3"], |_| {});
    world.put("beta", &[SELF], None).expect("put");
    world.move_to("h3", "beta").expect("moved");
    assert!(!starts(&world, "h3"), "its group changed");

    edit_runs(&world, |runs| {
        runs[0].hosts[0] = host_at("h1", GROUP, Step::Halted, UNIX, &[Kind::Linux]);
    });
    assert!(!starts(&world, "h2"), "h1 halted the group");
    assert_eq!(
        steps(&world),
        vec![
            ("h1".to_owned(), Step::Halted),
            ("h2".to_owned(), Step::Waiting),
            ("h3".to_owned(), Step::Waiting)
        ]
    );
}

/// Proves: a host the coordinator has not heard within its liveness bound neither counts as
/// serving nor holds back the hosts after it: with the lowest node id silent, the next host is
/// started; the silent host, heard again, waits for the batch.
///
/// Catches: a silent host that keeps the first place forever, so its group never goes on.
#[test]
fn a_silent_host_holds_back_no_other_host() {
    let world = scheduled("dm-silent", table(1, 1), &["h1", "h2", "h3"], |_| {});
    world.rig.at(2_100);
    tell(&world, "h3", beat(LINUX, None));
    assert!(
        starts(&world, "h2"),
        "h1 is silent: h2 is next, and h2 and h3 serve"
    );
    assert!(!starts(&world, "h1"), "the batch is taken");
    assert_eq!(in_turn(&world), vec!["h2"]);
}

/// Proves: hosts in no group are one pool with every default (a batch of 1, `min_serving` 1): a run
/// of one such host does not start it while it is the only host in no group the coordinator has
/// heard, and starts it once a second one is heard.
///
/// Catches: a host in no group taken with no serving count, or never taken because no group
/// policy is found for it.
#[test]
fn hosts_in_no_group_are_one_pool_with_every_default() {
    let world = World::new("dm-no-group");
    world.put("alpha", &[SELF], None).expect("put");
    for host in ["l1", "l2"] {
        world.enroll(host);
    }
    tell(&world, "l1", beat(LINUX, None));
    schedule(&world, hosts_request(&["l1"])).expect("scheduled");
    assert_eq!(world.ops().maintenance_runs[0].hosts[0].host_group, "");

    assert!(
        !starts(&world, "l1"),
        "one host in no group serves; two are needed"
    );
    tell(&world, "l2", beat(&[], None));
    assert!(starts(&world, "l1"));
}

/// Proves: when the hosts' groups cannot be read from the enrollment store at a heartbeat, no turn
/// starts at it, and the next heartbeat, which reads them, starts the host.
///
/// Catches: a turn started on what an unreadable store left out, as a host in no group.
#[test]
fn no_turn_starts_while_the_hosts_groups_cannot_be_read() {
    let world = World::new("dm-groups-unread");
    group_with(&world, GROUP, table(1, 0), &["h1", "h2"]);
    tell(&world, "h2", beat(&[], None));
    let mut asked = group_request(GROUP);
    asked.take_last_host = true;
    schedule(&world, asked).expect("scheduled");
    assert_eq!(steps(&world), vec![("h1".to_owned(), Step::Waiting)]);

    // A heartbeat of a host in no run reads the records as many times as one that waits does
    // before its groups are read.
    let before = world.records.count();
    tell(&world, "h2", beat(&[], None));
    let reads = world.records.count() - before;
    world.records.fail_at(world.records.count() + reads + 1);
    assert!(!starts(&world, "h1"), "the groups' read fails");
    assert!(starts(&world, "h1"));
}
