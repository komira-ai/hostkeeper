//! The `uninstall` order and its intent (design 5.7 items 5 to 7): only `Services.Unassign`
//! with `uninstall` records one, for each host that stops being assigned the service and lists
//! it; the rollout gate orders it under the hold, the quiet start and the one live update; it
//! survives a restart and ends when the host reports the service gone or is assigned it again;
//! retiring, excluding or abandoning a host orders nothing. Every time is the rig's clock.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope as MinScope;
use hostkeeper_wire::wire::{Health, OrderId, Outcome, ServiceReply, UpdateResult};
use serde_json::json;

use crate::assign_support::{
    assign, intents, order_to, set_min, told, unassign, uninstall_order, why_of,
};
use crate::assignment::{Entry, ServiceAssignments};
use crate::decide::{Gate, Skip, Why};
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask_typed, beat_of, hold, operator, restart, rig, runs, scope_host, set,
    start, unhold, update,
};
use crate::ops::OpsState;
use crate::orders::Order;
use crate::rollback_tests::unwritable;
use crate::testutil::{Log, OPERATOR, Rig, version};

/// The last entry of the audit ring, as (action, by, service, node).
fn last_audit(rig: &Rig) -> (String, String, Option<String>, Option<String>) {
    let state = rig.coordinator.ops().state();
    let last = state.audit.last().expect("an entry").clone();
    (last.action, last.by, last.service, last.node_id)
}

/// Whether the order is an uninstall.
fn is_uninstall(order: Option<&Order>) -> bool {
    matches!(order, Some(Order::Uninstall { .. }))
}

/// Catches: an `Unassign` without `uninstall` that orders an uninstall, or an update to a host
/// it took the service off: the host keeps running and listing it, is never told anything of
/// it again, and no intent is recorded.
#[tokio::test]
async fn removing_a_role_assignment_without_uninstall_leaves_the_service_alone() {
    let rig = rig("uninstall-none", "");
    assert_eq!(order_to(&rig, "d1", vec![runs(WEB, "1.0")]), None);
    set(&rig, WEB, "2.0", 1).await;
    let entry = unassign(&rig, WEB, "", "daemon", false)
        .expect("taken off the daemons")
        .entry
        .expect("audited");
    assert_eq!(entry.action, "unassign");
    assert_eq!(entry.reason.as_deref(), Some("role daemon"));
    assert_eq!(entry.node_id, None);
    for at in [2_000, 2_015, 2_200] {
        rig.at(at);
        assert_eq!(
            told(&rig, "d1", vec![runs(WEB, "1.0")]),
            (ServiceReply::Nothing, None),
            "at {at}"
        );
    }
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());
    assert!(
        rig.coordinator
            .lock_fleet()
            .lists(&crate::testutil::id("d1"), WEB)
    );
    let listed = rig
        .coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    let web = &listed.services[1];
    assert_eq!(web.service, WEB);
    assert!(web.hosts.is_empty(), "{web:?}");
    assert_eq!(web.roles, ["storage"]);
    assert!(web.uninstalling.is_empty());
}

/// Catches: an uninstall intent computed per role instead of per host: of two daemons that run
/// `web`, the one assigned it by its own entry keeps it and is told nothing; the other, and only
/// it, is ordered to uninstall; a daemon that does not run it is ordered nothing.
#[tokio::test]
async fn unassign_with_uninstall_orders_uninstall_only_on_hosts_that_stop_being_assigned() {
    let rig = rig("uninstall-per-host", "");
    for host in ["d1", "d2"] {
        assert_eq!(order_to(&rig, host, vec![runs(WEB, "1.0")]), None);
    }
    assert_eq!(order_to(&rig, "d3", vec![]), None);
    assign(&rig, WEB, "d2", "").expect("d2 by its own entry");
    let entry = unassign(&rig, WEB, "", "daemon", true)
        .expect("taken off the daemons")
        .entry
        .expect("audited");
    assert_eq!(entry.reason.as_deref(), Some("role daemon; uninstall"));
    assert_eq!(intents(&rig, WEB), ["d1"]);
    assert_eq!(order_to(&rig, "d2", vec![runs(WEB, "1.0")]), None);
    assert_eq!(order_to(&rig, "d3", vec![]), None);
    let order = order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    assert_eq!(order, Some(uninstall_order(WEB, "uninstall-1", 1)));
    assert_eq!(
        order.expect("an order").to_proto(),
        agent::Order {
            order_id: "uninstall-1".to_owned(),
            r#try: 1,
            verb: Some(agent::order::Verb::Uninstall(agent::Uninstall {
                service: WEB.to_owned()
            })),
        }
    );
    assert_eq!(intents(&rig, DB), Vec::<String>::new());
}

/// Catches: an uninstall intent created by any call but `Unassign` with `uninstall`: each of the
/// others, from exclusions to a desired version, holds, minimum versions, an approval's
/// assignment, marks, halts, aborts and an `Unassign` without `uninstall`, is made in turn, and
/// after each no intent stands and the host is ordered no uninstall.
#[tokio::test]
async fn no_other_rpc_orders_an_uninstall() {
    let rig = rig("uninstall-no-other", "");
    let both = || vec![runs(WEB, "1.0"), runs(DB, "1.0")];
    assert_eq!(order_to(&rig, "d1", both()), None);
    let host = |id: &str| crate::testutil::id(id);
    let calls = [
        "exclude",
        "include",
        "exclude_abandon",
        "include_again",
        "hold",
        "unhold",
        "approve",
        "set_min_version",
        "desired",
        "mark_bad",
        "halt",
        "abort",
        "empty_assign",
        "unassign_without_uninstall",
    ];
    for call in calls {
        match call {
            "exclude" | "exclude_abandon" => {
                rig.coordinator
                    .exclude(
                        &operator(),
                        proto::ExcludeRequest {
                            service: WEB.to_owned(),
                            node_id: "d1".to_owned(),
                            reason: "disk swap".to_owned(),
                            abandon: call == "exclude_abandon",
                        },
                    )
                    .expect("excluded");
            }
            "include" | "include_again" => {
                rig.coordinator
                    .operate(
                        Some(OPERATOR),
                        br#"{"action":"include","service":"web","node_id":"d1"}"#,
                    )
                    .expect("included");
            }
            "hold" => {
                hold(&rig, WEB, scope_host("d1")).expect("held");
            }
            "unhold" => {
                unhold(&rig, WEB, scope_host("d1")).expect("released");
            }
            "approve" => {
                let services = [WEB.to_owned(), DB.to_owned()];
                let role = "daemon".parse().expect("a role");
                rig.coordinator
                    .assign_host("operator", &host("d1"), Some(&role), &services)
                    .expect("assigned");
            }
            "set_min_version" => {
                set_min(&rig, WEB, MinScope::Role("daemon".to_owned()), "0.5").expect("set");
            }
            "desired" => {
                set(&rig, WEB, "2.0", 1).await;
                set(&rig, DB, "2.0", 2).await;
            }
            "mark_bad" => {
                rig.coordinator
                    .mark_bad(
                        &operator(),
                        proto::MarkBadRequest {
                            service: DB.to_owned(),
                            version: "2.0".to_owned(),
                            reason: "broken".to_owned(),
                        },
                    )
                    .expect("marked");
            }
            "halt" => {
                rig.coordinator
                    .halt(
                        &operator(),
                        proto::HaltRequest {
                            service: DB.to_owned(),
                            reason: "tainted".to_owned(),
                        },
                    )
                    .expect("halted");
            }
            "abort" => {
                rig.coordinator
                    .abort(
                        &operator(),
                        proto::AbortRequest {
                            node_id: "d1".to_owned(),
                            service: WEB.to_owned(),
                            reason: "stuck".to_owned(),
                        },
                    )
                    .expect("queued");
            }
            "empty_assign" => {
                assert_eq!(
                    assign(&rig, WEB, "", "").expect_err("no scope"),
                    proto::ServicesRefusal::InvalidRequest
                );
            }
            "unassign_without_uninstall" => {
                unassign(&rig, DB, "d1", "", false).expect("taken off");
            }
            other => panic!("no call {other}"),
        }
        let (_, order) = told(&rig, "d1", both());
        assert!(!is_uninstall(order.as_ref()), "{call}: {order:?}");
        assert_eq!(intents(&rig, WEB), Vec::<String>::new(), "{call}");
        assert_eq!(intents(&rig, DB), Vec::<String>::new(), "{call}");
    }
}

/// Catches: an uninstall sent to a host that is retired, excluded or abandoned, or a retired
/// host's slot held after its record is final: a host whose update ends and that is then
/// retired (its heartbeats stop) leaves its services as they are and frees its slot; a host
/// that vanishes mid-update is freed by an exclusion with `abandon`; an excluded host with an
/// uninstall intent is ordered nothing.
#[tokio::test]
async fn retiring_a_host_sends_no_uninstall() {
    let rig = rig("uninstall-retire", "");
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        told(&rig, "d1", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );
    let mut running = runs(WEB, "1.0");
    running.updating = Some(version("2.0"));
    assert_eq!(
        told(&rig, "d1", vec![running.clone()]),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));

    // Retired: d1's last record is final (the update succeeded), then it is heard no more.
    let mut done = runs(WEB, "2.0");
    done.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Succeeded));
    assert_eq!(told(&rig, "d1", vec![done]), (ServiceReply::Nothing, None));
    rig.at(2_100);
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());

    // d2 vanishes mid-update: its slot holds until an operator abandons it.
    assert_eq!(
        told(&rig, "d2", vec![running]),
        (ServiceReply::Nothing, None)
    );
    rig.at(2_400);
    assert_eq!(
        told(&rig, "d3", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: WEB.to_owned(),
                node_id: "d2".to_owned(),
                reason: "host is gone".to_owned(),
                abandon: true,
            },
        )
        .expect("abandoned");
    assert_eq!(
        told(&rig, "d3", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());

    // An excluded host with an uninstall intent is ordered nothing at all.
    let rig = crate::marks_support::rig("uninstall-excluded", "");
    order_to(&rig, "d4", vec![runs(WEB, "1.0")]);
    unassign(&rig, WEB, "d4", "", true).expect("unassigned");
    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: WEB.to_owned(),
                node_id: "d4".to_owned(),
                reason: "retiring".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        told(&rig, "d4", vec![runs(WEB, "1.0")]),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(why_of(&rig, WEB, "d4"), Some(Why::Skip(Skip::NotAMember)));
    assert_eq!(
        intents(&rig, WEB),
        ["d4"],
        "the intent stands, and is not acted on"
    );
}

/// Catches: an uninstall intent kept in memory only, never cleared, or a slot kept after the
/// service is gone: the intent is in the ops file across a restart, the restarted coordinator
/// orders the same uninstall after its quiet start, the order holds the one live update, and
/// once the host reports the service gone the intent is cleared (as the coordinator's own
/// audited change, read back after another restart) and the slot is free.
#[tokio::test]
async fn an_uninstall_intent_survives_a_coordinator_restart_and_clears_when_the_host_reports_the_service_gone()
 {
    let mut rig = rig("uninstall-restart", "");
    order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    unassign(&rig, WEB, "d1", "", true).expect("unassigned");
    restart(&mut rig, "");
    assert_eq!(intents(&rig, WEB), ["d1"]);
    assert_eq!(order_to(&rig, "d1", vec![runs(WEB, "1.0")]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Quiet)));
    rig.at(2_031);
    assert_eq!(
        order_to(&rig, "d1", vec![runs(WEB, "1.0")]),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
    let mut removing = runs(WEB, "1.0");
    removing.updating = Some(version("1.0"));
    assert_eq!(order_to(&rig, "d1", vec![removing]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Updating)));

    assert_eq!(order_to(&rig, "d1", vec![]), None);
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());
    assert_eq!(
        last_audit(&rig),
        (
            "uninstalled".to_owned(),
            "coordinator".to_owned(),
            Some(WEB.to_owned()),
            Some("d1".to_owned())
        )
    );
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::NotAMember)));
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );

    restart(&mut rig, "");
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());
}

/// A host's report of the uninstall `order_id`, try 1, ended `outcome`, of `web` at 1.0 with
/// `health`.
fn verdict(order_id: &str, outcome: Outcome, health: Health) -> hostkeeper_wire::wire::ServiceBeat {
    let mut one = runs(WEB, "1.0");
    one.health = health;
    let mut report = UpdateResult::new(version("1.0"), outcome);
    report.order_id = OrderId::parse(order_id);
    report.order_try = Some(1);
    one.last_update = Some(report);
    one
}

/// Catches: a failed uninstall ordered again on its own, a declined one ordered before its wait
/// ends, or one ordered to a daemon that cannot run it: a failure is left for the operator (the
/// intent stands; the host's slot is held until it beats healthy, as after a failed update), a
/// deferral waits out its backoff and is then ordered again, and a daemon that did not announce
/// `install-v1` is told nothing (`unsupported`).
#[tokio::test]
async fn a_failed_uninstall_is_left_for_the_operator_and_a_declined_one_waits() {
    let rig = rig("uninstall-verdicts", "");
    for host in ["d1", "d2"] {
        order_to(&rig, host, vec![runs(WEB, "1.0")]);
        unassign(&rig, WEB, host, "", true).expect("unassigned");
    }
    assert_eq!(
        order_to(&rig, "d1", vec![runs(WEB, "1.0")]),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
    let failed = verdict("uninstall-1", Outcome::Failed, Health::Unhealthy);
    assert_eq!(order_to(&rig, "d1", vec![failed]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));
    assert_eq!(intents(&rig, WEB), ["d1", "d2"]);
    assert_eq!(order_to(&rig, "d2", vec![runs(WEB, "1.0")]), None);
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));

    let healed = verdict("uninstall-1", Outcome::Failed, Health::Healthy);
    assert_eq!(order_to(&rig, "d1", vec![healed]), None);
    assert_eq!(
        order_to(&rig, "d2", vec![runs(WEB, "1.0")]),
        Some(uninstall_order(WEB, "uninstall-2", 1))
    );
    let mut declined = verdict("update-0", Outcome::Succeeded, Health::Healthy);
    declined.deferred = Some(
        serde_json::from_value(json!({
            "target": "1.0", "reason": "drain_bound", "retry_after_secs": 300
        }))
        .expect("a deferral"),
    );
    assert_eq!(order_to(&rig, "d2", vec![declined.clone()]), None);
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Skip(Skip::Deferred)));
    rig.at(2_400);
    // The host's last verdict, of `update-0` at try 1, puts the next order of `web` at try 2.
    assert_eq!(
        order_to(&rig, "d2", vec![declined]),
        Some(uninstall_order(WEB, "uninstall-2", 2))
    );

    let rig = crate::marks_support::rig("uninstall-unsupported", "");
    let old = |services| beat_of(NEW_DAEMON, services);
    ask_typed(&rig, "d1", "daemon", old(vec![runs(WEB, "1.0")]));
    unassign(&rig, WEB, "d1", "", true).expect("unassigned");
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", old(vec![runs(WEB, "1.0")])),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::Unsupported)));
    // Busy with another service, the host is not ordered the uninstall either.
    let mut db = runs(DB, "1.0");
    db.updating = Some(version("2.0"));
    ask_typed(&rig, "d1", "daemon", old(vec![runs(WEB, "1.0"), db]));
    assert_eq!(
        why_of(&rig, WEB, "d1"),
        Some(Why::Gate(Gate::BusyElsewhere))
    );
}

/// Catches: an intent that outlives the assignment that ends it, or one dropped when its clear
/// cannot be written: a host ordered to uninstall that the service is assigned to again (by a
/// role, while the coordinator had not heard from it) has the intent cleared at its next
/// heartbeat as `uninstall_cancelled`, and is ordered no uninstall; a clear whose ops file cannot
/// be written is logged, and the intent stands for the next heartbeat.
#[tokio::test]
async fn an_intent_ends_when_the_service_is_assigned_again_and_stands_when_its_clear_fails() {
    let mut rig = rig("uninstall-cancelled", "");
    order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    unassign(&rig, WEB, "", "daemon", true).expect("unassigned");
    assert_eq!(intents(&rig, WEB), ["d1"]);
    restart(&mut rig, "");
    rig.at(2_031);
    assign(&rig, WEB, "", "daemon").expect("the daemons again");
    assert_eq!(intents(&rig, WEB), ["d1"], "d1 is not heard from yet");
    let (_, order) = told(&rig, "d1", vec![runs(WEB, "1.0")]);
    assert!(!is_uninstall(order.as_ref()), "{order:?}");
    assert_eq!(intents(&rig, WEB), Vec::<String>::new());
    assert_eq!(last_audit(&rig).0, "uninstall_cancelled");

    let mut state = OpsState::default();
    state.assignments.insert(
        WEB.to_owned(),
        ServiceAssignments {
            hosts: BTreeMap::from([("d1".to_owned(), Entry::Exclude)]),
            uninstall: BTreeMap::from([("d1".to_owned(), 1)]),
            uninstall_generation: 1,
            ..ServiceAssignments::default()
        },
    );
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(state));
    rig.at(2_100);
    let log = Log::default();
    let _guard = log.install();
    assert_eq!(told(&rig, "d1", vec![]), (ServiceReply::Nothing, None));
    assert_eq!(intents(&rig, WEB), ["d1"]);
    assert!(
        log.text()
            .contains("an uninstall intent could not be cleared: it stands"),
        "{}",
        log.text()
    );
}
