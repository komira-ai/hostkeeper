//! The command queue's model: the design's rules hold every invariant, and each rule, given up
//! one at a time, breaks the invariant it protects.

use hostkeeper_mdm_model::explore::{Trace, explore};
use hostkeeper_mdm_model::queue::*;

const LIMIT: usize = 1_000_000;

fn find(rules: Rules) -> Trace {
    let report = explore(&QueueModel { rules }, LIMIT);
    assert!(!report.truncated, "the search was cut at {LIMIT} states");
    report.trace.expect("the rules should break an invariant")
}

fn assert_breaks(rules: Rules, invariant: &str, steps: &[&str]) {
    let trace = find(rules);
    assert_eq!(trace.violation.invariant, invariant, "{trace:?}");
    assert_eq!(trace.steps, steps, "{trace:?}");
}

#[test]
fn the_design_rules_hold_every_invariant_over_every_reachable_state() {
    let report = explore(
        &QueueModel {
            rules: Rules::design(),
        },
        LIMIT,
    );
    assert!(!report.truncated);
    assert!(report.trace.is_none(), "{:?}", report.trace);
    // Measured: the number of states of two commands, two enrollments and two connections.
    assert_eq!(report.states, 31165);
}

#[test]
fn a_command_is_delivered_to_at_most_one_connection_at_a_time() {
    // A claim that takes a Sent command hands it to a second connection.
    assert_breaks(
        Rules {
            claim_only_queued: false,
            ..Rules::design()
        },
        SINGLE_DELIVERY,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "Connect { slot: 1 }",
            "Idle { slot: 1, lost: false }",
        ],
    );
}

#[test]
fn a_command_whose_timeout_fires_while_the_mac_holds_it_is_delivered_twice() {
    // The command timeout re-sends a command whose Mac has not answered yet: at least once, not
    // at most once. The design's rules hold only if a Mac answers within the timeout or never.
    assert_breaks(
        Rules {
            timeout_while_held: true,
            ..Rules::design()
        },
        SINGLE_DELIVERY,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "Timeout { cmd: 0 }",
            "Connect { slot: 1 }",
            "Idle { slot: 1, lost: false }",
        ],
    );
}

#[test]
fn every_other_invariant_holds_when_a_slow_mac_answers_after_the_re_send() {
    // The same world, with delivery counted at least once: a late result of the first delivery
    // finds the command Sent again, Acked or Cancelled, and none of the other rules breaks.
    let rules = Rules {
        timeout_while_held: true,
        require_single_delivery: false,
        ..Rules::design()
    };
    let report = explore(&QueueModel { rules }, LIMIT);
    assert!(!report.truncated);
    assert!(report.trace.is_none(), "{:?}", report.trace);
    // Measured.
    assert_eq!(report.states, 97029);
}

#[test]
fn an_acknowledged_command_is_never_re_sent() {
    // The invariant on a row: a command that was acknowledged and is queued again.
    let model = QueueModel {
        rules: Rules::design(),
    };
    let mut s = hostkeeper_mdm_model::explore::Model::init(&model).remove(0);
    s.cmds[0].done = Some(Status::Acked);
    s.cmds[0].status = Status::Queued;
    let v = hostkeeper_mdm_model::explore::Model::check(&model, &s).unwrap_err();
    assert_eq!(v.invariant, ACKED_NEVER_RESENT);
    assert_eq!(v.detail, "command 0 was acknowledged and is now Queued");
}

#[test]
fn a_command_enqueued_again_after_it_ended_is_not_revived() {
    // Enqueuing an id that exists replaces the row when enqueue is not idempotent: the gate's
    // retry of a cancelled command puts it back in the queue.
    assert_breaks(
        Rules {
            enqueue_is_idempotent: false,
            ..Rules::design()
        },
        RESULT_ONLY_TO_SENT,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Reenroll { wiped: false }",
            "Enqueue { cmd: 0, target: 1 }",
        ],
    );
}

#[test]
fn a_result_is_matched_only_to_a_command_in_sent() {
    // The Mac, not wiped, answers on its new identity a command the re-enrollment cancelled; a
    // result matched to any status turns Cancelled into Acknowledged.
    assert_breaks(
        Rules {
            match_result_to_sent_only: false,
            ..Rules::design()
        },
        RESULT_ONLY_TO_SENT,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "Reenroll { wiped: false }",
            "Connect { slot: 1 }",
            "Answer { slot: 1, cmd: 0, answer: Acknowledged }",
        ],
    );
}

#[test]
fn a_not_now_command_is_never_lost() {
    assert_breaks(
        Rules {
            not_now_keeps_command: false,
            ..Rules::design()
        },
        NOT_NOW_KEPT,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "Answer { slot: 0, cmd: 0, answer: NotNow }",
        ],
    );
}

#[test]
fn a_not_now_command_is_not_resent_on_the_same_connection() {
    assert_breaks(
        Rules {
            skip_tried_on_connection: false,
            ..Rules::design()
        },
        NOT_NOW_NO_LOOP,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "Answer { slot: 0, cmd: 0, answer: NotNow }",
            "Idle { slot: 0, lost: false }",
        ],
    );
}

#[test]
fn no_command_is_delivered_to_a_superseded_identity() {
    // A connection opened under the old enrollment keeps asking after the serial enrolled again.
    assert_breaks(
        Rules {
            refuse_superseded_identity: false,
            ..Rules::design()
        },
        NO_SUPERSEDED_DELIVERY,
        &[
            "Connect { slot: 0 }",
            "Reenroll { wiped: false }",
            "Enqueue { cmd: 0, target: 1 }",
            "Idle { slot: 0, lost: false }",
        ],
    );
}

#[test]
fn no_command_is_stranded_on_a_superseded_enrollment() {
    // The gate read the enrollment before the serial enrolled again and enqueues for the old one.
    assert_breaks(
        Rules {
            reject_superseded_target: false,
            ..Rules::design()
        },
        NO_STRANDED,
        &["Reenroll { wiped: false }", "Enqueue { cmd: 0, target: 0 }"],
    );
}

#[test]
fn a_queued_command_always_has_a_wake_after_not_now() {
    // NotNow clears the outstanding push; if it does not arm the floor, the command waits for a
    // Mac that has to Idle on its own.
    assert_breaks(
        Rules {
            not_now_arms_repush: false,
            ..Rules::design()
        },
        ALWAYS_A_WAKE,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: false }",
            "FloorElapsed",
            "Answer { slot: 0, cmd: 0, answer: NotNow }",
        ],
    );
}

#[test]
fn a_queued_command_always_has_a_wake_after_a_timeout() {
    // The answer to a command was lost on the way to the Mac; the timeout returns the command to
    // the queue, and without a push nothing wakes the Mac.
    assert_breaks(
        Rules {
            timeout_requeue_pushes: false,
            ..Rules::design()
        },
        ALWAYS_A_WAKE,
        &[
            "Enqueue { cmd: 0, target: 0 }",
            "Connect { slot: 0 }",
            "Idle { slot: 0, lost: true }",
            "FloorElapsed",
            "Timeout { cmd: 0 }",
        ],
    );
}
