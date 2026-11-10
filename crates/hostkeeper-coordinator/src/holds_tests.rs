//! The operator's holds: a held pair is told nothing before every other rule of the gate, takes
//! no update slot, is never a canary and is left out of the bake, the convergence and the
//! rollback count; a role hold follows the host's role at the time of the decision; holds
//! survive a restart; the calls (`Services.Hold`, `Unhold`, `List`) refuse what they must; and
//! the ops file keeps the shape the release before reads until a hold is written. Every time is
//! the rig's clock.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{
    Class, Health, Heartbeat, Outcome, Phase, ServiceBeat, ServiceReply, UpdateResult,
};

use crate::converge::State;
use crate::decide::{Gate, Why};
use crate::fleet::{Fleet, Marks, QUIET_START};
use crate::holds::{HoldMark, HoldScope};
use crate::marks_support::{
    DB, INSTALLER, NEW_DAEMON, WEB, ask, ask_typed, beat_of, digest, fleet_target, force_request,
    hold, operator, restart, rig, runs, scope_fleet, scope_host, scope_role, services_with, set,
    unhold, update, who,
};
use crate::ops::{OpsState, ServiceOps};
use crate::orders::Order;
use crate::reconcile::{Change, plan};
use crate::testutil::{
    Log, OPERATOR, Rig, beat, converge_service, desired, id, ops_of, role, secs, version,
};

/// A daemon's heartbeat that lists `web` running `running`, healthy.
fn web_at(running: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, running)])
}

/// A daemon's heartbeat that lists `web` as `one` says.
fn web_with(one: ServiceBeat) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![one])
}

/// What a host reports of `web` when it runs `running` and its update to `target` failed after
/// the commit, as the release's fault.
fn failed_release(running: &str, target: &str) -> ServiceBeat {
    let mut one = runs(WEB, running);
    let mut last = UpdateResult::new(version(target), Outcome::Failed);
    last.phase = Some(Phase::AfterCommit);
    last.class = Some(Class::Release);
    one.last_update = Some(last);
    one
}

/// Why `host` is told nothing for `service` in the status, and whether the status flags it as
/// held on a version marked bad.
fn standing(rig: &Rig, service: &str, host: &str) -> (Option<Why>, bool) {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    let one = status.per_service[service]
        .hosts
        .iter()
        .find(|h| h.host.node_id == host)
        .expect("the host is in the status");
    (one.hold, one.held_on_bad_version)
}

/// The desired version of `service` now.
fn wanted(rig: &Rig, service: &str) -> Version {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.services[service].version.clone()
}

/// A hold of `web` on the host `host`, as the ops file keeps it.
fn hold_mark(host: &str) -> HoldMark {
    HoldMark {
        service: WEB.to_owned(),
        scope: HoldScope::Host(host.to_owned()),
        reason: "investigating".to_owned(),
        by: "operator".to_owned(),
        at_unix: 1_800_000_000,
    }
}

/// A fleet of `web` hosts, all daemons that heartbeated at 2000, with an operator's hold on
/// each host of `held`.
fn farm(hosts: &[(&str, Heartbeat)], held: &[&str]) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for (name, b) in hosts {
        fleet.record_in(WEB, id(name), role("daemon"), b.clone(), secs(2_000));
    }
    fleet.set_marks(Marks {
        holds: held.iter().copied().map(hold_mark).collect(),
        ..Marks::default()
    });
    fleet
}

/// A heartbeat of a host at 1.0 whose update to 2.0 failed after the commit.
fn failed_beat() -> Heartbeat {
    let mut b = beat("1.0");
    let mut last = UpdateResult::new(version("2.0"), Outcome::Failed);
    last.phase = Some(Phase::AfterCommit);
    last.class = Some(Class::Release);
    b.last_update = Some(last);
    b
}

// ---- the gate ---------------------------------------------------------------------------

/// A held host is told nothing however far behind it is, over the JSON route and the typed one,
/// before the slot rule (the status says `held`, not `no_slot`, while another host holds the
/// slot); an automatic rollback sets no lower version on it; and a force that does not include
/// held pairs skips it, sends no force order to it, and still orders the others. Unholding it
/// orders it again.
///
/// Catches: the held check placed after another rule or missed for one of the verbs (update,
/// rollback to a lower version, force), so that a held host is ordered all the same.
#[tokio::test]
async fn a_held_pair_is_never_ordered() {
    let rig = rig("holds-never-ordered", "rollback = \"automatic\"");
    set(&rig, WEB, "1.0", 1).await;
    for host in ["d1", "d2", "d3"] {
        assert_eq!(
            ask(&rig, host, "daemon", web_at("1.0")),
            ServiceReply::Nothing
        );
    }
    assert_eq!(
        rig.coordinator.ops().marks(WEB).last_good,
        Some(version("1.0")),
        "the farm converged on 1.0, so a rollback has somewhere to go"
    );

    hold(&rig, WEB, scope_host("d1")).expect("held");
    set(&rig, WEB, "2.0", 2).await;
    for _ in 0..3 {
        assert_eq!(
            ask(&rig, "d1", "daemon", web_at("1.0")),
            ServiceReply::Nothing
        );
        let (reply, order) = ask_typed(&rig, "d1", "daemon", web_at("1.0"));
        assert_eq!(reply, ServiceReply::Nothing);
        assert_eq!(order, None);
    }
    assert_eq!(
        standing(&rig, WEB, "d1"),
        (Some(Why::Gate(Gate::Held)), false)
    );

    // Another host takes the one live update; the held host still reads held, not no_slot.
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "the held host takes no slot"
    );
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        standing(&rig, WEB, "d1").0,
        Some(Why::Gate(Gate::Held)),
        "held is checked before the slot rule"
    );

    // The release fails on the two hosts that count: 2.0 is marked bad and the service goes back.
    assert_eq!(
        ask(&rig, "d2", "daemon", web_with(failed_release("1.0", "2.0"))),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d3", "daemon", web_with(failed_release("1.0", "2.0"))),
        ServiceReply::Nothing
    );
    assert_eq!(wanted(&rig, WEB), version("1.0"), "rolled back");
    assert_eq!(rig.coordinator.ops().marks(WEB).bad, vec![version("2.0")]);

    // The held host runs 2.0, which the rollback left: it is not ordered to 1.0.
    for _ in 0..2 {
        assert_eq!(
            ask(&rig, "d1", "daemon", web_at("2.0")),
            ServiceReply::Nothing
        );
        let (reply, order) = ask_typed(&rig, "d1", "daemon", web_at("2.0"));
        assert_eq!(reply, ServiceReply::Nothing);
        assert_eq!(order, None);
    }
    assert_eq!(
        standing(&rig, WEB, "d1"),
        (Some(Why::Gate(Gate::Held)), true)
    );

    // A force without include_held skips the held host and orders the others.
    set(&rig, WEB, "3.0", 3).await;
    let response = rig
        .coordinator
        .force(
            &operator(),
            force_request(WEB, "3.0", fleet_target(WEB), false),
        )
        .expect("forced");
    assert_eq!(response.held, vec!["d1".to_owned()]);
    assert_eq!(response.targeted, 2);
    let (reply, order) = ask_typed(&rig, "d1", "daemon", web_at("2.0"));
    assert_eq!(reply, ServiceReply::Nothing);
    assert_eq!(order, None, "no force order for the held host");
    let (reply, order) = ask_typed(&rig, "d2", "daemon", web_at("1.0"));
    assert_eq!(reply, ServiceReply::Nothing);
    assert!(
        matches!(&order, Some(Order::Force { service, version: forced, .. })
            if service == WEB && *forced == version("3.0")),
        "{order:?}"
    );

    // Released, the same host is ordered the desired version.
    unhold(&rig, WEB, scope_host("d1")).expect("released");
    assert_eq!(ask(&rig, "d1", "daemon", web_at("2.0")), update(WEB, "3.0"));
}

/// A held host that does not run the service (it reports it `absent`) is not told to install it,
/// and an unheld one is.
///
/// Catches: the held check placed after the choice of an install, or skipped for the install of
/// a service that never ran.
#[tokio::test]
async fn a_held_host_that_reports_the_service_absent_is_not_installed() {
    let rig = rig("holds-absent", "");
    set(&rig, WEB, "1.0", 1).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    let mut missing = runs(WEB, "0.9");
    missing.health = Health::Absent;
    let installer = |one: ServiceBeat| beat_of(INSTALLER, vec![one]);
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", installer(missing.clone())),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(standing(&rig, WEB, "d1").0, Some(Why::Gate(Gate::Held)));
    let (reply, order) = ask_typed(&rig, "d2", "daemon", installer(missing));
    assert_eq!(reply, ServiceReply::Nothing);
    assert_eq!(
        order,
        Some(crate::assign_support::install("1.0", 1)),
        "an absent host that no one holds is installed"
    );
}

/// A hold covers one service only: the host held for `web` is ordered `db`.
///
/// Catches: a hold that matches by host alone and freezes every service of it.
#[tokio::test]
async fn a_hold_is_per_service() {
    let rig = rig("holds-per-service", "");
    set(&rig, WEB, "2.0", 2).await;
    set(&rig, DB, "2.0", 2).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    let both = beat_of(NEW_DAEMON, vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    assert_eq!(ask(&rig, "d1", "daemon", both), update(DB, "2.0"));
    assert_eq!(standing(&rig, WEB, "d1").0, Some(Why::Gate(Gate::Held)));
    assert_eq!(standing(&rig, DB, "d1").0, None);
}

/// A hold of the whole fleet freezes every host of every role until it is released.
///
/// Catches: a fleet scope that covers no host, or an unhold of it that leaves a host held.
#[tokio::test]
async fn a_fleet_hold_covers_every_host_until_it_is_released() {
    let rig = rig("holds-fleet", "");
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, WEB, scope_fleet()).expect("held");
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "s1", "storage", web_at("1.0")),
        ServiceReply::Nothing
    );
    for host in ["d1", "s1"] {
        assert_eq!(
            standing(&rig, WEB, host).0,
            Some(Why::Gate(Gate::Held)),
            "{host}"
        );
    }
    unhold(&rig, WEB, scope_fleet()).expect("released");
    assert_eq!(ask(&rig, "d1", "daemon", web_at("1.0")), update(WEB, "2.0"));
}

/// A hold on a role covers a host that joins the role after the hold was set, holds a host whose
/// certificate role changes to it, and releases a host whose role changes away from it.
///
/// Catches: a role hold resolved once, when it was set, instead of by the host's role at each
/// decision.
#[tokio::test]
async fn a_hold_on_a_role_covers_a_host_that_joins_later() {
    let rig = rig("holds-role", "");
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, WEB, scope_role("storage")).expect("held");

    assert_eq!(
        ask(&rig, "s1", "storage", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        standing(&rig, WEB, "s1").0,
        Some(Why::Gate(Gate::Held)),
        "a storage host that joined after the hold is held"
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "a daemon is not covered"
    );
    // The daemon's update ends, and its slot with it.
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("2.0")),
        ServiceReply::Nothing
    );

    let answer = rig
        .coordinator
        .heartbeat_native(who("d1", "storage"), web_at("1.0"))
        .expect("answered");
    assert_eq!(answer.reply, ServiceReply::Nothing);
    assert_eq!(
        standing(&rig, WEB, "d1").0,
        Some(Why::Gate(Gate::Held)),
        "a daemon whose certificate role is now storage is held from then"
    );

    assert_eq!(
        ask(&rig, "s1", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "a storage host that heartbeats as a daemon is released and ordered"
    );
    assert_eq!(standing(&rig, WEB, "s1").0, None);
}

/// A hold is in the ops file and the status after a restart, and the restarted coordinator
/// decides the same: the held host is told nothing, and its reason is held, not the quiet start.
///
/// Catches: holds kept in memory only, or a hold that the quiet start hides.
#[tokio::test]
async fn a_hold_survives_a_coordinator_restart() {
    let mut rig = rig("holds-restart", "");
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );

    restart(&mut rig, "");
    rig.at(2_010);
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        standing(&rig, WEB, "d1").0,
        Some(Why::Gate(Gate::Held)),
        "held is checked before the quiet start"
    );
    assert_eq!(standing(&rig, WEB, "d2").0, Some(Why::Gate(Gate::Quiet)));

    rig.at(2_040);
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "past the quiet start the unheld host is ordered"
    );

    let holds = ops_of(&rig.dir).state().holds;
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].service, WEB);
    assert_eq!(holds[0].scope, HoldScope::Host("d1".to_owned()));
    assert_eq!(holds[0].by, "operator");
    let shown = rig.coordinator.status(Some(OPERATOR)).expect("status").ops;
    assert_eq!(shown.holds, holds);
}

/// A held pair is never a canary, never holds back a bake, is left out of the convergence and
/// out of the hosts a rollback is counted against, and a held host's failure does not count.
///
/// Catches: a held host with the lowest node id chosen as the canary, a held host that has not
/// baked blocking the others, a held host that is behind or failed keeping the farm from
/// converging or blocking it, and a rollback counted over held hosts.
#[tokio::test]
async fn a_held_pair_is_left_out_of_canary_bake_and_converged() {
    let rig = rig(
        "holds-canary-bake",
        "canary = { count = 1 }\nbake_beats = 2",
    );
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "the held host has the lowest node id and is not the canary"
    );
    assert_eq!(
        ask(&rig, "d3", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );

    // The canary bakes; the held host runs the desired version and has not baked.
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d3", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "a held host that has not baked does not hold the others back"
    );
    assert_eq!(standing(&rig, WEB, "d1").0, Some(Why::Gate(Gate::Held)));

    // The convergence leaves the held host out: behind, failed or updating, it blocks nothing.
    let services = services_with("");
    let def = services.get(WEB).expect("web");
    let want = desired("2.0", 0, 0);
    let mut running = beat("1.0");
    running.updating = Some(version("2.0"));
    for (name, behind, expected) in [
        ("behind", beat("1.0"), State::InProgress),
        ("failed", failed_beat(), State::Blocked),
        ("updating", running, State::InProgress),
    ] {
        let hosts = [("d1", behind), ("d2", beat("2.0"))];
        let open = farm(&hosts, &[]);
        assert_eq!(
            converge_service(def, Some(&want), &open, secs(2_000)).state,
            expected,
            "{name}, not held"
        );
        let held = farm(&hosts, &["d1"]);
        let converged = converge_service(def, Some(&want), &held, secs(2_000));
        assert_eq!(converged.state, State::Converged, "{name}, held");
        assert!(
            converged
                .roles
                .iter()
                .all(|c| c.failed == 0 && c.updating == 0),
            "{name}: {:?}",
            converged.roles
        );
    }

    // The rollback counts only the hosts that are not held: 50% of the two that count is one.
    let by_percent = services_with(
        "rollback = \"automatic\"\nrollback_after_failed = { percent = 50, min_hosts = 1 }",
    );
    let def = by_percent.get(WEB).expect("web");
    let marks = ServiceOps {
        last_good: Some(version("1.0")),
        ..ServiceOps::default()
    };
    let hosts = [
        ("d1", failed_beat()),
        ("d2", beat("1.0")),
        ("d3", beat("1.0")),
        ("d4", beat("1.0")),
    ];
    let rolled_back = vec![
        Change::MarkBad(version("2.0")),
        Change::RollBackTo(version("1.0")),
    ];
    let state = OpsState::default();
    assert_eq!(
        plan(def, &want, &marks, &farm(&hosts, &[]), &state, secs(2_000)),
        vec![],
        "one failure of four is under 50%"
    );
    assert_eq!(
        plan(
            def,
            &want,
            &marks,
            &farm(&hosts, &["d2", "d3"]),
            &state,
            secs(2_000)
        ),
        rolled_back,
        "one failure of the two hosts that count is enough"
    );

    // A held host's failure is not counted: one failure of the three that count is under the
    // default of two.
    let by_default = services_with("rollback = \"automatic\"");
    let def = by_default.get(WEB).expect("web");
    let hosts = [
        ("d1", failed_beat()),
        ("d2", failed_beat()),
        ("d3", beat("1.0")),
        ("d4", beat("1.0")),
    ];
    assert_eq!(
        plan(def, &want, &marks, &farm(&hosts, &[]), &state, secs(2_000)),
        rolled_back,
        "two failures roll the service back"
    );
    assert_eq!(
        plan(
            def,
            &want,
            &marks,
            &farm(&hosts, &["d2"]),
            &state,
            secs(2_000)
        ),
        vec![],
        "the failure of a held host is not counted"
    );
}

// ---- the slot ---------------------------------------------------------------------------

/// The line of `web`'s definition that rolls it in one stage, so that the daemon role's
/// `max_updating` alone decides who holds the slot.
const ONE_STAGE: &str = "stages = [100]";

/// Sets `web` to `version` as CI does, with the desired state's `max_updating` of the daemon
/// role at 1: one daemon of the service holds an update slot at a time.
async fn set_one_daemon_at_a_time(rig: &Rig, version: &str, n: u8) {
    let body = format!(
        r#"{{"service":"{WEB}","version":"{version}","release_digest":"{}","max_updating":{{"daemon":1}}}}"#,
        digest(n)
    );
    rig.set(&body).await;
}

/// A held host that is unhealthy after a failed update would hold the one live update if it were
/// not held; once held it does not, and another host is ordered.
///
/// Catches: a held pair counted towards the live update when it only failed.
#[tokio::test]
async fn a_held_pair_takes_no_slot() {
    let rig = rig("holds-no-slot", ONE_STAGE);
    set_one_daemon_at_a_time(&rig, "2.0", 2).await;
    let mut stuck = runs(WEB, "1.0");
    stuck.health = Health::Unhealthy;
    stuck.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Failed));
    assert_eq!(
        ask(&rig, "d1", "daemon", web_with(stuck.clone())),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing,
        "the failed host holds the slot while no one holds it"
    );
    assert_eq!(standing(&rig, WEB, "d2").0, Some(Why::Gate(Gate::NoSlot)));

    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        ask(&rig, "d1", "daemon", web_with(stuck)),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "a held host that failed takes no slot"
    );
}

/// A held host whose beat says it is updating (an update that ran when the hold was set) still
/// holds the one live update, until a beat stops saying so.
///
/// Catches: a hold that frees the slot of an update still running, so that a second update
/// starts beside it.
#[tokio::test]
async fn a_held_pair_that_is_updating_holds_the_slot_until_it_stops() {
    let rig = rig("holds-updating", ONE_STAGE);
    set_one_daemon_at_a_time(&rig, "2.0", 2).await;
    let mut running = runs(WEB, "1.0");
    running.updating = Some(version("2.0"));
    assert_eq!(
        ask(&rig, "d1", "daemon", web_with(running.clone())),
        ServiceReply::Nothing
    );
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(standing(&rig, WEB, "d2").0, Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(
        ask(&rig, "d1", "daemon", web_with(running)),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing,
        "still updating"
    );

    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "the slot is free once the held host stops updating"
    );
}

/// A host held after it was ordered and before its beat says it is updating still holds the one
/// live update while the order is outstanding, so no second update starts beside the one it
/// may be starting; once its beat shows the order done, the slot is free.
///
/// Catches: a hold that frees the slot of an order just given, so two updates are live at once.
#[tokio::test]
async fn a_held_pair_with_an_order_outstanding_holds_the_slot_until_the_order_ends() {
    let rig = rig("holds-ordered", ONE_STAGE);
    set_one_daemon_at_a_time(&rig, "2.0", 2).await;
    assert_eq!(ask(&rig, "d1", "daemon", web_at("1.0")), update(WEB, "2.0"));
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(standing(&rig, WEB, "d2").0, Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing,
        "held: not told the order again"
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing,
        "the order is still outstanding"
    );
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        update(WEB, "2.0"),
        "the slot is free once the held host's order ended"
    );
}

/// Unholding releases the pair to the desired version of its next heartbeat, not to a version CI
/// set while it was held.
///
/// Catches: an unhold that orders the version that was desired when the hold was set, or none.
#[tokio::test]
async fn unhold_releases_the_pair_to_the_then_desired_version() {
    let rig = rig("holds-unhold", "");
    hold(&rig, WEB, scope_host("d1")).expect("held");
    set(&rig, WEB, "2.0", 2).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "3.0", 3).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );

    let released = unhold(&rig, WEB, scope_host("d1")).expect("released");
    assert_eq!(released.entry.expect("an entry").action, "unhold");
    assert_eq!(ask(&rig, "d1", "daemon", web_at("1.0")), update(WEB, "3.0"));
    assert!(rig.coordinator.ops().read(|state| state.holds.is_empty()));
}

// ---- a version marked bad ---------------------------------------------------------------

/// A held host that runs a version an operator marked bad is not ordered off it; one error line
/// with `alert = "held_on_bad_version"` is logged per host, service and version however often it
/// heartbeats; the status flags the held host and no other; an unheld host is ordered off the
/// bad version.
///
/// Catches: a bad mark that reaches a held host, an alert logged at every observation of the
/// alerts or never, a status flag raised for hosts that are not held.
#[tokio::test]
async fn automatic_rollback_and_mark_bad_skip_a_held_pair_and_alert() {
    let rig = rig("holds-bad-version", "");
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    for bad in ["2.0", "1.5"] {
        rig.coordinator
            .mark_bad(
                &operator(),
                proto::MarkBadRequest {
                    service: WEB.to_owned(),
                    version: bad.to_owned(),
                    reason: "bad build".to_owned(),
                },
            )
            .expect("marked");
    }
    set(&rig, WEB, "1.0", 1).await;

    let log = Log::default();
    let _guard = log.install();
    let alerts = || {
        log.text()
            .lines()
            .filter(|l| l.contains("held_on_bad_version"))
            .map(str::to_owned)
            .collect::<Vec<String>>()
    };
    for _ in 0..2 {
        assert_eq!(
            ask(&rig, "d1", "daemon", web_at("2.0")),
            ServiceReply::Nothing
        );
        rig.coordinator.observe_alerts().expect("observed");
    }
    let lines = alerts();
    assert_eq!(lines.len(), 1, "{}", log.text());
    assert!(
        lines[0].contains("ERROR")
            && lines[0].contains("node_id=d1")
            && lines[0].contains("version=2.0"),
        "{}",
        lines[0]
    );

    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("2.0")),
        update(WEB, "1.0"),
        "a host that is not held is ordered off the bad version"
    );
    assert_eq!(
        standing(&rig, WEB, "d1"),
        (Some(Why::Gate(Gate::Held)), true)
    );
    assert_eq!(standing(&rig, WEB, "d2"), (None, false));
    let shown = serde_json::to_value(rig.coordinator.status(Some(OPERATOR)).expect("status"))
        .expect("JSON");
    let hosts = &shown["per_service"][WEB]["hosts"];
    assert_eq!(hosts[0]["node_id"], "d1");
    assert_eq!(hosts[0]["held_on_bad_version"].as_bool(), Some(true));
    assert_eq!(hosts[1]["node_id"], "d2");
    assert!(hosts[1].get("held_on_bad_version").is_none());

    // Another bad version on the same host is another alert.
    assert_eq!(
        ask(&rig, "d1", "daemon", web_at("1.5")),
        ServiceReply::Nothing
    );
    rig.coordinator.observe_alerts().expect("observed");
    assert_eq!(alerts().len(), 2, "{}", log.text());

    // Released, the host is ordered off it.
    assert_eq!(
        ask(&rig, "d2", "daemon", web_at("1.0")),
        ServiceReply::Nothing
    );
    unhold(&rig, WEB, scope_host("d1")).expect("released");
    assert_eq!(ask(&rig, "d1", "daemon", web_at("1.5")), update(WEB, "1.0"));
}
