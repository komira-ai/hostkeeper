//! What `Releases.Status` says of a service's update plan: its stages after collapsing, the hosts
//! of each stage with their states, the stage of each host, and why the plan orders no new host
//! now; and the gate the plan puts on an uninstall. Every time is the rig's clock; nothing
//! sleeps.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::{Value, json};

use crate::assign_support::{order_to, told, unassign, uninstall_order, why_of};
use crate::decide::{Gate, Why};
use crate::grpc::report;
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, beat_of, digest, hold, operator, restart, rig, runs, scope_host, set,
    start, update,
};
use crate::testutil::{OPERATOR, Rig, version};

/// What a daemon reports of `name` running `running`, healthy, with the fields of `extra`
/// (`updating`, `last_update`, `deferred`) added.
fn service(name: &str, running: &str, extra: &Value) -> ServiceBeat {
    let mut one = json!({"name": name, "version": running, "health": "healthy"});
    one.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    serde_json::from_value(one).expect("a service beat")
}

/// A heartbeat of a daemon that lists `web` running `running` with the fields of `extra`.
fn web_beat(running: &str, extra: &Value) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![service(WEB, running, extra)])
}

/// `host` heartbeats `web` at `running` with the fields of `extra`, and is answered.
fn beat_web(rig: &Rig, host: &str, running: &str, extra: &Value) -> ServiceReply {
    ask(rig, host, "daemon", web_beat(running, extra))
}

/// The hosts `d1` to `d{n}` heartbeat `web` at `running`.
fn farm(rig: &Rig, n: usize, running: &str) {
    for i in 1..=n {
        let reply = beat_web(rig, &format!("d{i}"), running, &json!({}));
        assert_eq!(reply, ServiceReply::Nothing, "d{i}: no desired state yet");
    }
}

/// A rig with eight daemons on `web` at 1.0, whose rollout-file entry has `extra` lines.
fn eight(test: &str, extra: &str) -> Rig {
    let rig = rig(test, extra);
    farm(&rig, 8, "1.0");
    rig
}

/// Sets `web` to 2.0 as CI does, with `max_updating` and `min_up` for the daemons.
async fn set_limits(rig: &Rig, max_updating: u32, min_up: u32) {
    let body = json!({
        "service": WEB, "version": "2.0", "release_digest": digest(1),
        "max_updating": {"daemon": max_updating}, "min_up": {"daemon": min_up},
    });
    rig.set(&body.to_string()).await;
}

/// The status of `service` as `Releases.Status` answers it.
fn answer(rig: &Rig, service: &str) -> proto::ServiceStatus {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("the status");
    report::status(&status, service)
        .services
        .into_iter()
        .next()
        .expect("the service")
}

/// The plan of `web`.
fn plan(rig: &Rig) -> proto::RolloutPlan {
    answer(rig, WEB).plan.expect("a plan")
}

/// A host of a plan in the role `daemon`, not deferring.
fn member(node: &str, state: proto::PlanHostState) -> proto::PlanHost {
    proto::PlanHost {
        node_id: node.to_owned(),
        role: "daemon".to_owned(),
        state: state.into(),
        deferring: false,
    }
}

/// A stage entered by itself, with `members`.
fn stage(
    number: u32,
    percent: Option<u32>,
    hosts: u32,
    members: Vec<proto::PlanHost>,
) -> proto::PlanStage {
    proto::PlanStage {
        number,
        percent,
        hosts,
        advance: i32::from(proto::StageAdvance::Auto),
        members,
    }
}

/// The hosts `ids` waiting, in the order given.
fn waiting(ids: &[&str]) -> Vec<proto::PlanHost> {
    ids.iter()
        .map(|id| member(id, proto::PlanHostState::Waiting))
        .collect()
}

/// The stage of each host that `web`'s status lists, by node id.
fn stages_of(rig: &Rig) -> Vec<(String, Option<u32>)> {
    answer(rig, WEB)
        .hosts
        .iter()
        .map(|h| (h.node_id.clone(), h.view.as_ref().expect("a view").stage))
        .collect()
}

/// The gate reason `web`'s status gives `node`.
fn gate_of(rig: &Rig, node: &str) -> proto::GateReason {
    let status = answer(rig, WEB);
    let host = status
        .hosts
        .iter()
        .find(|h| h.node_id == node)
        .expect("the host");
    host.view.as_ref().expect("a view").gate_reason()
}

fn pause(rig: &Rig) -> Result<proto::PauseResponse, proto::RolloutsRefusal> {
    rig.coordinator.rollout_pause(
        &operator(),
        proto::PauseRequest {
            service: WEB.to_owned(),
            reason: "watching the farm".to_owned(),
            host_group: String::new(),
        },
    )
}

fn resume(rig: &Rig) -> Result<proto::ResumeResponse, proto::RolloutsRefusal> {
    rig.coordinator.rollout_resume(
        &operator(),
        proto::ResumeRequest {
            service: WEB.to_owned(),
            reason: "looks fine".to_owned(),
            host_group: String::new(),
        },
    )
}

/// Proves: for a service on eight hosts with the default plan, the status lists three stages
/// numbered 1 to 3, because the 1% and 10% stages come out one host and fold into one: percent
/// 10 (the last folded), 50 and 100, holding 1, 4 and 8 hosts, each entered automatically; the
/// members are the hosts by node id, all waiting; eligible 8, the desired version, the first
/// stage current, and no reason to wait, since the stage has a free place. Each host's status
/// names the stage that lists it.
/// Catches: stages listed before collapsing (four of them), a folded stage keeping the percent
/// of its first stage, stage numbers that skip, and hosts missing from the stage that takes them.
#[tokio::test]
async fn the_default_plan_over_eight_hosts_is_three_stages_of_one_four_and_eight() {
    let rig = eight("plan-status-default", "");
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        plan(&rig),
        proto::RolloutPlan {
            version: "2.0".to_owned(),
            stages: vec![
                stage(1, Some(10), 1, waiting(&["d1"])),
                stage(2, Some(50), 4, waiting(&["d2", "d3", "d4"])),
                stage(3, Some(100), 8, waiting(&["d5", "d6", "d7", "d8"])),
            ],
            current: 1,
            eligible: 8,
            waiting: proto::PlanWait::None.into(),
            paused: false,
            paused_by: None,
            pause_reason: None,
            finished: false,
            waiting_on: vec![],
            host_group: String::new(),
            waiting_on_why: vec![],
        }
    );
    let stages: Vec<(String, Option<u32>)> = [1, 2, 2, 2, 3, 3, 3, 3]
        .into_iter()
        .enumerate()
        .map(|(i, number)| (format!("d{}", i + 1), Some(number)))
        .collect();
    assert_eq!(stages_of(&rig), stages);
}

/// Proves: a plan of a service of one host is one stage of one host at 100 percent.
/// Catches: a one-host service shown with the empty stage list of a service nothing is eligible
/// for, or with stages the host cannot fill.
#[tokio::test]
async fn one_host_is_one_stage() {
    let rig = rig("plan-status-one", "");
    farm(&rig, 1, "1.0");
    set(&rig, WEB, "2.0", 1).await;
    let plan = plan(&rig);
    assert_eq!(plan.stages, vec![stage(1, Some(100), 1, waiting(&["d1"]))]);
    assert_eq!((plan.current, plan.eligible), (1, 1));
}

/// Proves: a canary stage has no percent: `canary = { count = 3 }` before a plan of `[100]` on
/// eight hosts is a first stage of 3 hosts that names none, then the stage of every host at 100.
/// Catches: the canary's host count shown as a percent of the fleet.
#[tokio::test]
async fn a_canary_stage_has_no_percent() {
    let rig = eight(
        "plan-status-canary",
        "canary = { count = 3 }\nstages = [100]",
    );
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        plan(&rig).stages,
        vec![
            stage(1, None, 3, waiting(&["d1", "d2", "d3"])),
            stage(2, Some(100), 8, waiting(&["d4", "d5", "d6", "d7", "d8"])),
        ]
    );
}

/// Proves: a manual stage that is folded into the one before it makes the next stage that adds
/// hosts manual: `[1, { percent = 10, advance = "manual" }, 50, 100]` on eight hosts is a first
/// stage of one host, entered automatically, then the 50% stage and the 100% stage, and the
/// 50% stage is manual because the 10% stage folded into the first was.
/// Catches: the manual flag of a folded stage lost, so the plan goes wider without an operator.
#[tokio::test]
async fn a_manual_stage_folded_into_the_one_before_makes_the_next_stage_manual() {
    let rig = eight(
        "plan-status-folded-manual",
        "stages = [1, { percent = 10, advance = \"manual\" }, 50, 100]",
    );
    set(&rig, WEB, "2.0", 1).await;
    let advances: Vec<i32> = plan(&rig).stages.iter().map(|s| s.advance).collect();
    assert_eq!(
        advances,
        [
            i32::from(proto::StageAdvance::Auto),
            i32::from(proto::StageAdvance::Manual),
            i32::from(proto::StageAdvance::Auto),
        ]
    );
}

/// Proves: the hosts that have started fill the stages first, the done ones first, then those
/// updating, then the rest by node id: with d5 and d6 already at 2.0 and d7 updating to it, d5
/// is the first stage, and the stages after it take d6, d7 and then the waiting hosts, each
/// with its state; the plan is in the second stage, as the first is done.
/// Catches: members listed by node id whatever they did, so a done host sits in a later stage
/// than the one it completed, or a stage that shows a waiting host before an updating one.
#[tokio::test]
async fn hosts_that_started_fill_the_stages_first_and_show_their_states() {
    let rig = rig("plan-status-started", "");
    farm(&rig, 8, "1.0");
    beat_web(&rig, "d5", "2.0", &json!({}));
    beat_web(&rig, "d6", "2.0", &json!({}));
    beat_web(&rig, "d7", "1.0", &json!({"updating": "2.0"}));
    set(&rig, WEB, "2.0", 1).await;
    let plan = plan(&rig);
    use proto::PlanHostState::{Done, Updating, Waiting};
    assert_eq!(
        plan.stages,
        vec![
            stage(1, Some(10), 1, vec![member("d5", Done)]),
            stage(
                2,
                Some(50),
                4,
                vec![
                    member("d6", Done),
                    member("d7", Updating),
                    member("d1", Waiting)
                ]
            ),
            stage(3, Some(100), 8, waiting(&["d2", "d3", "d4", "d8"])),
        ]
    );
    assert_eq!(plan.current, 2);
    // Three of the stage's four places are taken, so it has a free place.
    assert_eq!(plan.waiting, i32::from(proto::PlanWait::None));
}

/// Proves: a host that failed the desired version is a FAILED member of its stage, and a host
/// that reports the version and has not baked is BAKING (two beats of bake are asked, and it
/// has beaten once).
/// Catches: a failed or a baking host shown as waiting or done.
#[tokio::test]
async fn failed_and_baking_hosts_show_their_states() {
    let rig = eight("plan-status-states", "bake_beats = 2");
    set_limits(&rig, 8, 0).await;
    // d1 is ordered, and reports it failed.
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(
        &rig,
        "d1",
        "1.0",
        &json!({"last_update": {"target": "2.0", "outcome": "failed"}}),
    );
    let first = plan(&rig);
    assert_eq!(
        first.stages[0].members,
        vec![member("d1", proto::PlanHostState::Failed)]
    );

    let rig = eight("plan-status-baking-state", "bake_beats = 2");
    set_limits(&rig, 8, 0).await;
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(&rig, "d1", "2.0", &json!({}));
    assert_eq!(
        plan(&rig).stages[0].members,
        vec![member("d1", proto::PlanHostState::Baking)]
    );
    beat_web(&rig, "d1", "2.0", &json!({}));
    assert_eq!(
        plan(&rig).stages[0].members,
        vec![member("d1", proto::PlanHostState::Done)]
    );
}

/// Proves: when every eligible host runs the version and has baked, the plan is finished, its
/// members are DONE, every stage is entered, and it waits for nothing.
/// Catches: a rolled service still reported as waiting (for a stage that no host is left for).
#[tokio::test]
async fn a_rolled_service_is_finished_and_waits_for_nothing() {
    let rig = rig("plan-status-finished", "");
    farm(&rig, 3, "2.0");
    set(&rig, WEB, "2.0", 1).await;
    let plan = plan(&rig);
    assert!(plan.finished);
    assert_eq!(plan.waiting, i32::from(proto::PlanWait::None));
    assert_eq!((plan.current, plan.eligible), (3, 3));
    assert_eq!(plan.stages.len(), 3);
    for stage in &plan.stages {
        assert!(
            stage
                .members
                .iter()
                .all(|m| m.state == i32::from(proto::PlanHostState::Done)),
            "{stage:?}"
        );
    }
}

/// Proves: a halt and a version marked bad each make the plan wait as HALTED, and a plan that
/// has a free place does not.
/// Catches: a halted service whose plan still says it has a place to order.
#[tokio::test]
async fn a_halt_and_a_bad_version_make_the_plan_wait_as_halted() {
    let rig = eight("plan-status-halt", "");
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::None));
    rig.coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "tainted".to_owned(),
            },
        )
        .expect("halted");
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::Halted));

    let rig = eight("plan-status-bad", "");
    set(&rig, WEB, "2.0", 1).await;
    rig.coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "broken".to_owned(),
            },
        )
        .expect("marked bad");
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::Halted));
}

/// Proves: a paused plan says PAUSED with who paused it and why, and a resume clears the three;
/// the host's gate reason is `paused` while it stands.
/// Catches: a pause the status does not show, or one it keeps showing after the resume.
#[tokio::test]
async fn a_paused_plan_says_who_paused_it_and_why() {
    let rig = eight("plan-status-paused", "");
    set(&rig, WEB, "2.0", 1).await;
    pause(&rig).expect("paused");
    let paused = plan(&rig);
    assert_eq!(paused.waiting, i32::from(proto::PlanWait::Paused));
    assert!(paused.paused);
    assert_eq!(paused.paused_by.as_deref(), Some("operator"));
    assert_eq!(paused.pause_reason.as_deref(), Some("watching the farm"));
    assert_eq!(gate_of(&rig, "d1"), proto::GateReason::Paused);

    resume(&rig).expect("resumed");
    let resumed = plan(&rig);
    assert_eq!(resumed.waiting, i32::from(proto::PlanWait::None));
    assert!(!resumed.paused);
    assert_eq!((resumed.paused_by, resumed.pause_reason), (None, None));
    assert_eq!(gate_of(&rig, "d1"), proto::GateReason::None);
}

/// Proves: within the quiet start after a restart the plan says QUIET, and it says what it holds
/// (a free place) once the start is over.
/// Catches: a restarted coordinator whose plan claims to be ordering.
#[tokio::test]
async fn within_the_quiet_start_the_plan_says_quiet() {
    let mut rig = eight("plan-status-quiet", "");
    set(&rig, WEB, "2.0", 1).await;
    restart(&mut rig, "");
    rig.at(2_005);
    farm(&rig, 8, "1.0");
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::Quiet));
    rig.at(2_031);
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::None));
}

/// Starts `db` rolling: the hosts `d1` to `d3` list `web` and `db`, and `d1`, the first to ask,
/// is ordered the update of `db` to 2.0.
async fn db_rolls(rig: &Rig) {
    set(rig, DB, "2.0", 2).await;
    for i in 1..=3 {
        let host = format!("d{i}");
        let b = beat_of(
            NEW_DAEMON,
            vec![
                service(WEB, "1.0", &json!({})),
                service(DB, "1.0", &json!({})),
            ],
        );
        ask(rig, &host, "daemon", b);
    }
}

/// Proves: while another service's rollout is live, a service whose own is not says
/// ROLLOUT_ELSEWHERE, names the service it waits on (`waiting_on`), and its hosts' gate is
/// `rollout_elsewhere`; with `--live-rollouts 2` the same service does not wait, names none, and
/// its first host is ordered.
/// Catches: a service that waits for a rollout elsewhere though the coordinator allows two, one
/// that does not wait for a live rollout though it allows one, and a wait that does not say
/// which service holds it.
#[tokio::test]
async fn a_service_waits_for_another_services_rollout_unless_two_may_be_live() {
    let rig = rig("plan-status-elsewhere", "");
    db_rolls(&rig).await;
    set(&rig, WEB, "2.0", 1).await;
    let waits = plan(&rig);
    assert_eq!(
        (waits.waiting, waits.waiting_on),
        (
            i32::from(proto::PlanWait::RolloutElsewhere),
            vec![DB.to_owned()]
        )
    );
    assert_eq!(gate_of(&rig, "d3"), proto::GateReason::RolloutElsewhere);

    let mut rig = Rig::new("plan-status-two-live");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_live_rollouts(2);
    rig.at(2_000);
    db_rolls(&rig).await;
    set(&rig, WEB, "2.0", 1).await;
    let free = plan(&rig);
    assert_eq!(
        (free.waiting, free.waiting_on),
        (i32::from(proto::PlanWait::None), vec![])
    );
    let ordered = ask(
        &rig,
        "d3",
        "daemon",
        beat_of(
            NEW_DAEMON,
            vec![
                service(WEB, "1.0", &json!({})),
                service(DB, "1.0", &json!({})),
            ],
        ),
    );
    assert_eq!(ordered, update(WEB, "2.0"));
}

/// Proves: a plan whose first stage waits for an operator says MANUAL_ADVANCE before any host is
/// ordered, with no stage current and the hosts told `advance_wait`; and a plan whose first
/// stage is done and whose second waits for an operator says it too, with the first stage
/// current.
/// Catches: a manual stage reported as filling, so the operator is not told the plan waits for
/// them.
#[tokio::test]
async fn a_plan_waiting_for_an_operator_says_manual_advance() {
    let rig = eight(
        "plan-status-manual-first",
        "stages = [{ percent = 50, advance = \"manual\" }, 100]",
    );
    set(&rig, WEB, "2.0", 1).await;
    let first = plan(&rig);
    assert_eq!(first.waiting, i32::from(proto::PlanWait::ManualAdvance));
    assert_eq!(first.current, 0);
    assert_eq!(gate_of(&rig, "d1"), proto::GateReason::AdvanceWait);

    let rig = eight(
        "plan-status-manual-second",
        "stages = [10, { percent = 50, advance = \"manual\" }, 100]",
    );
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(&rig, "d1", "2.0", &json!({}));
    let second = plan(&rig);
    assert_eq!(second.waiting, i32::from(proto::PlanWait::ManualAdvance));
    assert_eq!(second.current, 1);
    assert_eq!(gate_of(&rig, "d2"), proto::GateReason::AdvanceWait);
}

/// Proves: a host of the current stage that failed the version makes the plan say FAILED_HOST,
/// and the hosts that wait are told `stage_failed`.
/// Catches: a failed host that the status does not name as why the next stage does not start.
#[tokio::test]
async fn a_failed_host_of_the_stage_makes_the_plan_say_failed_host() {
    let rig = eight("plan-status-failed", "");
    set_limits(&rig, 8, 0).await;
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(
        &rig,
        "d1",
        "1.0",
        &json!({"last_update": {"target": "2.0", "outcome": "failed"}}),
    );
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::FailedHost));
    assert_eq!(gate_of(&rig, "d2"), proto::GateReason::StageFailed);
}

/// Proves: when ordering the next host would leave fewer of the role up than `min_up` (8 of 8
/// hosts are to stay up), the plan says BELOW_MIN_UP and the host's gate is `below_min_up`.
/// Catches: `min_up` not shown as the reason the plan orders nobody.
#[tokio::test]
async fn min_up_makes_the_plan_say_below_min_up() {
    let rig = eight("plan-status-min-up", "");
    set_limits(&rig, 8, 8).await;
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::BelowMinUp));
    assert_eq!(gate_of(&rig, "d1"), proto::GateReason::BelowMinUp);
}

/// Proves: a stage whose host is updating says UPDATING, and the hosts that wait are told
/// `stage_wait`.
/// Catches: an updating stage reported as having a place to order.
#[tokio::test]
async fn an_updating_stage_makes_the_plan_say_updating() {
    let rig = eight("plan-status-updating", "");
    set_limits(&rig, 8, 0).await;
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(&rig, "d1", "1.0", &json!({"updating": "2.0"}));
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::Updating));
    assert_eq!(gate_of(&rig, "d2"), proto::GateReason::StageWait);
}

/// Proves: a stage whose hosts are all up at the version and still baking says BAKING, and the
/// hosts that wait are told `baking`.
/// Catches: a baking stage reported as updating or as free.
#[tokio::test]
async fn a_baking_stage_makes_the_plan_say_baking() {
    let rig = eight("plan-status-baking", "bake_beats = 3");
    set_limits(&rig, 8, 0).await;
    assert_eq!(beat_web(&rig, "d1", "1.0", &json!({})), update(WEB, "2.0"));
    beat_web(&rig, "d1", "2.0", &json!({}));
    assert_eq!(plan(&rig).waiting, i32::from(proto::PlanWait::Baking));
    assert_eq!(gate_of(&rig, "d2"), proto::GateReason::Baking);
}

/// Proves: a host that declined the update and waits takes no place: with d1 deferring, d2 holds
/// the first stage and d1 is listed after it, flagged `deferring`, and the plan has a place; with
/// both hosts of a two-host service deferring, the plan says DEFERRING.
/// Catches: a deferring host holding the place of the stage, so the plan stalls on a host that
/// will not take it, or a plan of deferring hosts that says it has a place.
#[tokio::test]
async fn a_deferring_host_takes_no_place_and_a_plan_of_deferring_hosts_says_deferring() {
    let rig = rig("plan-status-deferring", "");
    farm(&rig, 2, "1.0");
    set_limits(&rig, 8, 0).await;
    let declined = json!({"deferred": {
        "target": "2.0", "reason": "drain_bound", "retry_after_secs": 0}});
    assert_eq!(
        beat_web(&rig, "d1", "1.0", &declined),
        ServiceReply::Nothing
    );
    let one = plan(&rig);
    let mut d1 = member("d1", proto::PlanHostState::Waiting);
    d1.deferring = true;
    assert_eq!(
        one.stages,
        vec![
            stage(1, Some(50), 1, waiting(&["d2"])),
            stage(2, Some(100), 2, vec![d1.clone()]),
        ]
    );
    assert_eq!(one.waiting, i32::from(proto::PlanWait::None));

    assert_eq!(
        beat_web(&rig, "d2", "1.0", &declined),
        ServiceReply::Nothing
    );
    let mut d2 = member("d2", proto::PlanHostState::Waiting);
    d2.deferring = true;
    let both = plan(&rig);
    assert_eq!(
        both.stages,
        vec![
            stage(1, Some(50), 1, vec![d1]),
            stage(2, Some(100), 2, vec![d2])
        ]
    );
    assert_eq!(both.waiting, i32::from(proto::PlanWait::Deferring));
}

/// Proves: a service with no desired state has no plan, and none of its hosts has a stage.
/// Catches: an empty plan or a stage number made up for a service nothing is rolling.
#[tokio::test]
async fn a_service_with_no_desired_state_has_no_plan() {
    let rig = eight("plan-status-no-desired", "");
    let status = answer(&rig, WEB);
    assert_eq!(status.plan, None);
    assert_eq!(stages_of(&rig).len(), 8);
    assert!(stages_of(&rig).iter().all(|(_, stage)| stage.is_none()));
}

/// Proves: the stage of a host is the one that lists it, and a host the plan does not count (an
/// operator holds it) has none, while the hosts left are staged without it: with d3 held, seven
/// hosts make stages of 1, 4 and 7, and d3 is in none of them.
/// Catches: a held host given a stage, or the stages sized over hosts that are not rolled.
#[tokio::test]
async fn a_held_host_is_in_no_stage_and_the_others_are_staged_without_it() {
    let rig = eight("plan-status-held", "");
    set(&rig, WEB, "2.0", 1).await;
    hold(&rig, WEB, scope_host("d3")).expect("held");
    let plan = plan(&rig);
    assert_eq!(plan.eligible, 7);
    assert_eq!(
        plan.stages.iter().map(|s| s.hosts).collect::<Vec<_>>(),
        [1, 4, 7]
    );
    let stages = stages_of(&rig);
    let of = |node: &str| {
        stages
            .iter()
            .find(|(id, _)| id == node)
            .expect("the host")
            .1
    };
    assert_eq!(of("d3"), None);
    assert_eq!(
        ["d1", "d2", "d4", "d5", "d6", "d7", "d8"].map(of),
        [1, 2, 2, 2, 3, 3, 3].map(Some)
    );
}

/// Sets up an uninstall of `web` from `d1` while `db` rolls on `d2`: both hosts take installs;
/// `d1` lists `web` and is taken off it with the uninstall; `d2` is ordered the update of `db` to
/// 2.0 and reports it is updating. The rig's clock is past the quiet start.
async fn uninstall_beside_a_rolling_db(rig: &Rig) {
    let both = || vec![runs(WEB, "1.0"), runs(DB, "1.0")];
    told(rig, "d1", both());
    told(rig, "d2", vec![runs(DB, "1.0")]);
    // d1 is no host of db, so db's plan is d2's alone.
    unassign(rig, DB, "d1", "", false).expect("d1 is taken off db");
    unassign(rig, WEB, "d1", "", true).expect("d1 is taken off web, to uninstall");
    set(rig, DB, "2.0", 2).await;
    let mut updating = runs(DB, "1.0");
    updating.updating = Some(version("2.0"));
    told(rig, "d2", vec![runs(DB, "1.0")]);
    told(rig, "d2", vec![updating]);
}

/// Proves: an uninstall of a service whose rollout is not live waits as `rollout_elsewhere` while
/// another service's rollout is live, and goes once that one is not (the host reports the
/// version): `db` rolls on `d2`, `web` is to be uninstalled from `d1`.
/// Catches: an uninstall that runs through another service's rollout, or one that stays held
/// after the rollout is done.
#[tokio::test]
async fn an_uninstall_waits_for_another_services_rollout_and_goes_when_it_is_done() {
    let rig = rig("plan-status-uninstall", "");
    uninstall_beside_a_rolling_db(&rig).await;
    let both = || vec![runs(WEB, "1.0"), runs(DB, "1.0")];
    assert_eq!(order_to(&rig, "d1", both()), None);
    assert_eq!(
        why_of(&rig, WEB, "d1"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );

    // d2 reports the version: db's only host has rolled, so its rollout is not live.
    told(&rig, "d2", vec![runs(DB, "2.0")]);
    assert_eq!(why_of(&rig, WEB, "d1"), None);
    assert_eq!(
        order_to(&rig, "d1", both()),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
}

/// Proves: with `--live-rollouts 2` the same uninstall is not held by one other service's
/// rollout.
/// Catches: the uninstall gate counting other rollouts against a limit of one whatever the
/// flag says.
#[tokio::test]
async fn an_uninstall_is_not_held_when_two_rollouts_may_be_live() {
    let mut rig = Rig::new("plan-status-uninstall-two");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_live_rollouts(2);
    rig.at(2_000);
    uninstall_beside_a_rolling_db(&rig).await;
    let both = || vec![runs(WEB, "1.0"), runs(DB, "1.0")];
    assert_eq!(
        order_to(&rig, "d1", both()),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
}

/// Proves: a canary that later stages fold into keeps no percent: `canary = { count = 3 }` with
/// the default plan on eight hosts is a stage of 3 hosts (1 and 10 percent fold into it) with no
/// percent, then 50 and 100 percent.
/// Catches: the canary stage shown as the percent of a stage folded into it.
#[tokio::test]
async fn a_canary_that_later_stages_fold_into_keeps_no_percent() {
    let rig = eight("plan-status-canary-folded", "canary = { count = 3 }");
    set(&rig, WEB, "2.0", 1).await;
    let sizes: Vec<(Option<u32>, u32)> = plan(&rig)
        .stages
        .iter()
        .map(|s| (s.percent, s.hosts))
        .collect();
    assert_eq!(sizes, [(None, 3), (Some(50), 4), (Some(100), 8)]);
}

/// Proves: a service with a desired version and no eligible host (none heard, or every one
/// down) is not finished: it has no stage, waits for nothing of its own, and still says when it
/// is paused.
/// Catches: a fleet with no live host read as a finished rollout.
#[tokio::test]
async fn a_plan_with_no_eligible_host_is_not_finished() {
    let rig = rig("plan-status-empty", "");
    set(&rig, WEB, "2.0", 1).await;
    let empty = plan(&rig);
    assert_eq!(
        (
            empty.finished,
            empty.eligible,
            empty.stages.len(),
            empty.current
        ),
        (false, 0, 0, 0)
    );
    assert_eq!(empty.waiting, proto::PlanWait::None as i32);
    pause(&rig).expect("paused");
    assert_eq!(plan(&rig).waiting, proto::PlanWait::Paused as i32);
}
