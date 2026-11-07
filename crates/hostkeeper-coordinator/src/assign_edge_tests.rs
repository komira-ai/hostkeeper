//! The edges of assignment, install and uninstall: what frees the slot of an install, what a
//! heartbeat may and may not end of an uninstall intent, the JSON route, a host taken off a
//! service mid-update, and the hosts and roles the calls know without having heard them.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Outcome, ServiceReply, UpdateResult};

use crate::assign_support::{
    absent, assign, install, intents, order_to, told, unassign, uninstall_order, why_of,
};
use crate::assignment::{Entry, ServiceAssignments};
use crate::decide::{Gate, Why};
use crate::install_tests::{ONE_STAGE, set_one_slot};
use crate::marks_support::{
    OLD_DAEMON, WEB, ask, ask_typed, beat_of, rig, runs, set, start, update,
};
use crate::ops::{FailureAck, OpsState, OpsStore};
use crate::rollback_tests::unwritable;
use crate::testutil::{OPERATOR, version};

/// Catches: a failed install that holds the role's slot for ever because the service it left
/// `absent` never beats healthy, or an install order that holds it past its age: after either,
/// another host of the role is ordered (the daemons update one at a time here: `max_updating`
/// of 1 and one stage).
#[tokio::test]
async fn a_failed_or_unheard_install_frees_the_slot() {
    let rig = rig("edge-install-freed", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install("2.0", 1)));
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
    let mut failed = absent(WEB, "0.0");
    failed.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Failed));
    assert_eq!(order_to(&rig, "d1", vec![failed]), None);
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );

    let rig = crate::marks_support::rig("edge-install-aged", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install("2.0", 1)));
    rig.at(2_119);
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
    rig.at(2_121);
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );
}

/// Catches: an intent ended by a heartbeat that cannot say the service is gone (an agent that
/// does not list every service it runs), or an uninstall sent over the JSON route, which carries
/// none: the intent stands, nothing is audited, and the JSON reply says nothing.
#[tokio::test]
async fn only_a_heartbeat_that_lists_every_service_ends_an_intent() {
    let rig = rig("edge-intent-v1", "");
    order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    unassign(&rig, WEB, "d1", "", true).expect("unassigned");
    let audited = rig.coordinator.ops().state().audit.len();
    ask_typed(&rig, "d1", "daemon", beat_of(&[], vec![]));
    assert_eq!(intents(&rig, WEB), ["d1"]);
    assert_eq!(rig.coordinator.ops().state().audit.len(), audited);
    assert_eq!(
        ask(
            &rig,
            "d1",
            "daemon",
            beat_of(OLD_DAEMON, vec![runs(WEB, "1.0")])
        ),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(
            &rig,
            "d1",
            "daemon",
            beat_of(crate::marks_support::INSTALLER, vec![runs(WEB, "1.0")])
        ),
        ServiceReply::Nothing,
        "the JSON reply carries no uninstall"
    );
    assert_eq!(
        order_to(&rig, "d1", vec![runs(WEB, "1.0")]),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
}

/// Catches: a host taken off a service while it ran an update of it holding the role's slot for
/// ever, its later beats ignored: while it runs the update the next host of the role is held,
/// and once it reports the update ended, that host is ordered (the daemons update one at a time
/// here: `max_updating` of 1 and one stage).
#[tokio::test]
async fn a_host_taken_off_mid_update_frees_the_slot_when_the_update_ends() {
    let rig = rig("edge-unassigned-mid-update", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        told(&rig, "d1", vec![runs(WEB, "1.0")]).0,
        update(WEB, "2.0")
    );
    let mut running = runs(WEB, "1.0");
    running.updating = Some(version("2.0"));
    order_to(&rig, "d1", vec![running.clone()]);
    unassign(&rig, WEB, "d1", "", false).expect("taken off d1");
    order_to(&rig, "d1", vec![running]);
    let (_, order) = ask_typed(
        &rig,
        "d2",
        "daemon",
        beat_of(OLD_DAEMON, vec![runs(WEB, "1.0")]),
    );
    assert_eq!(order, None);
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
    let mut done = runs(WEB, "2.0");
    done.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Succeeded));
    order_to(&rig, "d1", vec![done]);
    let (reply, _) = ask_typed(
        &rig,
        "d2",
        "daemon",
        beat_of(OLD_DAEMON, vec![runs(WEB, "1.0")]),
    );
    assert_eq!(reply, update(WEB, "2.0"));
}

/// Catches: a host the ops file names (by its labels) or a role an assignment names refused as
/// unknown before the coordinator has heard from them: each is a scope the calls take.
#[tokio::test]
async fn the_calls_know_the_hosts_and_roles_the_ops_file_names() {
    let mut rig = rig("edge-known", "");
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"set_labels","node_id":"h9","labels":{"zone":"a"}}"#,
        )
        .expect("labelled");
    assign(&rig, WEB, "h9", "").expect("h9 is named by its labels");

    let mut state = OpsState::default();
    state.assignments.insert(
        WEB.to_owned(),
        ServiceAssignments {
            roles: BTreeMap::from([("builder".to_owned(), Entry::Include)]),
            ..ServiceAssignments::default()
        },
    );
    rig.coordinator = start(&rig.dir, &rig.clock, "")
        .with_ops(OpsStore::with_writer(state, Box::new(|_| Ok(()))));
    let entry = unassign(&rig, WEB, "", "builder", false)
        .expect("builder is named by an assignment")
        .entry
        .expect("audited");
    assert_eq!(entry.reason.as_deref(), Some("role builder"));
    assert_eq!(
        assign(&rig, WEB, "", "builder").expect_err("named no more"),
        proto::ServicesRefusal::UnknownRole
    );
}

/// Catches: an install order ended by a staged copy (a host that lists the service `absent` at
/// the version it is installing), which would free the role's slot before the install ran (the
/// daemons update one at a time here: `max_updating` of 1 and one stage).
#[tokio::test]
async fn an_absent_copy_at_the_target_does_not_end_the_install_order() {
    let rig = rig("edge-install-staged", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install("2.0", 1)));
    assert_eq!(
        order_to(&rig, "d1", vec![absent(WEB, "2.0")]),
        Some(install("2.0", 1)),
        "still under its own order"
    );
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "1.0")]).0,
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
}

/// Catches: a host whose role changed away from the service's having its beats of it taken as
/// the service's, or ordered: it is told nothing of the service at its new role.
#[tokio::test]
async fn a_host_that_changed_role_is_not_ordered_the_service_of_its_old_role() {
    let rig = rig("edge-role-change", "");
    order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    set(&rig, WEB, "2.0", 1).await;
    let (reply, order) = ask_typed(
        &rig,
        "d1",
        "frontend",
        beat_of(crate::marks_support::INSTALLER, vec![runs(WEB, "1.0")]),
    );
    assert_eq!((reply, order), (ServiceReply::Nothing, None));
}

/// Catches: an install given while the host's failure ack could not be dropped from the ops
/// file: no order goes out while the ack is on disk.
#[tokio::test]
async fn no_install_goes_out_while_the_hosts_ack_cannot_be_dropped() {
    let mut rig = rig("edge-install-ack", "");
    set(&rig, WEB, "2.0", 1).await;
    let mut state = OpsState::default();
    state
        .services
        .entry(WEB.to_owned())
        .or_default()
        .failure_acked
        .push(FailureAck {
            node_id: "d1".to_owned(),
            report: UpdateResult::new(version("1.9"), Outcome::Failed),
        });
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(state));
    rig.at(2_031);
    assert_eq!(order_to(&rig, "d1", vec![]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), None, "the gate would order it");
}
