//! `WaitConvergedResponse.not_yet` (field 3), added after `convergence`, and its messages in
//! waits.proto (`NotYet`, `WaitingHost`, `WaitingPlan`, `WaitingState`): what a wait that did not
//! end converged still waits on. A reader of the release before (the baseline,
//! `proto/compat/baseline.binpb`) knows none of them, reads the outcome and the convergence of an
//! answer that carries them as it always did, and writes the answer back unchanged; an answer the
//! release before wrote reads with no `not_yet`.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// An answer at the deadline with one host updating, one silent, and the plan waiting.
fn answer() -> op::WaitConvergedResponse {
    op::WaitConvergedResponse {
        outcome: op::WaitOutcome::Deadline.into(),
        convergence: Some(op::Convergence {
            desired: Some("0.3.212".to_owned()),
            state: op::ConvergenceState::InProgress.into(),
            ..op::Convergence::default()
        }),
        not_yet: Some(op::NotYet {
            hosts: vec![
                op::WaitingHost {
                    node_id: "mac-1".to_owned(),
                    role: "mac-build".to_owned(),
                    state: op::WaitingState::Updating.into(),
                    installed_version: Some("0.3.211".to_owned()),
                    updating_to: Some("0.3.212".to_owned()),
                    step: Some("verify".to_owned()),
                    step_attempt: 1,
                    ..op::WaitingHost::default()
                },
                op::WaitingHost {
                    node_id: "mac-2".to_owned(),
                    role: "mac-build".to_owned(),
                    state: op::WaitingState::Silent.into(),
                    since_unix: Some(1_800_000_000),
                    ..op::WaitingHost::default()
                },
            ],
            hosts_left_out: 0,
            plans: vec![op::WaitingPlan {
                host_group: "mac-build".to_owned(),
                waiting: op::PlanWait::Updating.into(),
                ..op::WaitingPlan::default()
            }],
            answered_unix: 1_800_000_060,
        }),
    }
}

/// Catches an answer with `not_yet` that a CI client of the release before refuses, or whose
/// outcome or convergence it then reads otherwise, or the release before already knowing a message
/// of these names (the addition would re-mean it).
#[test]
fn an_older_reader_keeps_the_outcome_of_an_answer_that_says_what_it_waits_on() {
    let older = baseline_pool();
    for name in ["NotYet", "WaitingHost", "WaitingPlan"] {
        let full = format!("hostkeeper.operator.v1.{name}");
        assert!(older.get_message_by_name(&full).is_none(), "{full}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.WaitingState")
            .is_none()
    );
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.WaitConvergedResponse")
        .expect("released");
    assert!(message.get_field(3).is_none(), "field 3 is new");
    let sent = answer();
    let bytes = sent.encode_to_vec();
    let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("outcome").as_deref(),
        Some(&Value::EnumNumber(op::WaitOutcome::Deadline as i32))
    );
    let back = op::WaitConvergedResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, sent, "the unknown field is written back as it came");
}

/// Catches an answer of a coordinator of the release before read as one that waits on nothing
/// (`not_yet` present and empty) rather than one that does not say.
#[test]
fn an_answer_of_the_release_before_says_nothing_of_what_it_waits_on() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.WaitConvergedResponse")
        .expect("released");
    let mut written = DynamicMessage::new(message);
    written.set_field_by_name(
        "outcome",
        Value::EnumNumber(op::WaitOutcome::Deadline as i32),
    );
    let read =
        op::WaitConvergedResponse::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.outcome(), op::WaitOutcome::Deadline);
    assert_eq!(read.not_yet, None);
}
