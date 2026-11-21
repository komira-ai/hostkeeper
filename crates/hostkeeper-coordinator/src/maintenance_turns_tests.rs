//! How a host's turn of a maintenance run ends, from what its daemon reports in its heartbeats
//! (`docs/design/maintenance-updates.md`, sections 5.3, 5.4, 5.6 and 5.7), through the
//! coordinator's heartbeat with typed orders: a done host holds its batch place until its group's
//! `bake_beats` healthy heartbeats, a deferred host is offered again after the others under a new
//! order id once its backoff has passed and only while the window is open, a halt stops new hosts
//! in its group only and keeps the host's place, a host not back holds its place until its report
//! moves it on, and a cancel or the window's end leaves the hosts that had not started not
//! reached. The rows of section 5.7's table are each a test below, but the `apple-business` row
//! (not applied), whose path is not built. The builders are in `decide_maintenance_tests.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceStep;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHostStep, MaintenanceState};
use hostkeeper_wire::wire::Heartbeat;

use crate::decide_maintenance::Ordered;
use crate::decide_maintenance::tests::{
    ORDER, RUN, at_step, beat, group_with, maintain_id, scheduled, starts, steps, table, tell,
    unhealthy,
};
use crate::host_groups_support::{SELF, UNIX, World};
use crate::maintenance::text_of;
use crate::maintenance_calls_tests::{
    GROUP, LINUX, cancel, edit_runs, get, group_request, hosts_request, member, run_of, schedule,
    waiting,
};
use crate::maintenance_runs::{Kind, MAX_OFFERED_AGAIN, RunHost, Step};
use crate::orders::Order;

use super::Next;

/// The order id of [`RUN`]'s second offer to a host, after it deferred once.
const ORDER_2: &str = "maintain-run-1900000000-1-2";

/// The order id of [`RUN`]'s third offer to a host, after it deferred twice.
const ORDER_3: &str = "maintain-run-1900000000-1-3";

/// A heartbeat of a host that reports a Linux update due and the run record of [`RUN`] taken by
/// `order` at `step`, with `reason`.
fn report(order: &str, step: MaintenanceStep, reason: &str) -> Heartbeat {
    let mut said = beat(LINUX, None);
    let record = agent::MaintenanceReport {
        due: vec![agent::DueItem {
            kind: agent::MaintenanceKind::Linux as i32,
            label: "pkg".to_owned(),
            ..agent::DueItem::default()
        }],
        run: Some(agent::MaintenanceRunState {
            run_id: RUN.to_owned(),
            order_id: order.to_owned(),
            step: step as i32,
            applied: Vec::new(),
            halted_reason: reason.to_owned(),
        }),
        ..agent::MaintenanceReport::default()
    };
    said.maintenance = Some(text_of(&record));
    said
}

/// The turn of `host` in the first run the world stores.
fn turn_of(world: &World, host: &str) -> RunHost {
    world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .find(|held| held.host_id == host)
        .cloned()
        .expect("the run holds the host")
}

/// The order id of the `maintain` a heartbeat `beat` of `host` is answered with, if any.
fn ordered(world: &World, host: &str, beat: Heartbeat) -> Option<String> {
    maintain_id(tell(world, host, beat).as_ref()).map(str::to_owned)
}

/// Sets when the turn of `host` in the first run entered its step.
fn since(world: &World, host: &str, unix: u64) {
    edit_runs(world, |runs| {
        for held in runs[0].hosts.iter_mut().filter(|held| held.host_id == host) {
            held.since_unix = unix;
        }
    });
}

/// The `maintenance_step` entries of the coordinator's audit about `host`, by their reason.
fn steps_audited(world: &World, host: &str) -> Vec<String> {
    world
        .audit()
        .iter()
        .filter(|entry| entry.action == "maintenance_step" && entry.by == "coordinator")
        .filter(|entry| entry.node_id.as_deref() == Some(host))
        .filter_map(|entry| entry.reason.clone())
        .collect()
}

/// A world named `name` with the group [`GROUP`] of the maintenance table `table`, `bake_beats`
/// and `hosts`, each heard, and a run of the group scheduled now that may take its last host.
fn baking(name: &str, table: proto::MaintenancePolicy, bake_beats: u32, hosts: &[&str]) -> World {
    let world = World::new(name);
    let policy = proto::RolloutPolicy {
        bake_beats,
        maintenance: Some(table),
        ..proto::RolloutPolicy::default()
    };
    world
        .put_with(GROUP, &[SELF], Some(policy), None)
        .expect("put");
    for host in hosts {
        member(&world, GROUP, host);
        tell(&world, host, beat(LINUX, None));
    }
    let mut asked = group_request(GROUP);
    asked.take_last_host = true;
    schedule(&world, asked).expect("scheduled");
    world
}

/// Proves (section 5.7, the row `done`; section 5.6): in a group whose `bake_beats` is 2, a host
/// whose daemon reports its turn done holds its batch place, so the next host is not started,
/// until it has sent two healthy heartbeats in a row after the one that reported it done; an
/// unhealthy heartbeat between starts the count again; then the next host starts (the baking
/// host counts as serving, with `min_serving` 1). The count is stored with no audit entry; the
/// end of the bake is audited `maintenance_step` `baked` once, and later heartbeats write nothing.
///
/// Catches: a done host that frees its place at once (the next host drained before the first
/// was proven healthy), a bake that counts the heartbeat that reported done or one that was not
/// healthy, and a write and an audit entry at every heartbeat after it.
#[test]
fn a_done_host_holds_its_place_until_its_groups_bake_beats() {
    let world = baking("mt-bake", table(1, 1), 2, &["h1", "h2"]);
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    let bake = |world: &World| {
        let held = turn_of(world, "h1");
        (held.step, held.baked_beats)
    };
    assert_eq!(
        bake(&world),
        (Step::Done, 0),
        "the done beat is not counted"
    );
    assert!(!starts(&world, "h2"), "h1 bakes: its place is held");

    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(bake(&world), (Step::Done, 1));
    assert!(!starts(&world, "h2"), "one beat of two");
    tell(&world, "h1", unhealthy(at_step(MaintenanceStep::Done)));
    assert_eq!(
        bake(&world),
        (Step::Done, 0),
        "an unhealthy beat starts again"
    );
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert!(!starts(&world, "h2"), "one beat of two again");
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(bake(&world), (Step::Done, 2));
    assert!(starts(&world, "h2"), "h1 baked: the place is free");

    let run = format!("run {RUN}");
    let audited = vec![format!("{run} done"), format!("{run} baked")];
    assert_eq!(steps_audited(&world, "h1"), audited);
    let before = world.ops().maintenance_runs;
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    tell(&world, "h1", unhealthy(at_step(MaintenanceStep::Done)));
    assert_eq!(world.ops().maintenance_runs, before, "a baked host is left");
    assert_eq!(steps_audited(&world, "h1"), audited);
}

/// Proves: in a group whose `bake_beats` is 0, a done host frees its place at the heartbeat that
/// reports it done, and nothing is counted or audited for it.
///
/// Catches: a bake of zero that still holds a place for a heartbeat, or that writes a count.
#[test]
fn a_group_that_bakes_for_no_beat_frees_the_place_at_done() {
    let world = baking("mt-bake-zero", table(1, 0), 0, &["h1", "h2"]);
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert!(starts(&world, "h2"));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(turn_of(&world, "h1").baked_beats, 0);
    assert_eq!(steps_audited(&world, "h1"), vec![format!("run {RUN} done")]);
}

/// Proves (section 5.7, the row `deferred`; section 5.3): a host whose daemon reports its turn
/// deferred, with its reason stored, frees its place and does not halt the run: the next host is
/// started. Once its backoff has passed it is still offered only after the hosts that wait, and
/// when they are done it is sent a new order, `maintain-<run id>-2` with try 1, its offer counted
/// and its reason cleared. Its daemon's record of the first offer (deferred) does not take it back
/// to deferred, and the new order is sent again until the daemon names it; then its step follows
/// that order's record.
///
/// Catches: a deferred host that keeps its place or halts its group, a deferred host offered
/// before the others, one offered under its first order id (which its daemon answers with the
/// end of that turn again, so it would never run), and a stale record that folds it back.
#[test]
fn a_deferred_host_is_offered_again_after_the_others_under_a_new_order() {
    let world = scheduled("mt-deferred", table(1, 0), &["h1", "h2", "h3"], |_| {});
    assert!(starts(&world, "h1"));
    tell(
        &world,
        "h1",
        report(ORDER, MaintenanceStep::Deferred, "stage: exit 75"),
    );
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.reason.as_str()),
        (Step::Deferred, "stage: exit 75")
    );
    since(&world, "h1", UNIX - 10_000);
    assert_eq!(
        ordered(&world, "h1", report(ORDER, MaintenanceStep::Deferred, "")),
        None,
        "the hosts that wait go first"
    );
    assert!(starts(&world, "h2"), "h1 holds no place");
    tell(&world, "h2", at_step(MaintenanceStep::Done));
    assert_eq!(
        ordered(&world, "h1", report(ORDER, MaintenanceStep::Deferred, "")),
        None,
        "h3 still waits"
    );
    assert!(starts(&world, "h3"));
    tell(&world, "h3", at_step(MaintenanceStep::Done));

    let order = tell(&world, "h1", report(ORDER, MaintenanceStep::Deferred, ""));
    assert!(
        matches!(
            &order,
            Some(Order::Maintain { order_id, order_try: 1, .. }) if order_id == ORDER_2
        ),
        "{order:?}"
    );
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.offered_again, held.reason.as_str()),
        (Step::Staging, 1, "")
    );
    assert_eq!(
        ordered(&world, "h1", report(ORDER, MaintenanceStep::Deferred, "")),
        Some(ORDER_2.to_owned()),
        "the first offer's record: the second offer is sent again"
    );
    assert_eq!(turn_of(&world, "h1").step, Step::Staging);
    assert_eq!(
        ordered(&world, "h1", report(ORDER_2, MaintenanceStep::Draining, "")),
        None
    );
    assert_eq!(turn_of(&world, "h1").step, Step::Draining);
}

/// Proves: a deferred host waits its group's `defer_backoff` (300 s, doubling, by default) from
/// the time it deferred: not one second less; the first deferral 300 s, the second 600 s, each
/// offer under the next order id.
///
/// Catches: a deferred host offered again at its next heartbeat, so a stage that fails at once
/// (a full disk) is ordered in a loop, and a backoff that does not grow.
#[test]
fn a_deferred_host_waits_a_backoff_that_grows_with_each_deferral() {
    let world = scheduled("mt-backoff", table(1, 0), &["h1"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", report(ORDER, MaintenanceStep::Deferred, ""));
    let again = |world: &World, order: &str| {
        ordered(world, "h1", report(order, MaintenanceStep::Deferred, ""))
    };
    assert_eq!(again(&world, ORDER), None, "deferred just now");
    since(&world, "h1", UNIX - 299);
    assert_eq!(again(&world, ORDER), None, "a second short");
    since(&world, "h1", UNIX - 300);
    assert_eq!(again(&world, ORDER), Some(ORDER_2.to_owned()));

    tell(&world, "h1", report(ORDER_2, MaintenanceStep::Deferred, ""));
    assert_eq!(turn_of(&world, "h1").step, Step::Deferred);
    since(&world, "h1", UNIX - 599);
    assert_eq!(
        again(&world, ORDER_2),
        None,
        "the second deferral waits 600 s"
    );
    since(&world, "h1", UNIX - 600);
    assert_eq!(again(&world, ORDER_2), Some(ORDER_3.to_owned()));
    assert_eq!(turn_of(&world, "h1").offered_again, 2);
}

/// Proves (section 5.7, the row `not reached`, by the window): once the run's window has ended, a
/// deferred host whose backoff has passed, and a host that waits, are sent nothing, and `Get`
/// shows both not reached since the window's end, the deferred one with its reason; the run is
/// `window_ended`.
///
/// Catches: a deferred host offered again after its window, and hosts shown waiting in a run
/// that can no longer start them.
#[test]
fn no_host_is_offered_once_the_window_ends_and_the_unstarted_are_not_reached() {
    let world = scheduled("mt-window", table(1, 0), &["h1", "h2", "h3"], |asked| {
        asked.window_start_unix = UNIX - 100;
        asked.window_end_unix = UNIX + 100;
    });
    assert!(starts(&world, "h1"));
    tell(
        &world,
        "h1",
        report(ORDER, MaintenanceStep::Deferred, "drain: deadline"),
    );
    since(&world, "h1", UNIX - 10_000);
    edit_runs(&world, |runs| {
        runs[0].window_start_unix = UNIX - 200;
        runs[0].window_end_unix = UNIX - 1;
    });
    assert_eq!(
        ordered(
            &world,
            "h1",
            report(ORDER, MaintenanceStep::Deferred, "drain: deadline")
        ),
        None
    );
    assert!(!starts(&world, "h2"));

    let got = get(&world, RUN).expect("the run");
    assert_eq!(
        got.run.expect("a run").state(),
        MaintenanceState::WindowEnded
    );
    let shown: Vec<(String, MaintenanceHostStep, u64, String)> = got
        .hosts
        .iter()
        .map(|host| {
            (
                host.host_id.clone(),
                host.step(),
                host.since_unix,
                host.reason.clone(),
            )
        })
        .collect();
    let not_reached = MaintenanceHostStep::NotReached;
    assert_eq!(
        shown,
        vec![
            (
                "h1".to_owned(),
                not_reached,
                UNIX - 1,
                "drain: deadline".to_owned()
            ),
            ("h2".to_owned(), not_reached, UNIX - 1, String::new()),
            ("h3".to_owned(), not_reached, UNIX - 1, String::new()),
        ]
    );
}

/// Proves (section 5.7, the row `not reached`, by a cancel): `Cancel` of a run ends the host that
/// waits and the one deferred as not reached, and neither is sent anything after (the deferred
/// one with its backoff passed); the host in its turn finishes it from its own reports.
///
/// Catches: a deferred host left to be offered again by a cancelled run, and a cancel that cuts
/// a host in its turn.
#[test]
fn a_cancel_ends_the_unstarted_hosts_not_reached_and_lets_a_started_one_finish() {
    let world = scheduled("mt-cancel", table(2, 0), &["h1", "h2", "h3"], |_| {});
    assert!(starts(&world, "h1"));
    assert!(starts(&world, "h2"));
    tell(&world, "h2", report(ORDER, MaintenanceStep::Deferred, ""));
    since(&world, "h2", UNIX - 10_000);
    cancel(&world, RUN, "not today").expect("cancelled");
    assert_eq!(
        steps(&world),
        vec![
            ("h1".to_owned(), Step::Staging),
            ("h2".to_owned(), Step::NotReached),
            ("h3".to_owned(), Step::NotReached)
        ]
    );
    assert_eq!(
        ordered(&world, "h2", report(ORDER, MaintenanceStep::Deferred, "")),
        None
    );
    assert!(!starts(&world, "h3"));
    tell(&world, "h1", at_step(MaintenanceStep::Draining));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(turn_of(&world, "h1").step, Step::Done);
}

/// Proves (section 5.7, the row `halted`; section 5.4): a host whose daemon reports its turn
/// halted has its step and reason stored and marks the run halted in its group: no further host
/// of that group starts although its batch has a free place, while the run's other group goes
/// on; `Get` names the halted group. And the halted host keeps its batch place: a host of another
/// run of the group with a batch of 1, which no halt holds back, is not started while it is
/// halted, and is once its turn has ended.
///
/// Catches: a halt that stops every group of the run, one that stops none, a halt reason lost,
/// and a halted host whose place is given to another host while it stays cordoned.
#[test]
fn a_halt_stops_new_hosts_in_its_group_only_and_keeps_its_place() {
    let world = World::new("mt-halt");
    group_with(&world, GROUP, table(2, 0), &["a1", "a2"]);
    group_with(&world, "beta", table(1, 0), &["b1", "b2"]);
    let mut asked = hosts_request(&["a1", "a2", "b1", "b2"]);
    asked.take_last_host = true;
    schedule(&world, asked).expect("scheduled");
    assert!(starts(&world, "a1"));
    tell(
        &world,
        "a1",
        report(ORDER, MaintenanceStep::Halted, "drain: exit 3"),
    );
    let held = turn_of(&world, "a1");
    assert_eq!(
        (held.step, held.reason.as_str()),
        (Step::Halted, "drain: exit 3")
    );
    assert!(!starts(&world, "a2"), "alpha is halted");
    assert!(starts(&world, "b1"), "beta goes on");
    tell(&world, "b1", at_step(MaintenanceStep::Done));
    assert!(starts(&world, "b2"));
    let run = get(&world, RUN).expect("the run").run.expect("a run");
    assert_eq!(
        (run.state(), run.halted_groups),
        (MaintenanceState::Halted, vec![GROUP.to_owned()])
    );

    edit_runs(&world, |runs| {
        let mut other = run_of("run-2-1", vec![waiting("a3", GROUP, &[Kind::Linux])]);
        other.take_last_host = true;
        other.batch = Some(1);
        runs.push(other);
    });
    member(&world, GROUP, "a3");
    let second = |world: &World| ordered(world, "a3", beat(LINUX, None));
    assert_eq!(second(&world), None, "a1 holds alpha's place");
    edit_runs(&world, |runs| runs[0].hosts[0].step = Step::Done);
    assert_eq!(second(&world), Some("maintain-run-2-1".to_owned()));
}

/// Proves (section 5.7, the row `not back`): a host whose turn is not back (silent past its
/// group's `return_secs` while rebooting) halts its group and holds its place; when its daemon
/// reports again, its step follows the report, the halt is lifted but its place is held while it
/// is in its turn, and once it is done the next host starts.
///
/// Catches: a host not back whose place is given away while it may still come back, and one
/// whose return does not resume its turn.
#[test]
fn a_host_not_back_holds_its_place_until_its_report_moves_it_on() {
    let world = scheduled("mt-not-back", table(1, 0), &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Rebooting));
    edit_runs(&world, |runs| runs[0].hosts[0].step = Step::NotBack);
    assert!(!starts(&world, "h2"), "halted, and the place is held");
    assert_eq!(world.ops().maintenance_runs[0].halted_groups(), vec![GROUP]);
    tell(&world, "h1", at_step(MaintenanceStep::Checking));
    assert_eq!(turn_of(&world, "h1").step, Step::Checking);
    assert!(world.ops().maintenance_runs[0].halted_groups().is_empty());
    assert!(!starts(&world, "h2"), "h1 is in its turn");
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert!(starts(&world, "h2"));
}

/// Proves: a deferred host offered again [`MAX_OFFERED_AGAIN`] times is not offered again, its
/// backoff passed; its run does not end while it is deferred, and `Cancel` ends it, the host not
/// reached.
///
/// Catches: an offer past the count whose order id would no longer fit the wire's 64 bytes.
#[test]
fn a_host_offered_again_as_often_as_an_order_id_allows_is_not_offered_more() {
    let world = scheduled("mt-max-offers", table(1, 0), &["h1"], |_| {});
    edit_runs(&world, |runs| {
        let held = &mut runs[0].hosts[0];
        held.step = Step::Deferred;
        held.offered_again = MAX_OFFERED_AGAIN;
    });
    assert!(!starts(&world, "h1"));
    assert_eq!(turn_of(&world, "h1").step, Step::Deferred);
    assert!(!world.ops().maintenance_runs[0].ended_at(UNIX));
    cancel(&world, RUN, "give up").expect("cancelled");
    assert_eq!(turn_of(&world, "h1").step, Step::NotReached);
    assert!(world.ops().maintenance_runs[0].ended_at(UNIX));
}

/// Proves: a start decided for a deferred host under an order id that is no longer its next one
/// (another coordinator offered it again in between) stores nothing, sends nothing and writes
/// no audit entry; the start under its next order id is stored.
///
/// Catches: two offers of one deferred host counted, or sent under one order id.
#[test]
fn a_start_under_a_stale_order_id_stores_and_sends_nothing() {
    let world = scheduled("mt-stale-start", table(1, 0), &["h1"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", report(ORDER, MaintenanceStep::Deferred, ""));
    let before = (world.ops().maintenance_runs, world.audit().len());
    let ordered = |order_id: &str| Ordered {
        run_id: RUN.to_owned(),
        host_group: GROUP.to_owned(),
        order_id: order_id.to_owned(),
        maintain: crate::orders::Maintain {
            run_id: RUN.to_owned(),
            kinds: vec![agent::MaintenanceKind::Linux],
            linux_scope: agent::LinuxScope::Security,
            drain_deadline_secs: None,
            macos_path: agent::MacosPath::Install,
            macos_major: false,
            return_secs: 1200,
        },
    };
    let id = crate::testutil::id("h1");
    let next = world.coordinator().start_turn(&id, ordered(ORDER), UNIX);
    assert!(matches!(next, Next::Busy));
    assert_eq!((world.ops().maintenance_runs, world.audit().len()), before);
    let next = world.coordinator().start_turn(&id, ordered(ORDER_2), UNIX);
    assert!(matches!(next, Next::Order(_)));
    assert_eq!(turn_of(&world, "h1").offered_again, 1);
    let before = (world.ops().maintenance_runs, world.audit().len());
    let next = world.coordinator().start_turn(&id, ordered(ORDER_2), UNIX);
    assert!(matches!(next, Next::Busy), "h1 is in its turn already");
    assert_eq!((world.ops().maintenance_runs, world.audit().len()), before);
}

/// Proves: a bake count the store refuses leaves the count held and the place taken; once the
/// store takes writes again, the next healthy heartbeat counts from what is held.
///
/// Catches: a bake counted in memory only, which a restart would read as baked or not baked at
/// random, and a refused write that frees the place.
#[test]
fn a_bake_count_that_cannot_be_stored_counts_again_at_the_next_beat() {
    let world = baking("mt-bake-store", table(1, 0), 1, &["h1", "h2"]);
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    world.fake.refuse(|_| Some(crate::store::store_failure()));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    world.fake.heal();
    assert_eq!(turn_of(&world, "h1").baked_beats, 0);
    assert!(!starts(&world, "h2"), "h1 still bakes");
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(turn_of(&world, "h1").baked_beats, 1);
    assert!(starts(&world, "h2"));
}

/// Proves: a host in its turn when the run is cancelled, which then reports its turn deferred, is
/// stored deferred, shown not reached since the cancel by `Get`, and never offered again.
///
/// Catches: a cancelled run that lists a host deferred for ever.
#[test]
fn a_host_that_defers_after_a_cancel_is_shown_not_reached() {
    let world = scheduled("mt-cancel-defer", table(1, 0), &["h1"], |_| {});
    assert!(starts(&world, "h1"));
    cancel(&world, RUN, "not today").expect("cancelled");
    tell(
        &world,
        "h1",
        report(ORDER, MaintenanceStep::Deferred, "stage: exit 75"),
    );
    assert_eq!(turn_of(&world, "h1").step, Step::Deferred);
    since(&world, "h1", UNIX - 10_000);
    assert_eq!(
        ordered(
            &world,
            "h1",
            report(ORDER, MaintenanceStep::Deferred, "stage: exit 75")
        ),
        None
    );
    let got = get(&world, RUN).expect("the run");
    let host = &got.hosts[0];
    assert_eq!(
        (host.step(), host.since_unix, host.reason.as_str()),
        (MaintenanceHostStep::NotReached, UNIX, "stage: exit 75")
    );
}

/// Proves: a host done and baking in one run and in its turn of another holds one batch place,
/// not two: with a batch of 2 the group's next host starts. While it is in that turn its
/// heartbeats do not count toward the bake of the run it is done with, even healthy ones.
///
/// Catches: one host counted twice against the batch, and a bake proven, or reset, by the
/// heartbeats of a later turn.
#[test]
fn a_host_in_two_runs_holds_one_place_and_bakes_only_out_of_its_turn() {
    let world = baking("mt-two-runs", table(2, 0), 5, &["h1", "h2", "h3"]);
    edit_runs(&world, |runs| {
        let hosts = &mut runs[0].hosts;
        hosts[0].step = Step::Done;
        hosts[1].step = Step::Done;
        hosts[1].baked_beats = 5;
        let mut other = run_of(
            "run-2-1",
            vec![RunHost {
                step: Step::Staging,
                ..waiting("h1", GROUP, &[Kind::Linux])
            }],
        );
        other.take_last_host = true;
        runs.push(other);
    });
    assert!(starts(&world, "h3"), "h1 holds one place of two");
    tell(&world, "h1", beat(LINUX, None));
    tell(&world, "h1", beat(LINUX, None));
    assert_eq!(turn_of(&world, "h1").baked_beats, 0, "h1 is in a turn");
    edit_runs(&world, |runs| runs[1].hosts[0].step = Step::Done);
    tell(&world, "h1", beat(LINUX, None));
    assert_eq!(turn_of(&world, "h1").baked_beats, 1);
    assert_eq!(
        world.ops().maintenance_runs[1].hosts[0].baked_beats,
        1,
        "both bake"
    );
}

/// Proves: a deferred host still in its backoff holds back no host that waits: the next host of
/// the group is started while it backs off.
///
/// Catches: a deferred host that takes a free place, or blocks the order of hosts, before its
/// backoff has passed.
#[test]
fn a_deferred_host_in_its_backoff_holds_back_no_waiting_host() {
    let world = scheduled("mt-backoff-others", table(1, 0), &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", report(ORDER, MaintenanceStep::Deferred, ""));
    assert!(starts(&world, "h2"), "h1 backs off: h2 is next");
    assert_eq!(turn_of(&world, "h1").step, Step::Deferred);
}

/// Proves: a bake count decided on a run that, by the time it is stored, is no longer at the
/// place it was read at (another change put a run before it) writes no count and no audit entry;
/// the next heartbeat counts it.
///
/// Catches: a count written to another run's host.
#[test]
fn a_bake_decided_on_a_run_that_moved_writes_nothing() {
    let world = baking("mt-bake-moved", table(1, 0), 2, &["h1", "h2"]);
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    let audited = world.audit().len();
    let coordinator = world.coordinator();
    coordinator.ops().interleave(|state| {
        state
            .maintenance_runs
            .insert(0, run_of("run-1-1", Vec::new()));
    });
    {
        let fleet = coordinator.lock_fleet();
        coordinator.bake_maintenance(&fleet, &crate::testutil::id("h1"));
    }
    let runs = world.ops().maintenance_runs;
    assert_eq!(runs[0].id, "run-1-1");
    assert_eq!(runs[1].hosts[0].baked_beats, 0);
    assert_eq!(world.audit().len(), audited);
    tell(&world, "h1", at_step(MaintenanceStep::Done));
    assert_eq!(world.ops().maintenance_runs[1].hosts[0].baked_beats, 1);
}
