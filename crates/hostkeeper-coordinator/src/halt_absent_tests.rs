//! A halt clears itself when the host that raised it reports the service absent (the incident of
//! a first install whose rollback purged the package and then failed): audited as the coordinator's
//! `clear_halt`, with the host ordered the install again in the same reply. A halt whose host
//! still has the service, another host's, and an operator's stay for an operator. Every time is
//! the rig's clock.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat, UpdateResult};
use serde_json::json;

use crate::assign_support::{absent, order_to, told, why_of};
use crate::decide::{Gate, Skip, Why};
use crate::halt::ABSENT_REASON;
use crate::marks_support::{WEB, operator, rig, runs, set};
use crate::ops::{AuditEntry, Halt, HaltOrigin, OpsState, ServiceOps};
use crate::orders::Order;
use crate::rollback_tests::{AUTO, ONE, restart, rig as legacy_rig, rig_over, unwritable, who};
use crate::testutil::{LEGACY, Rig, beat, version};

/// `service` at 1.0 in `health`, reporting that its install of 2.0 halted.
fn halting(service: &str, health: &str) -> ServiceBeat {
    serde_json::from_value(json!({
        "name": service, "version": "1.0", "health": health,
        "last_update": {"target": "2.0", "outcome": "failed", "phase": "halt",
            "reason": "unit_failed"}
    }))
    .expect("a service beat")
}

fn halted_by(rig: &Rig) -> Option<String> {
    rig.coordinator
        .ops()
        .state()
        .services
        .get(WEB)
        .and_then(|marks| marks.halt.as_ref())
        .map(|halt| halt.node_id.clone())
}

/// The audit entries of `action`.
fn entries(rig: &Rig, action: &str) -> Vec<AuditEntry> {
    rig.coordinator
        .ops()
        .audit_after(0)
        .expect("the audit")
        .into_iter()
        .filter(|entry| entry.action == action)
        .collect()
}

fn generation(rig: &Rig) -> u64 {
    rig.coordinator.ops().state().halt_generation_in(None, WEB)
}

fn is_install(order: Option<Order>) -> bool {
    matches!(order, Some(Order::Install { service, .. }) if service == WEB)
}

/// A halted `web` of host `d1`, which listed it healthy when it halted; 2.0 is desired.
async fn halted() -> Rig {
    let rig = rig("halt-absent", "");
    set(&rig, WEB, "2.0", 7).await;
    assert_eq!(order_to(&rig, "d1", vec![halting(WEB, "healthy")]), None);
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));
    rig
}

/// Proves: the host that halted `web` and then reports it absent clears the halt with no operator
/// call, the clear is one `clear_halt` entry by the coordinator naming the host and the reason,
/// the generation counts (the agents lift their hold on it), the host is ordered the install of the
/// desired version in the same reply although its report of the failed 2.0 still stands, and the
/// report it repeats does not raise the halt again.
///
/// Catches: a halt that outlives the install it protected and holds the service for the group
/// until an operator's `clear_halt`.
#[tokio::test]
async fn a_halt_clears_itself_when_its_host_reports_the_service_absent() {
    let rig = halted().await;
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Halted)));
    assert_eq!(generation(&rig), 0);

    let order = order_to(&rig, "d1", vec![halting(WEB, "absent")]);
    assert!(is_install(order), "the install is ordered");
    assert_eq!(halted_by(&rig), None);
    assert_eq!(generation(&rig), 1);
    let cleared = entries(&rig, "clear_halt");
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].by, "coordinator");
    assert_eq!(cleared[0].service.as_deref(), Some(WEB));
    assert_eq!(cleared[0].node_id.as_deref(), Some("d1"));
    assert_eq!(cleared[0].reason.as_deref(), Some(ABSENT_REASON));

    // The host repeats the report of the failed install: not a new halt.
    order_to(&rig, "d1", vec![halting(WEB, "absent")]);
    assert_eq!(halted_by(&rig), None);
    assert_eq!(entries(&rig, "clear_halt").len(), 1);
    assert_eq!(entries(&rig, "halt").len(), 1);
}

/// Proves: once the halt is gone the gate does not hold the absent host as `failed_here` for the
/// 2.0 it failed: its failed report is set aside as `Hosts.Retry` does.
///
/// Catches: a clear that lifts the halt and leaves the host skipped as `failed_here`, so no order
/// is sent.
#[tokio::test]
async fn the_cleared_host_is_not_held_by_its_failed_report() {
    let rig = halted().await;
    order_to(&rig, "d1", vec![halting(WEB, "absent")]);
    assert_eq!(why_of(&rig, WEB, "d1"), None);
    assert_ne!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));
}

/// Proves: a host that reports its halt and the service absent in one heartbeat is cleared by
/// it, and ordered the install.
///
/// Catches: a clear that reads only a halt standing from an earlier heartbeat.
#[tokio::test]
async fn a_host_that_reports_its_halt_and_the_service_absent_at_once_is_cleared() {
    let rig = rig("halt-absent-once", "");
    set(&rig, WEB, "2.0", 7).await;
    let order = order_to(&rig, "d1", vec![halting(WEB, "absent")]);
    assert!(is_install(order));
    assert_eq!(halted_by(&rig), None);
    assert_eq!(entries(&rig, "halt").len(), 1);
    assert_eq!(entries(&rig, "clear_halt").len(), 1);
}

/// Proves: a halt whose host still has the service installed stays, healthy or not, however often
/// it reports, and nothing is audited as a clear.
///
/// Catches: a halt cleared by any heartbeat of its host, or by a heartbeat that does not say the
/// service is absent.
#[tokio::test]
async fn a_halt_whose_host_still_runs_the_service_stays() {
    let rig = halted().await;
    for health in ["healthy", "unhealthy"] {
        assert_eq!(order_to(&rig, "d1", vec![halting(WEB, health)]), None);
        assert_eq!(halted_by(&rig), Some("d1".to_owned()), "{health}");
    }
    assert_eq!(order_to(&rig, "d1", vec![runs(WEB, "1.0")]), None);
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Halted)));
    assert_eq!(generation(&rig), 0);
    assert!(entries(&rig, "clear_halt").is_empty());
}

/// Proves: another host reporting the service absent does not clear the halt of a host that has
/// it, and neither does the halting host's absence while another host reported a halt too (it
/// may still run the service).
///
/// Catches: a halt cleared by the wrong host, or one that swallows another host's halt report.
#[tokio::test]
async fn an_absent_report_of_another_host_does_not_clear_the_halt() {
    let rig = halted().await;
    order_to(&rig, "d2", vec![absent(WEB, "1.0")]);
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));

    // d2 halts as well while the halt stands, and keeps the service.
    order_to(&rig, "d2", vec![halting(WEB, "healthy")]);
    let (reply, order) = told(&rig, "d1", vec![halting(WEB, "absent")]);
    assert_eq!(
        (reply, order),
        (hostkeeper_wire::wire::ServiceReply::Nothing, None)
    );
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));
    assert!(entries(&rig, "clear_halt").is_empty());
}

/// Proves: a halt an operator raised names no host, and no host's absent report clears it.
///
/// Catches: an operator's halt cleared by a host that never ran the service.
#[tokio::test]
async fn an_operator_halt_is_not_cleared_by_an_absent_host() {
    let rig = rig("halt-absent-operator", "");
    set(&rig, WEB, "2.0", 7).await;
    rig.coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
    order_to(&rig, "d1", vec![absent(WEB, "1.0")]);
    assert_eq!(halted_by(&rig), Some(String::new()));
    assert!(entries(&rig, "clear_halt").is_empty());
}

/// The halt `d1` raised of the legacy service.
fn legacy_halt() -> ServiceOps {
    let report: UpdateResult = serde_json::from_value(json!({
        "target": "2.0", "outcome": "failed", "phase": "halt", "reason": "unit_failed"
    }))
    .expect("a report");
    ServiceOps {
        halt: Some(Halt {
            node_id: "d1".to_owned(),
            check: "unit_failed".to_owned(),
            target: version("2.0"),
            report: Some(report),
            origin: HaltOrigin::Hook,
        }),
        ..ServiceOps::default()
    }
}

/// The top-level beat of a host that lists no services: the legacy service in `health`.
fn legacy_beat(health: Health) -> Heartbeat {
    let mut b = beat("1.0");
    b.health = health;
    b
}

/// Proves: the legacy service, reported in the heartbeat's top-level fields, is cleared by the
/// same word: its halting host reports it absent.
///
/// Catches: a clear that reads only the listed services.
#[tokio::test]
async fn the_legacy_service_of_a_host_reporting_it_absent_is_cleared() {
    let mut rig = legacy_rig("halt-absent-legacy", AUTO, ONE).await;
    crate::rollback_tests::seed(&rig, |marks| *marks = legacy_halt());
    restart(&mut rig, AUTO);
    rig.coordinator
        .heartbeat_native(who("d1"), legacy_beat(Health::Healthy))
        .expect("a heartbeat");
    assert!(rig.coordinator.ops().marks(LEGACY).halt.is_some());
    rig.coordinator
        .heartbeat_native(who("d1"), legacy_beat(Health::Absent))
        .expect("a heartbeat");
    assert_eq!(rig.coordinator.ops().marks(LEGACY).halt, None);
    assert_eq!(entries(&rig, "clear_halt").len(), 1);
}

/// Proves: a clear that cannot be written leaves the halt standing, and the host is ordered
/// nothing; the clear is not recorded as done.
///
/// Catches: a halt lifted in memory only, which a restart would bring back, or a host ordered
/// while its halt stands.
#[tokio::test]
async fn a_clear_that_cannot_be_written_leaves_the_halt_standing() {
    let mut state = OpsState::default();
    state.services.insert(LEGACY.to_owned(), legacy_halt());
    let rig = rig_over("halt-absent-unwritable", AUTO, ONE, unwritable(state)).await;
    let answered = rig
        .coordinator
        .heartbeat_native(who("d1"), legacy_beat(Health::Absent))
        .expect("a heartbeat");
    assert_eq!(answered.reply, hostkeeper_wire::wire::ServiceReply::Nothing);
    assert!(rig.coordinator.ops().marks(LEGACY).halt.is_some());
    assert!(entries(&rig, "clear_halt").is_empty());
}

/// `service` absent, reporting that its install of 2.0 halted at the try `order_try`.
fn halting_at(service: &str, order_try: u64) -> ServiceBeat {
    serde_json::from_value(json!({
        "name": service, "version": "1.0", "health": "absent",
        "last_update": {"target": "2.0", "outcome": "failed", "phase": "halt",
            "reason": "unit_failed", "order_try": order_try}
    }))
    .expect("a service beat")
}

/// Proves: only a halt of the first try of its order clears itself: a halt that names try 1
/// clears, and a halt of try 2 (the install a clear ordered again halted too) stands with its
/// host held, no clear entry and no new generation, so a check that fails before anything is
/// placed cannot clear and retry at every heartbeat; an operator's clear still lifts it.
///
/// Catches: a halt that is cleared and re-ordered at every heartbeat for as long as its cause
/// stays.
#[tokio::test]
async fn a_halt_of_a_later_try_than_the_first_stays_for_an_operator() {
    let rig = rig("halt-absent-later-try", "");
    set(&rig, WEB, "2.0", 7).await;
    assert_eq!(order_to(&rig, "d1", vec![halting_at(WEB, 2)]), None);
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));
    assert_eq!(order_to(&rig, "d1", vec![halting_at(WEB, 2)]), None);
    assert_eq!(halted_by(&rig), Some("d1".to_owned()));
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Halted)));
    assert_eq!(generation(&rig), 0);
    assert!(entries(&rig, "clear_halt").is_empty());

    let first = crate::marks_support::rig("halt-absent-first-try", "");
    set(&first, WEB, "2.0", 7).await;
    assert!(is_install(order_to(&first, "d1", vec![halting_at(WEB, 1)])));
    assert_eq!(halted_by(&first), None);
    assert_eq!(entries(&first, "clear_halt").len(), 1);
}
