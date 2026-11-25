//! A run of `probe` ([`crate::maintenance_service`], [`crate::decide_maintenance`]): it is named
//! alone, for hosts named; it holds each host named whose kept report lists `probe` on demand, and
//! no other; the Update button and a report that lists `probe` as due never take it; and a host's
//! turn of it starts only while its report lists it. The builders are in
//! `maintenance_calls_tests.rs` and `decide_maintenance_tests.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_wire::wire::Heartbeat;

use crate::decide_maintenance::tests::{ORDER, RUN, maintain_id, table, tell};
use crate::host_groups_support::{REASON, SELF, World};
use crate::maintenance::{FEATURE, text_of};
use crate::maintenance_calls_tests::{
    FIRST, GROUP, LINUX, MACOS, NONE, group_request, hear, hosts_request, kept, member, run_of,
    schedule, skipped, waiting,
};
use crate::maintenance_runs::{Kind, Run, Skip};
use crate::marks_support::{NEW_DAEMON, beat_of, runs};
use crate::orders::Order;

/// A heartbeat of a host that announces `maintenance-v1` and reports `due`, and `probe` on demand
/// when `on_demand`.
fn probe_beat(due: &[MaintenanceKind], on_demand: bool) -> Heartbeat {
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
        mac: crate::maintenance_calls_tests::mac_of(due),
        on_demand: if on_demand {
            vec![MaintenanceKind::Probe as i32]
        } else {
            Vec::new()
        },
        ..agent::MaintenanceReport::default()
    };
    beat.maintenance = Some(text_of(&report));
    beat
}

/// A world of the group [`GROUP`] (batch 1, `min_serving` 0): `m1` a Mac whose daemon lists
/// `probe` on demand with nothing due, `m2` the same with a macOS update due, `l1` a host whose
/// daemon lists nothing on demand with a Linux update due, and `o1` a host whose daemon does not
/// announce `maintenance-v1`.
fn world(name: &str) -> World {
    let world = World::new(name);
    let policy = proto::RolloutPolicy {
        maintenance: Some(table(1, 0)),
        ..proto::RolloutPolicy::default()
    };
    world
        .put_with(GROUP, &[SELF], Some(policy), None)
        .expect("put");
    for host in ["m1", "m2", "l1", "o1"] {
        member(&world, GROUP, host);
    }
    tell(&world, "m1", probe_beat(NONE, true));
    tell(&world, "m2", probe_beat(MACOS, true));
    tell(&world, "l1", probe_beat(LINUX, false));
    hear(&world, "o1", false, NONE);
    world
}

/// A `Schedule` of `probe` alone for the hosts `ids`, confirmed.
fn probe_of(ids: &[&str]) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        kinds: vec![MaintenanceKind::Probe as i32],
        ..hosts_request(ids)
    }
}

/// Proves: a run of `probe` for hosts named holds each host whose report lists `probe` on demand,
/// waiting with the kind `probe` alone though something else is due there; a host whose daemon
/// lists nothing on demand is not in the run, as one with nothing due is not; a host whose daemon
/// does not announce `maintenance-v1` is left out with that reason; the run's kinds are `probe`.
///
/// Catches: a probe ordered to a daemon that has none (its hook would halt the host drained with
/// `unsupported_kind`), or a probe run that also applies what is due.
#[test]
fn a_probe_holds_each_host_named_that_lists_it_and_no_other() {
    let world = world("mp-holds");
    let response = schedule(&world, probe_of(&["m1", "m2", "l1", "o1"])).expect("scheduled");
    let rows = vec![
        waiting("m1", GROUP, &[Kind::Probe]),
        waiting("m2", GROUP, &[Kind::Probe]),
        skipped("o1", GROUP, &[Kind::Probe], Skip::NoMaintenanceV1),
    ];
    assert_eq!(
        world.ops().maintenance_runs,
        vec![Run {
            kinds: vec![Kind::Probe],
            host_ids: ["m1", "m2", "l1", "o1"].map(str::to_owned).to_vec(),
            host_group: String::new(),
            ..run_of(FIRST, rows)
        }]
    );
    assert_eq!(
        response.run.expect("the run").kinds,
        [MaintenanceKind::Probe as i32]
    );
}

/// Proves: a run of `probe` whose hosts named all list nothing on demand is refused
/// `nothing_due`, and changes nothing.
///
/// Catches: a stored run that can never start a host.
#[test]
fn a_probe_of_hosts_that_list_none_is_nothing_due() {
    let world = world("mp-none");
    let before = kept(&world);
    assert_eq!(
        schedule(&world, probe_of(&["l1"])).expect_err("refused"),
        Refusal::NothingDue
    );
    assert_eq!(kept(&world), before);
}

/// Proves: `probe` named with another kind, or for a host group, is refused `invalid_request`,
/// and changes nothing.
///
/// Catches: a probe ordered beside an update (the run would drain, install and restart, then
/// probe), or a probe of a whole group by mistake.
#[test]
fn a_probe_is_named_alone_for_hosts() {
    let world = world("mp-alone");
    let before = kept(&world);
    let mut with_macos = probe_of(&["m2"]);
    with_macos.kinds.push(MaintenanceKind::Macos as i32);
    let of_group = proto::ScheduleMaintenanceRequest {
        kinds: vec![MaintenanceKind::Probe as i32],
        take_last_host: true,
        ..group_request(GROUP)
    };
    for request in [with_macos, of_group] {
        assert_eq!(
            schedule(&world, request).expect_err("refused"),
            Refusal::InvalidRequest
        );
    }
    assert_eq!(kept(&world), before);
}

/// Proves: the Update button (no kinds) never takes `probe`: a host that lists it on demand and
/// has nothing due is not in the run, and one with a macOS update due waits for that alone; a
/// report that lists a `probe` item as due does not make it due.
///
/// Catches: every Mac of a group drained for a probe nobody asked for at each Update.
#[test]
fn the_update_button_never_takes_a_probe() {
    let world = world("mp-update");
    tell(&world, "m1", probe_beat(&[MaintenanceKind::Probe], true));
    let update = proto::ScheduleMaintenanceRequest {
        confirm: false,
        reason: REASON.to_owned(),
        ..hosts_request(&["m1", "m2"])
    };
    let asked = schedule(&world, update).expect("asked");
    assert_eq!(
        asked.hosts,
        vec![crate::maintenance_service::host_proto(&waiting(
            "m2",
            GROUP,
            &[Kind::Macos]
        ))]
    );
}

/// The kinds of the `maintain` `order`, if it is one.
fn kinds_of(order: Option<&Order>) -> Option<Vec<MaintenanceKind>> {
    match order? {
        Order::Maintain { maintain, .. } => Some(maintain.kinds.clone()),
        _ => None,
    }
}

/// Proves: a host's turn of `probe` starts at a heartbeat whose report lists `probe` on demand,
/// with a `maintain` of the kind `probe` alone; at a heartbeat whose report no longer lists it
/// (its daemon replaced by one without it) it is not started, and waits.
///
/// Catches: a daemon sent a kind it does not know, which it refuses, leaving the host's turn
/// stored as staging while nothing runs.
#[test]
fn a_probe_turn_starts_only_while_the_report_lists_it() {
    let world = world("mp-start");
    schedule(&world, probe_of(&["m1"])).expect("scheduled");
    let held = tell(&world, "m1", probe_beat(NONE, false));
    assert_eq!(maintain_id(held.as_ref()), None, "not started");
    assert_eq!(
        world.ops().maintenance_runs[0].hosts[0].step,
        crate::maintenance_runs::Step::Waiting
    );
    let started = tell(&world, "m1", probe_beat(NONE, true));
    assert_eq!(maintain_id(started.as_ref()), Some(ORDER));
    assert_eq!(
        kinds_of(started.as_ref()),
        Some(vec![MaintenanceKind::Probe])
    );
    assert_eq!(world.ops().maintenance_runs[0].id, RUN);
}
