//! What the operator's `Rollouts.Advance`, `Rollouts.Pause` and `Rollouts.Resume`
//! ([`crate::rollouts`]) refuse, each with its exact refusal and nothing changed: a caller without
//! the operator token, an unknown service, a bad reason, a service with no desired version, a plan
//! that does not wait for an operator, and an audit file or an ops file that takes nothing.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::ServiceBeat;
use serde_json::json;

use crate::assign_support::why_of;
use crate::audit_file::{self, Appender, AuditLog, DEFAULT_ROTATION};
use crate::decide::{Gate, Why};
use crate::desired::StoreError;
use crate::marks_support::{WEB, ask, operator, restart, rig};
use crate::operator::Caller;
use crate::ops::OpsError;
use crate::ops_file;
use crate::rollouts::{Pause, Rollout};
use crate::rollouts_calls_support::{
    DAEMON, REASON, STAGES, advance, advance_as, beat_at, entry_of, farm, farm_over, kept, newest,
    pause, pause_as, resume, resume_as, snapshot, waiting,
};
use crate::testutil::{OPERATOR, Rig, ops_of};

// ---- who may call, and what they must name ------------------------------------------------

/// Proves: with no token `Rollouts.Advance`, `Pause` and `Resume` are each refused
/// `unauthorized`, and with a valid CI token `forbidden`, whatever the service and the reason say
/// (the caller is judged before the service and the reason), and the files and the ops state are
/// byte for byte as they were.
///
/// Catches: a call that reads the service or the reason before the caller, or a CI token that
/// reaches a plan.
#[tokio::test]
async fn only_the_operator_token_may_advance_pause_or_resume() {
    let (rig, _farm) = waiting("rc-callers").await;
    let before = snapshot(&rig);
    for (service, reason) in [(WEB, REASON), ("nosuch", ""), (WEB, "")] {
        for (caller, advance_wants, pause_wants, resume_wants) in [
            (
                Caller::Anonymous,
                proto::RolloutsRefusal::Unauthorized,
                proto::RolloutsRefusal::Unauthorized,
                proto::RolloutsRefusal::Unauthorized,
            ),
            (
                Caller::Ci,
                proto::RolloutsRefusal::Forbidden,
                proto::RolloutsRefusal::Forbidden,
                proto::RolloutsRefusal::Forbidden,
            ),
        ] {
            assert_eq!(
                advance_as(&rig, &caller, service, reason),
                Err(advance_wants),
                "advance {caller:?} {service:?} {reason:?}"
            );
            assert_eq!(
                pause_as(&rig, &caller, service, reason),
                Err(pause_wants),
                "pause {caller:?} {service:?} {reason:?}"
            );
            assert_eq!(
                resume_as(&rig, &caller, service, reason),
                Err(resume_wants),
                "resume {caller:?} {service:?} {reason:?}"
            );
        }
    }
    assert_eq!(snapshot(&rig), before);
    advance(&rig).expect("the operator's token, on the same state, advances");
}

/// Proves: a service the coordinator does not manage is `unknown_service` for all three calls,
/// even with a reason that is empty (the service is judged before the reason), and nothing
/// changes.
#[tokio::test]
async fn a_service_the_coordinator_does_not_manage_is_unknown_service() {
    let (rig, _farm) = waiting("rc-unknown").await;
    let before = snapshot(&rig);
    for reason in [REASON, ""] {
        assert_eq!(
            advance_as(&rig, &operator(), "nosuch", reason),
            Err(proto::RolloutsRefusal::UnknownService)
        );
        assert_eq!(
            pause_as(&rig, &operator(), "nosuch", reason),
            Err(proto::RolloutsRefusal::UnknownService)
        );
        assert_eq!(
            resume_as(&rig, &operator(), "nosuch", reason),
            Err(proto::RolloutsRefusal::UnknownService)
        );
    }
    assert_eq!(snapshot(&rig), before);
}

/// Proves: a reason that is empty, only blanks, holds a control character (a new line, a tab, a
/// bell, a NUL) or is 257 bytes is `invalid_request` for all three calls and changes nothing; it
/// is judged before the service has a desired version (an Advance of `db`, which has none, is
/// `invalid_request` and not `no_desired`).
///
/// Catches: a reason taken unchecked into the audit log.
#[tokio::test]
async fn a_reason_that_is_empty_or_holds_a_control_character_is_invalid_request() {
    let (rig, _farm) = waiting("rc-reason").await;
    let before = snapshot(&rig);
    let long = "x".repeat(257);
    for reason in [
        "",
        "   ",
        "two\nlines",
        "a\ttab",
        "bell\u{7}",
        "nul\u{0}",
        long.as_str(),
    ] {
        assert_eq!(
            advance_as(&rig, &operator(), WEB, reason),
            Err(proto::RolloutsRefusal::InvalidRequest),
            "advance {reason:?}"
        );
        assert_eq!(
            pause_as(&rig, &operator(), WEB, reason),
            Err(proto::RolloutsRefusal::InvalidRequest),
            "pause {reason:?}"
        );
        assert_eq!(
            resume_as(&rig, &operator(), WEB, reason),
            Err(proto::RolloutsRefusal::InvalidRequest),
            "resume {reason:?}"
        );
    }
    assert_eq!(
        advance_as(&rig, &operator(), "db", ""),
        Err(proto::RolloutsRefusal::InvalidRequest)
    );
    assert_eq!(snapshot(&rig), before);
}

/// Proves: a reason of exactly 256 bytes is taken, and the audit entry carries it whole; the
/// reason is trimmed of the blanks around it.
#[tokio::test]
async fn a_reason_of_256_bytes_is_taken_whole_and_blanks_around_it_are_trimmed() {
    let (rig, _farm) = waiting("rc-reason-edge").await;
    let long = "y".repeat(256);
    let paused = pause_as(&rig, &operator(), WEB, &format!("  {long}  ")).expect("paused");
    assert_eq!(paused.entry.expect("an entry").reason, Some(long.clone()));
    let resumed = resume_as(&rig, &operator(), WEB, "\tdone\t").expect("resumed");
    assert_eq!(
        resumed.entry.expect("an entry").reason.as_deref(),
        Some("done")
    );
}

/// Proves: a service with no desired version has no plan to advance: `no_desired`, nothing
/// changes; its plan can still be paused and resumed (the pause belongs to the service), and the
/// answers carry no plan.
#[tokio::test]
async fn advance_of_a_service_with_no_desired_state_is_no_desired() {
    let rig = rig("rc-no-desired", STAGES);
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NoDesired));
    assert_eq!(snapshot(&rig), before);

    let paused = pause(&rig).expect("a pause stands without a version");
    assert_eq!(paused.plan, None);
    assert_eq!(
        kept(&rig),
        Some(Rollout {
            paused: Some(Pause {
                by: "operator".to_owned(),
                reason: REASON.to_owned(),
            }),
            ..Rollout::default()
        })
    );
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NoDesired));
    let resumed = resume(&rig).expect("resumed");
    assert_eq!(resumed.plan, None);
    assert_eq!(kept(&rig), None);
}

// ---- Advance when the plan does not wait --------------------------------------------------

/// Proves: while the stage is not done (its hosts are ordered, then updating, and the first
/// stage's bake not over) `Advance` is `not_waiting`, and the files, the ops state and the audit
/// log are as they were; the audit log has no entry of it.
///
/// Catches: an Advance that enters a manual stage before the stage before it is done.
#[tokio::test]
async fn advance_while_the_stage_is_not_done_is_not_waiting() {
    let (rig, mut farm) = farm("rc-not-done", STAGES).await;
    assert_eq!(
        advance(&rig),
        Err(proto::RolloutsRefusal::NotWaiting),
        "idle"
    );
    assert_eq!(farm.round(&rig), ["d1", "d2"]);
    let before = snapshot(&rig);
    let logged = rig.coordinator.ops().audit_log().expect("the log");
    assert_eq!(
        advance(&rig),
        Err(proto::RolloutsRefusal::NotWaiting),
        "d1 and d2 are ordered"
    );
    assert_eq!(snapshot(&rig), before);
    assert_eq!(rig.coordinator.ops().audit_log().expect("the log"), logged);
    assert_eq!(kept(&rig), None);

    assert_eq!(farm.round(&rig), Vec::<String>::new());
    advance(&rig).expect("the stage is done: the next waits for the operator");
}

/// Proves: when the stage after a done one enters by itself (the default plan, 1, 4 and 8
/// hosts), `Advance` is `not_waiting`, before and after the first stage is done, and nothing is
/// written by it.
///
/// Catches: an Advance that skips a stage an `auto` plan enters by itself.
#[tokio::test]
async fn advance_when_the_next_stage_is_auto_is_not_waiting() {
    let (rig, mut farm) = farm("rc-next-auto", "").await;
    assert_eq!(farm.round(&rig), ["d1"]);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(
        farm.round(&rig),
        ["d2", "d3"],
        "the second stage at once, under the daemon role's max_updating"
    );
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
}

/// Proves: with the second stage entered and the third (automatic) one not yet, `Advance` is
/// `not_waiting`: the plan does not wait for an operator at an automatic stage.
#[tokio::test]
async fn advance_after_the_last_manual_stage_is_not_waiting() {
    let (rig, mut farm) = waiting("rc-after-manual").await;
    advance(&rig).expect("stage 2");
    assert_eq!(farm.round(&rig), ["d3", "d4"]);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    farm.round(&rig);
    farm.round(&rig);
    assert_eq!(
        advance(&rig),
        Err(proto::RolloutsRefusal::NotWaiting),
        "stage 2 is done and stage 3 enters by itself"
    );
}

/// Proves: while an operator has paused the plan, `Advance` is `not_waiting` though the stage is
/// done and the next waits for it, and writes nothing; after `Resume` the same Advance enters the
/// stage.
#[tokio::test]
async fn advance_while_paused_is_not_waiting() {
    let (rig, _farm) = waiting("rc-paused").await;
    pause(&rig).expect("paused");
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
    resume(&rig).expect("resumed");
    advance(&rig).expect("the stage is entered once the pause is lifted");
}

/// Proves: while the service is halted by `Releases.Halt`, and while it is halted by a host's
/// halt report, `Advance` is `not_waiting` and nothing is written by it; once an operator clears
/// the halt the Advance enters the stage.
#[tokio::test]
async fn advance_while_halted_is_not_waiting() {
    let (rig, _farm) = waiting("rc-halted").await;
    rig.coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "hold everything".to_owned(),
            },
        )
        .expect("halted");
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"web","reason":"checked"}"#,
        )
        .expect("cleared");
    advance(&rig).expect("the stage is entered once the halt is cleared");

    let (rig, mut farm) = waiting("rc-halt-report").await;
    let mut report = beat_at("1.0");
    report.services = Some(
        serde_json::from_value::<Vec<ServiceBeat>>(json!([{
            "name": WEB, "version": "1.0", "health": "healthy",
            "last_update": {"target": "2.0", "outcome": "failed", "phase": "halt",
                "reason": "disk_tainted"}
        }]))
        .expect("a service beat"),
    );
    ask(&rig, "d3", DAEMON, report);
    assert_eq!(
        rig.coordinator
            .ops()
            .marks(WEB)
            .halt
            .map(|halt| halt.node_id),
        Some("d3".to_owned())
    );
    farm.round(&rig);
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
}

/// Proves: while the desired version is marked bad, `Advance` is `not_waiting` and writes
/// nothing; once the mark is lifted the Advance enters the stage.
#[tokio::test]
async fn advance_while_the_version_is_marked_bad_is_not_waiting() {
    let (rig, _farm) = waiting("rc-bad").await;
    rig.coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "bad build".to_owned(),
            },
        )
        .expect("marked");
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
    rig.coordinator
        .unmark_bad(
            &operator(),
            proto::UnmarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "it was fine".to_owned(),
            },
        )
        .expect("unmarked");
    advance(&rig).expect("the stage is entered once the mark is lifted");
}

/// Proves: within the coordinator's quiet start after a restart, `Advance` is `not_waiting` and
/// writes nothing though the hosts it heard say the stage is done; once the quiet start is over
/// (the rig's clock 31 seconds on, the hosts still live) the same Advance enters the stage.
#[tokio::test]
async fn advance_within_the_quiet_start_is_not_waiting() {
    let (mut rig, mut farm) = waiting("rc-quiet").await;
    restart(&mut rig, STAGES);
    farm.round(&rig);
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
    rig.at(2_031);
    let answer = advance(&rig).expect("the quiet start is over");
    assert_eq!(answer.plan.expect("a plan").current, 2);
}

/// Proves: once every host runs the version and has baked, `Advance` is `not_waiting`: there is
/// no stage left to enter.
#[tokio::test]
async fn advance_when_every_host_has_rolled_is_not_waiting() {
    let (rig, mut farm) = waiting("rc-finished").await;
    advance(&rig).expect("stage 2");
    farm.finish(&rig, "2.0", 12);
    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::NotWaiting));
    assert_eq!(snapshot(&rig), before);
}

// ---- audit failures -----------------------------------------------------------------------

/// An appender that fails with a full disk while `down` is set, and otherwise appends to the audit
/// file of the ops file at `ops` as the store's own would.
fn switchable(ops: &std::path::Path, down: &Arc<AtomicBool>) -> Appender {
    let log = AuditLog::open(audit_file::path_of(ops), DEFAULT_ROTATION).expect("opened");
    let down = Arc::clone(down);
    Box::new(move |line| {
        if down.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        log.append(line)
    })
}

/// Proves: with the audit file taking no lines, `Advance` and `Resume` are each refused
/// `audit_unavailable` and change nothing (the files and the ops state are byte for byte as they
/// were), while `Pause` stands, written memory first: the plan is paused, the ring holds its
/// entry and the audit file does not; once the file takes lines again the same Resume and Advance
/// are made, so the refusals were the audit's.
///
/// Catches: an Advance or a Resume that applies its change and only logs the failed append, a
/// Pause refused for its audit (the operator's way to stop a rollout on a full disk), or a
/// refusal reported as `store_failed`.
#[tokio::test]
async fn advance_and_resume_refuse_when_the_audit_cannot_be_appended_but_pause_stands() {
    let down = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&down);
    let (rig, mut farm) = farm_over("rc-audit-down", STAGES, move |dir| {
        let ops = dir.path().join("desired.json.ops");
        ops_of(dir).with_append(switchable(&ops, &switch))
    })
    .await;
    // The disk fills after the first stage is done and the second waits.
    assert_eq!(farm.round(&rig), ["d1", "d2"]);
    assert_eq!(farm.round(&rig), Vec::<String>::new());
    down.store(true, Ordering::SeqCst);

    let before = snapshot(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::AuditUnavailable));
    assert_eq!(snapshot(&rig), before);
    assert_eq!(kept(&rig), None);

    let answer = pause(&rig).expect("a pause stands without its audit line");
    assert!(answer.plan.expect("a plan").paused);
    let ringed = newest(&rig);
    assert_eq!(
        (
            ringed.action.as_str(),
            ringed.by.as_str(),
            ringed.reason.as_deref()
        ),
        ("rollout_pause", "operator", Some(REASON))
    );
    assert_eq!(
        answer.entry,
        Some(entry_of(ringed.seq, "rollout_pause", REASON))
    );
    let logged: Vec<String> = rig
        .coordinator
        .ops()
        .audit_log()
        .expect("the log")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        !logged.contains(&"rollout_pause".to_owned()),
        "the audit file has no line of it: {logged:?}"
    );
    assert!(kept(&rig).expect("kept").paused.is_some());

    let paused = snapshot(&rig);
    assert_eq!(resume(&rig), Err(proto::RolloutsRefusal::AuditUnavailable));
    assert_eq!(snapshot(&rig), paused, "still paused");

    down.store(false, Ordering::SeqCst);
    resume(&rig).expect("resumed once the audit takes lines");
    advance(&rig).expect("advanced once the audit takes lines");
    let logged: Vec<String> = rig
        .coordinator
        .ops()
        .audit_log()
        .expect("the log")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert_eq!(
        logged.iter().rev().take(2).collect::<Vec<_>>(),
        ["rollout_advance", "rollout_resume"]
    );
}

// ---- a store that cannot be written -------------------------------------------------------

/// Proves: with the ops file unwritable, `Advance` and `Resume` are each refused `store_failed`
/// and the ops state in memory, the ops file and the halts file are as they were (the plan is
/// still waiting, the pause still stands); once the file can be written the same two calls are
/// made, so the refusal was the store's.
///
/// Catches: an Advance or a Resume that stands in memory after its write failed, or a failed
/// write reported as `audit_unavailable`.
#[tokio::test]
async fn advance_and_resume_refuse_store_failed_and_change_nothing_when_the_write_fails() {
    let fail = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&fail);
    let (rig, mut farm) = farm_over("rc-store-fails", STAGES, move |dir| {
        let path = dir.path().join("desired.json.ops");
        ops_of(dir).with_write(Box::new(move |state| {
            if switch.load(Ordering::SeqCst) {
                return Err(OpsError::Io(StoreError::Io {
                    path: "ops".into(),
                    source: io::Error::other("disk full"),
                }));
            }
            ops_file::write(&path, state)
        }))
    })
    .await;
    assert_eq!(farm.round(&rig), ["d1", "d2"]);
    assert_eq!(farm.round(&rig), Vec::<String>::new());
    let memory_and_files = |rig: &Rig| {
        let (mut files, state) = snapshot(rig);
        // The audit line of a change is appended before its state is written: it is not part of
        // what a refusal must leave.
        files.pop();
        (files, state)
    };

    fail.store(true, Ordering::SeqCst);
    let before = memory_and_files(&rig);
    assert_eq!(advance(&rig), Err(proto::RolloutsRefusal::StoreFailed));
    assert_eq!(memory_and_files(&rig), before);
    assert_eq!(kept(&rig), None);
    assert_eq!(why_of(&rig, WEB, "d3"), Some(Why::Gate(Gate::AdvanceWait)));

    fail.store(false, Ordering::SeqCst);
    pause(&rig).expect("paused");
    fail.store(true, Ordering::SeqCst);
    let paused = memory_and_files(&rig);
    assert_eq!(resume(&rig), Err(proto::RolloutsRefusal::StoreFailed));
    assert_eq!(memory_and_files(&rig), paused);
    assert!(kept(&rig).expect("kept").paused.is_some());
    assert_eq!(why_of(&rig, WEB, "d3"), Some(Why::Gate(Gate::Paused)));

    fail.store(false, Ordering::SeqCst);
    resume(&rig).expect("resumed once the file can be written");
    advance(&rig).expect("advanced once the file can be written");
}
