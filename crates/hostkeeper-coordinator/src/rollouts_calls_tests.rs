//! The operator's `Rollouts.Advance`, `Rollouts.Pause` and `Rollouts.Resume`
//! ([`crate::rollouts`]) when they act, made directly on a real coordinator over the files of a
//! scratch directory: the audit entry each writes, what the plan and the hosts' next heartbeats
//! show after, and what a pause stands across. What they refuse is in
//! [`crate::rollouts_refusal_tests`]; the farm they run on in [`crate::rollouts_calls_support`].

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::ServiceReply;
use serde_json::json;

use crate::assign_support::why_of;
use crate::decide::{Gate, Why};
use crate::marks_support::{WEB, operator, set, update};
use crate::rollouts::{Pause, Rollout};
use crate::rollouts_calls_support::{
    MANUAL_FIRST, REASON, STAGES, advance, all_in, entry_of, farm, halts_json, kept, last_seq,
    newest, pause, pause_as, resume, resume_as, sizes, snapshot, stage_hosts, waiting,
};
use crate::testutil::version;

// ---- Advance when the plan waits ----------------------------------------------------------

/// Proves: a done first stage before a manual second one waits: no host of the second stage is
/// ordered and each is told `advance_wait`; `Advance` then enters stage 2: the audit entry is
/// `rollout_advance` by the operator's token name, its reason names the service, the version and
/// "stage 2 of 3 (4 hosts)" and then the operator's reason; the answer's entry is the ring's
/// newest, and its plan is at stage 2 of sizes 2, 4 and 8, with the first stage's hosts done and
/// nothing to wait for; the position kept is stage 2 with the two done hosts; the next heartbeats
/// order the two hosts the stage adds, and no more; a second Advance is `not_waiting`.
///
/// Catches: a manual stage entered with no audit line, an entry under the coordinator's name, a
/// reason that does not say which stage was entered, or an Advance that does not let the stage's
/// hosts be ordered.
#[tokio::test]
async fn advance_enters_a_manual_stage_after_a_done_one() {
    let (rig, mut farm) = waiting("rc-advance").await;
    for host in ["d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(why_of(&rig, WEB, host), Some(Why::Gate(Gate::AdvanceWait)));
    }
    let seq = last_seq(&rig);

    let answer = advance(&rig).expect("advanced");

    let reason = format!("web 2.0: stage 2 of 3 (4 hosts): {REASON}");
    assert_eq!(
        answer.entry,
        Some(entry_of(seq + 1, "rollout_advance", &reason))
    );
    let written = newest(&rig);
    assert_eq!(
        (written.seq, written.by.as_str(), written.action.as_str()),
        (seq + 1, "operator", "rollout_advance")
    );
    assert_eq!(written.service.as_deref(), Some(WEB));
    assert_eq!(written.reason.as_deref(), Some(reason.as_str()));
    let logged = rig.coordinator.ops().audit_log().expect("the log");
    assert_eq!(logged.last(), Some(&written), "the audit file has it too");

    let plan = answer.plan.expect("a plan");
    assert_eq!(plan.version, "2.0");
    assert_eq!(plan.current, 2);
    assert_eq!(plan.eligible, 8);
    assert_eq!(sizes(&plan), [2, 4, 8]);
    assert_eq!(plan.waiting(), proto::PlanWait::None);
    assert!(!plan.paused && !plan.finished);
    assert_eq!(
        stage_hosts(&plan, 1),
        all_in(&["d1", "d2"], proto::PlanHostState::Done)
    );
    assert_eq!(
        stage_hosts(&plan, 2),
        all_in(&["d3", "d4"], proto::PlanHostState::Waiting)
    );
    assert_eq!(
        kept(&rig),
        Some(Rollout {
            version: Some(version("2.0")),
            entered: 2,
            done: ["d1", "d2"].map(str::to_owned).into(),
            paused: None,
        })
    );

    assert_eq!(farm.beat(&rig, "d3"), update(WEB, "2.0"));
    assert_eq!(farm.beat(&rig, "d4"), update(WEB, "2.0"));
    assert_eq!(farm.beat(&rig, "d5"), ServiceReply::Nothing);
    assert_eq!(why_of(&rig, WEB, "d5"), Some(Why::Gate(Gate::StageWait)));
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
}

/// Proves: a plan whose first stage is manual orders no host at all before the Advance (every
/// host is told `advance_wait`, and nothing is kept); the Advance enters stage 1 of 2 ("web 2.0:
/// stage 1 of 2 (4 hosts): " and the operator's reason), the plan answers `current` 1 with no
/// host ordered yet, and the next heartbeats order the stage's four hosts, two at a time under the
/// daemon role's `max_updating`, and no host of the second stage.
///
/// Catches: a first manual stage that starts by itself, or an Advance of a plan that has not
/// entered any stage that is refused.
#[tokio::test]
async fn advance_enters_a_manual_first_stage_before_any_host_is_ordered() {
    let (rig, mut farm) = farm("rc-advance-first", MANUAL_FIRST).await;
    assert_eq!(farm.round(&rig), Vec::<String>::new(), "no host is ordered");
    for host in ["d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(why_of(&rig, WEB, host), Some(Why::Gate(Gate::AdvanceWait)));
    }
    assert_eq!(kept(&rig), None);
    let seq = last_seq(&rig);

    let answer = advance(&rig).expect("advanced");

    let reason = format!("web 2.0: stage 1 of 2 (4 hosts): {REASON}");
    assert_eq!(
        answer.entry,
        Some(entry_of(seq + 1, "rollout_advance", &reason))
    );
    let plan = answer.plan.expect("a plan");
    assert_eq!(plan.current, 1);
    assert_eq!(sizes(&plan), [4, 8]);
    assert_eq!(plan.waiting(), proto::PlanWait::None);
    assert_eq!(
        stage_hosts(&plan, 1),
        all_in(&["d1", "d2", "d3", "d4"], proto::PlanHostState::Waiting)
    );
    assert_eq!(
        kept(&rig),
        Some(Rollout {
            version: Some(version("2.0")),
            entered: 1,
            done: std::collections::BTreeSet::new(),
            paused: None,
        })
    );

    assert_eq!(farm.round(&rig), ["d1", "d2"], "two hold the daemon slots");
    assert_eq!(farm.round(&rig), ["d3", "d4"]);
    assert_eq!(
        farm.round(&rig),
        ["d5", "d6"],
        "the automatic second stage follows once the first has baked"
    );
    assert_eq!(farm.running.values().filter(|v| *v == "2.0").count(), 6);
}

// ---- Pause and Resume ---------------------------------------------------------------------

/// Proves: `Pause` in the middle of the first stage writes `rollout_pause` by the operator's token
/// with the operator's reason as it stands; the answer's plan is paused, by `operator`, with the
/// reason, waiting `paused`, and shows the two ordered hosts updating; every host that has not
/// started is told `paused`; the two running updates finish (both hosts run 2.0 at their next
/// heartbeat and are not ordered anything) and `Resume` shows them done; no other host is ordered
/// while it is paused.
///
/// Catches: a pause that undoes or blocks an update already ordered, a host not started that is
/// still ordered, or a pause whose answer omits who paused and why.
#[tokio::test]
async fn pause_tells_hosts_not_started_paused_and_lets_a_running_update_finish() {
    let (rig, mut farm) = farm("rc-pause", STAGES).await;
    assert_eq!(farm.round(&rig), ["d1", "d2"]);
    let seq = last_seq(&rig);

    let answer = pause(&rig).expect("paused");

    assert_eq!(
        answer.entry,
        Some(entry_of(seq + 1, "rollout_pause", REASON))
    );
    let written = newest(&rig);
    assert_eq!(
        (written.seq, written.by.as_str(), written.action.as_str()),
        (seq + 1, "operator", "rollout_pause")
    );
    assert_eq!(written.reason.as_deref(), Some(REASON));
    let plan = answer.plan.expect("a plan");
    assert!(plan.paused);
    assert_eq!(plan.paused_by.as_deref(), Some("operator"));
    assert_eq!(plan.pause_reason.as_deref(), Some(REASON));
    assert_eq!(plan.waiting(), proto::PlanWait::Paused);
    assert_eq!(plan.current, 1);
    assert_eq!(
        stage_hosts(&plan, 1),
        all_in(&["d1", "d2"], proto::PlanHostState::Updating)
    );
    for host in ["d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(
            why_of(&rig, WEB, host),
            Some(Why::Gate(Gate::Paused)),
            "{host}"
        );
    }

    assert_eq!(farm.round(&rig), Vec::<String>::new(), "nobody is ordered");
    assert_eq!(farm.round(&rig), Vec::<String>::new());
    assert_eq!(farm.running["d1"], "2.0");
    assert_eq!(farm.running["d2"], "2.0");
    assert_eq!(farm.running.values().filter(|v| *v == "2.0").count(), 2);
    for host in ["d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(
            why_of(&rig, WEB, host),
            Some(Why::Gate(Gate::Paused)),
            "{host}"
        );
    }

    let resumed = resume(&rig).expect("resumed");
    let plan = resumed.plan.expect("a plan");
    assert_eq!(
        stage_hosts(&plan, 1),
        all_in(&["d1", "d2"], proto::PlanHostState::Done),
        "the updates finished under the pause"
    );
}

/// Proves: a second `Pause` is `already_paused`, nothing is written (the files, the ops state and
/// the audit log are as they were) and the first pause's operator and reason stand.
#[tokio::test]
async fn a_second_pause_is_already_paused_and_changes_nothing() {
    let (rig, _farm) = waiting("rc-pause-twice").await;
    pause(&rig).expect("paused");
    let before = snapshot(&rig);
    let logged = rig.coordinator.ops().audit_log().expect("the log");
    assert_eq!(
        pause_as(&rig, &operator(), WEB, "again, for another reason"),
        Err(proto::RolloutsRefusal::AlreadyPaused)
    );
    assert_eq!(snapshot(&rig), before);
    assert_eq!(rig.coordinator.ops().audit_log().expect("the log"), logged);
    assert_eq!(
        kept(&rig).and_then(|rollout| rollout.paused),
        Some(Pause {
            by: "operator".to_owned(),
            reason: REASON.to_owned(),
        })
    );
}

/// Proves: `Resume` writes `rollout_resume` by the operator's token with the operator's reason;
/// the answer's plan is not paused (no operator, no reason) and goes on from where it stood: with
/// the default plan paused before any host started, nobody is ordered while paused, and after the
/// Resume the first host to ask is ordered the first stage.
#[tokio::test]
async fn resume_lets_the_plan_go_on() {
    let (rig, mut farm) = farm("rc-resume", "").await;
    pause(&rig).expect("paused");
    assert_eq!(farm.round(&rig), Vec::<String>::new());
    let seq = last_seq(&rig);

    let answer = resume_as(&rig, &operator(), WEB, "all clear").expect("resumed");

    assert_eq!(
        answer.entry,
        Some(entry_of(seq + 1, "rollout_resume", "all clear"))
    );
    assert_eq!(newest(&rig).action, "rollout_resume");
    let plan = answer.plan.expect("a plan");
    assert!(!plan.paused);
    assert_eq!((&plan.paused_by, &plan.pause_reason), (&None, &None));
    assert_eq!(plan.waiting(), proto::PlanWait::None);
    assert_eq!(sizes(&plan), [1, 4, 8]);
    assert_eq!(plan.current, 1);

    assert_eq!(farm.round(&rig), ["d1"], "the first stage holds one host");
}

/// Proves: `Resume` of a plan that is not paused is `not_paused`, before any pause and after one
/// is lifted, and nothing is written.
#[tokio::test]
async fn resume_of_a_plan_that_is_not_paused_is_not_paused() {
    let (rig, _farm) = waiting("rc-not-paused").await;
    let before = snapshot(&rig);
    assert_eq!(resume(&rig), Err(proto::RolloutsRefusal::NotPaused));
    assert_eq!(snapshot(&rig), before);
    pause(&rig).expect("paused");
    resume(&rig).expect("resumed");
    let after = snapshot(&rig);
    assert_eq!(resume(&rig), Err(proto::RolloutsRefusal::NotPaused));
    assert_eq!(snapshot(&rig), after);
}

/// Proves: a pause belongs to the service, not to a version: after CI sets 3.0 the plan is still
/// paused (the position kept still holds the pause, and every host is told `paused`), nobody is
/// ordered 3.0, and after `Resume` the answer's plan is for 3.0 and starts at its first stage.
///
/// Catches: a pause dropped by a new desired version, which would let a service an operator
/// stopped roll on.
#[tokio::test]
async fn a_pause_stands_across_a_new_desired_version() {
    let (rig, mut farm) = waiting("rc-pause-version").await;
    pause(&rig).expect("paused");
    set(&rig, WEB, "3.0", 3).await;

    assert_eq!(
        kept(&rig).and_then(|rollout| rollout.paused),
        Some(Pause {
            by: "operator".to_owned(),
            reason: REASON.to_owned(),
        })
    );
    for host in ["d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(
            why_of(&rig, WEB, host),
            Some(Why::Gate(Gate::Paused)),
            "{host}"
        );
    }
    assert_eq!(farm.round(&rig), Vec::<String>::new());

    let resumed = resume(&rig).expect("resumed");
    let plan = resumed.plan.expect("a plan");
    assert_eq!(plan.version, "3.0");
    assert!(!plan.paused);
    assert_eq!(plan.current, 1);
    assert_eq!(
        farm.round(&rig),
        ["d1", "d2"],
        "3.0 starts from its first stage"
    );
    assert_eq!(farm.running["d1"], "3.0");
}

/// Proves: `Resume` leaves no empty record: after a `Pause` and a `Resume` on a service whose plan
/// kept nothing else, the position is gone from memory and the halts file has no `rollouts` entry
/// for the service (while the pause was in force the file held exactly the pause).
///
/// Catches: a resume that leaves `{}` for the service in the halts file.
#[tokio::test]
async fn resume_leaves_no_empty_record() {
    let (rig, _farm) = farm("rc-no-empty", STAGES).await;
    pause(&rig).expect("paused");
    assert_eq!(
        halts_json(&rig)["rollouts"],
        json!({"web": {"paused": {"by": "operator", "reason": REASON}}})
    );
    resume(&rig).expect("resumed");
    assert_eq!(kept(&rig), None);
    assert!(
        rig.coordinator
            .ops()
            .read(|state| state.rollouts.is_empty()),
        "no position kept"
    );
    assert_eq!(
        halts_json(&rig).get("rollouts").and_then(|r| r.get(WEB)),
        None
    );
}

/// Proves: `Resume` removes only the pause: the position the plan had entered (stage 2, the two
/// hosts done) is as `Advance` wrote it, in memory and in the halts file.
#[tokio::test]
async fn resume_keeps_the_position_the_pause_stood_over() {
    let (rig, _farm) = waiting("rc-keeps").await;
    advance(&rig).expect("stage 2");
    let position = kept(&rig).expect("a position");
    let in_file = halts_json(&rig)["rollouts"][WEB].clone();
    assert_eq!(
        in_file,
        json!({"version": "2.0", "entered": 2, "done": ["d1", "d2"]})
    );
    pause(&rig).expect("paused");
    assert_eq!(
        kept(&rig).and_then(|r| r.paused.clone()).map(|p| p.by),
        Some("operator".to_owned())
    );
    resume(&rig).expect("resumed");
    assert_eq!(kept(&rig), Some(position));
    assert_eq!(halts_json(&rig)["rollouts"][WEB], in_file);
}

/// Proves: a Pause, a Resume and an Advance are each one line of the audit file, numbered one
/// after the other in the order made, by the operator's token name, and the answers carry those
/// numbers.
#[tokio::test]
async fn each_call_is_one_line_of_the_audit_file_in_the_order_made() {
    let (rig, _farm) = waiting("rc-audit-order").await;
    let first = pause(&rig).expect("paused");
    let second = resume(&rig).expect("resumed");
    let third = advance(&rig).expect("advanced");
    let logged = rig.coordinator.ops().audit_log().expect("the log");
    let tail: Vec<(u64, String, String)> = logged
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(|e| (e.seq, e.by.clone(), e.action.clone()))
        .collect();
    let seq = first.entry.as_ref().expect("an entry").seq;
    assert_eq!(
        tail,
        [
            (seq, "operator".to_owned(), "rollout_pause".to_owned()),
            (seq + 1, "operator".to_owned(), "rollout_resume".to_owned()),
            (seq + 2, "operator".to_owned(), "rollout_advance".to_owned()),
        ]
    );
    assert_eq!(second.entry.expect("an entry").seq, seq + 1);
    assert_eq!(third.entry.expect("an entry").seq, seq + 2);
}
