//! A host rebooting in its turn of a maintenance run that stays silent: the coordinator stores it
//! not back at its group's `return_secs` when it observes the alerts (and not when it lists them),
//! the alert follows from what is stored, the `silent_host` it would raise is spared while the
//! turn is rebooting or not back, and the host's return stores the step its daemon reports. The
//! clock is the coordinator's own, set by each test.

use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceStep;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;

use crate::alerts::{Alert, Kind};
use crate::decide_maintenance::tests::{RUN, at_step, scheduled, starts, table, tell};
use crate::host_groups_support::{UNIX, World};
use crate::maintenance_calls_tests::{GROUP, edit_runs, host_at, run_of, seed_runs};
use crate::maintenance_runs::{Kind as RunKind, Step};
use crate::marks_support::operator;
use crate::testutil::id;

/// The second of the coordinator's clock at which the hosts of a [`World`] are heard.
const HEARD: u64 = 2_000;

/// Every alert of `kind` that an observation of the alerts finds now.
fn observed(world: &World, kind: Kind) -> Vec<Alert> {
    world
        .coordinator()
        .observe_alerts()
        .expect("observed")
        .into_iter()
        .map(|found| found.alert)
        .filter(|alert| alert.subject.kind == kind)
        .collect()
}

/// The stored step of the turn of `host` in the first run.
fn step_of(world: &World, host: &str) -> Step {
    world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .find(|turn| turn.host_id == host)
        .map(|turn| turn.step)
        .expect("the run holds the host")
}

/// Sets the stored step of the turn of `host` in the first run, as another coordinator's heartbeat
/// would.
fn put_step(world: &World, host: &str, step: Step) {
    edit_runs(world, |runs| {
        for turn in runs[0].hosts.iter_mut().filter(|turn| turn.host_id == host) {
            turn.step = step;
        }
    });
}

/// A world whose host `h1`, with `h2` waiting behind it in the group, is stored rebooting in its
/// turn, its last heartbeat at [`HEARD`], the group's maintenance table `table`.
fn rebooting(name: &str, table: proto::MaintenancePolicy) -> World {
    let world = scheduled(name, table, &["h1", "h2"], |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at_step(MaintenanceStep::Rebooting));
    assert_eq!(step_of(&world, "h1"), Step::Rebooting);
    world
}

/// Proves: a Linux host stored rebooting that has been silent for 1199 seconds (the default
/// `return_secs`, 1200, less one) is left rebooting and raises no alert; at 1200 seconds the
/// observation stores its turn not back, as of the unix time now, with one `maintenance_step`
/// entry by the coordinator naming the host, its group and the run, and raises
/// `maintenance_not_back` for it; a second observation writes nothing more.
///
/// Catches: a host marked a second early or late, an alert raised without the step stored (so no
/// other coordinator would see it), and an entry written at every observation.
#[test]
fn a_linux_host_rebooting_is_not_back_at_exactly_its_return_secs_of_silence() {
    let world = rebooting("anb-linux", table(1, 0));
    world.rig.at(HEARD + 1_199);
    let audited = world.audit().len();
    assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
    assert_eq!(step_of(&world, "h1"), Step::Rebooting);
    assert_eq!(world.audit().len(), audited);

    world.rig.at(HEARD + 1_200);
    let found = observed(&world, Kind::MaintenanceNotBack);
    let [alert] = found.as_slice() else {
        panic!("one alert: {found:?}");
    };
    assert_eq!(alert.subject.host, "h1");
    assert_eq!(alert.subject.group, GROUP);
    assert!(
        alert.message.contains(&format!("maintenance run {RUN}, ")),
        "{}",
        alert.message
    );
    assert_eq!(step_of(&world, "h1"), Step::NotBack);
    let turn = &world.ops().maintenance_runs[0].hosts[0];
    assert_eq!((turn.host_id.as_str(), turn.since_unix), ("h1", UNIX));
    let entries = world.audit_after(audited);
    let [entry] = entries.as_slice() else {
        panic!("one entry: {entries:?}");
    };
    assert_eq!(
        (
            entry.by.as_str(),
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.host_group.as_deref(),
            entry.reason.as_deref(),
        ),
        (
            "coordinator",
            "maintenance_step",
            Some("h1"),
            Some(GROUP),
            Some(format!("run {RUN} not_back").as_str()),
        )
    );

    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(
        world.audit_after(audited),
        entries,
        "nothing more is written"
    );
}

/// Proves: a host the fleet knows as a Mac is held to the Mac's default `return_secs`, 3600: not
/// back at 3600 seconds of silence and not at the Linux default of 1200 or at 3599.
///
/// Catches: one default for every platform, which marks a Mac that is still installing its update
/// not back after twenty minutes.
#[test]
fn a_mac_rebooting_is_not_back_at_the_macs_default_not_the_linux_one() {
    let world = rebooting("anb-mac", table(1, 0));
    {
        let mut fleet = world.coordinator().lock_fleet();
        let host = id("h1");
        let mut node = fleet.node(&host).expect("heard").clone();
        node.platform = Platform::DarwinArm64;
        fleet.note_node(host, node, UNIX);
    }
    for silent in [1_200, 3_599] {
        world.rig.at(HEARD + silent);
        assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
        assert_eq!(step_of(&world, "h1"), Step::Rebooting, "at {silent} s");
    }
    world.rig.at(HEARD + 3_600);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(step_of(&world, "h1"), Step::NotBack);
}

/// Proves: a group that names its own `return_secs` (600) has its hosts marked not back at that
/// many seconds of silence, not at the platform's default.
///
/// Catches: a group's `return_secs` that is read for the order and ignored by the alert.
#[test]
fn a_groups_own_return_secs_decides_when_its_host_is_not_back() {
    let own = proto::MaintenancePolicy {
        return_secs: 600,
        ..table(1, 0)
    };
    let world = rebooting("anb-group", own);
    world.rig.at(HEARD + 599);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
    assert_eq!(step_of(&world, "h1"), Step::Rebooting);
    world.rig.at(HEARD + 600);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(step_of(&world, "h1"), Step::NotBack);
}

/// Proves: a host stored rebooting that the coordinator has not heard since it started is judged
/// by its silence since the fleet's start, at the larger of the platforms' defaults because its
/// platform is unknown: not back at 3600 seconds after the start and not at 3599; its turn is
/// audited with no group when the run took it in none.
///
/// Catches: a host never heard that is marked at the first observation (the coordinator has had
/// no time to hear it), one that is never marked, and one marked at the Linux default while it
/// may be a Mac.
#[test]
fn a_rebooting_host_never_heard_is_not_back_after_the_larger_return_secs_since_the_start() {
    let world = World::new("anb-unheard");
    let turn = host_at("ghost", "", Step::Rebooting, UNIX, &[RunKind::Linux]);
    seed_runs(&world, vec![run_of("run-x", vec![turn])]);
    let started = world.coordinator().lock_fleet().started().as_secs();
    let audited = world.audit().len();
    for silent in [1_200, 3_599] {
        world.rig.at(started + silent);
        assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
        assert_eq!(step_of(&world, "ghost"), Step::Rebooting, "at {silent} s");
    }
    world.rig.at(started + 3_600);
    let found = observed(&world, Kind::MaintenanceNotBack);
    let [alert] = found.as_slice() else {
        panic!("one alert: {found:?}");
    };
    assert_eq!(
        (alert.subject.host.as_str(), alert.subject.group.as_str()),
        ("ghost", "")
    );
    assert!(
        alert.message.contains("in no host group"),
        "{}",
        alert.message
    );
    let entries = world.audit_after(audited);
    let [entry] = entries.as_slice() else {
        panic!("one entry: {entries:?}");
    };
    assert_eq!(
        (entry.node_id.as_deref(), entry.host_group.as_deref()),
        (Some("ghost"), None)
    );
    assert_eq!(entry.reason.as_deref(), Some("run run-x not_back"));
}

/// Proves: `Alerts.List` alone stores nothing: past `return_secs` it answers no
/// `maintenance_not_back`, leaves the turn rebooting and writes no entry; the next observation
/// stores it.
///
/// Catches: a read call that writes the ops state, which a viewer of the alerts could then
/// trigger.
#[test]
fn listing_the_alerts_does_not_store_a_host_not_back() {
    let world = rebooting("anb-list", table(1, 0));
    world.rig.at(HEARD + 2_000);
    let audited = world.audit().len();
    let listed = world
        .coordinator()
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("listed");
    assert!(
        listed
            .alerts
            .iter()
            .all(|alert| alert.kind() != proto::AlertKind::MaintenanceNotBack),
        "{listed:?}"
    );
    assert_eq!(step_of(&world, "h1"), Step::Rebooting);
    assert_eq!(world.audit().len(), audited);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(step_of(&world, "h1"), Step::NotBack);
}

/// Proves: a host stored rebooting raises no `silent_host` from the silent threshold (300 s) up to
/// its `return_secs`, nor once it is stored not back; once its turn ends (stored done) while it
/// stays silent, `silent_host` is raised for it again, with the silence in its message.
///
/// Catches: a silent-host alert for the quiet of a reboot, which the not-back alert answers, and
/// a host whose turn ended that is never reported silent again.
#[test]
fn a_host_rebooting_in_its_turn_is_not_silent_until_the_turn_ends() {
    let world = rebooting("anb-silent", table(1, 0));
    let silent = |world: &World| observed(world, Kind::SilentHost);
    let silent_h1 = |world: &World| {
        silent(world)
            .into_iter()
            .filter(|alert| alert.subject.host == "h1")
            .collect::<Vec<_>>()
    };
    for seconds in [300, 1_199] {
        world.rig.at(HEARD + seconds);
        assert_eq!(silent_h1(&world), Vec::new(), "at {seconds} s");
    }
    world.rig.at(HEARD + 1_200);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(silent_h1(&world), Vec::new(), "stored not back");

    put_step(&world, "h1", Step::Done);
    let again = silent_h1(&world);
    let [alert] = again.as_slice() else {
        panic!("one alert: {again:?}");
    };
    assert_eq!(
        alert.message,
        "h1 has not heartbeated for 1200 s (the threshold is 300 s)"
    );
}

/// Proves: a host stored not back that heartbeats again, reporting its turn at checking, has its
/// step stored as its daemon reports it, and the next observation no longer raises
/// `maintenance_not_back` for it.
///
/// Catches: a not-back alert that outlives the host's return, and a return that does not resume
/// the stored turn.
#[test]
fn a_host_not_back_that_reports_again_has_its_step_stored_and_the_alert_clears() {
    let world = rebooting("anb-return", table(1, 0));
    world.rig.at(HEARD + 1_200);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(step_of(&world, "h1"), Step::NotBack);

    world.rig.at(HEARD + 1_300);
    tell(&world, "h1", at_step(MaintenanceStep::Checking));
    assert_eq!(step_of(&world, "h1"), Step::Checking);
    assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
    assert_eq!(step_of(&world, "h1"), Step::Checking, "not marked again");
}

/// Proves: a turn stored halted raises `maintenance_halted` over a coordinator, and
/// `Alerts.List` answers it critical, about the host and its group; once the stored step is
/// another it is gone.
///
/// Catches: a halted alert that `Alerts.List` does not carry with the host and the group, and one
/// that stays after the turn moved on.
#[test]
fn a_halted_turn_is_listed_with_its_host_and_group_until_its_step_moves_on() {
    let world = rebooting("anb-halted", table(1, 0));
    edit_runs(&world, |runs| {
        for turn in runs[0].hosts.iter_mut().filter(|turn| turn.host_id == "h1") {
            turn.step = Step::Halted;
            turn.reason = "apply linux: exit 1".to_owned();
        }
    });
    let list = |world: &World| {
        world
            .coordinator()
            .list_alerts(&operator(), proto::ListAlertsRequest {})
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind() == proto::AlertKind::MaintenanceHalted)
            .collect::<Vec<_>>()
    };
    let listed = list(&world);
    let [alert] = listed.as_slice() else {
        panic!("one alert: {listed:?}");
    };
    assert_eq!(alert.severity(), proto::AlertSeverity::Critical);
    assert_eq!(
        (alert.host_id.as_str(), alert.host_group.as_str()),
        ("h1", GROUP)
    );
    assert!(
        alert.message.contains("apply linux: exit 1"),
        "{}",
        alert.message
    );

    put_step(&world, "h1", Step::Checking);
    assert_eq!(list(&world), Vec::new());
}

/// Proves: a not-back mark the store refuses leaves the turn rebooting and raises nothing; once
/// the store takes writes again the next observation stores it and raises the alert.
///
/// Catches: an alert raised for a step that was not stored, and a refusal that is remembered and
/// never tried again.
#[test]
fn a_not_back_mark_the_store_refuses_is_made_again_at_the_next_observation() {
    let world = rebooting("anb-store", table(1, 0));
    world.rig.at(HEARD + 1_200);
    world.fake.fail();
    assert_eq!(observed(&world, Kind::MaintenanceNotBack), Vec::new());
    assert_eq!(step_of(&world, "h1"), Step::Rebooting);
    world.fake.heal();
    assert_eq!(observed(&world, Kind::MaintenanceNotBack).len(), 1);
    assert_eq!(step_of(&world, "h1"), Step::NotBack);
}

/// Proves: a turn that another coordinator moved on (its host reported checking) between the read
/// of the silent hosts and the change is left as it is: no step is overwritten and no entry
/// written.
///
/// Catches: a stale not-back mark written over a host that came back in the meantime.
#[test]
fn a_turn_that_moved_on_meanwhile_is_not_marked_not_back() {
    let world = rebooting("anb-moved", table(1, 0));
    world.rig.at(HEARD + 1_200);
    let audited = world.audit().len();
    world.coordinator().ops().interleave(|state| {
        for turn in &mut state.maintenance_runs[0].hosts {
            if turn.host_id == "h1" {
                turn.step = Step::Checking;
            }
        }
    });
    world.coordinator().note_not_back();
    assert_eq!(step_of(&world, "h1"), Step::Checking);
    assert_eq!(world.audit().len(), audited);
}
