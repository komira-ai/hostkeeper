//! The messages of the update plan, added since the committed baseline
//! (`proto/compat/baseline.binpb`): each reads back as it was written, and a reader built from the
//! baseline, the release before, keeps what it does not know and writes it back unchanged.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

fn audit_entry(action: &str) -> op::AuditEntry {
    op::AuditEntry {
        seq: 41,
        by: op::Actor::Operator.into(),
        action: action.to_owned(),
        service: Some("db".to_owned()),
        node_id: Some("db-1".to_owned()),
        reason: Some("disk swap".to_owned()),
        at_unix: Some(1_800_000_000),
        by_name: Some("alice".to_owned()),
    }
}

/// A plan with every field set, of two stages, the second waiting for an operator.
fn plan() -> op::RolloutPlan {
    op::RolloutPlan {
        version: "2.0".to_owned(),
        stages: vec![
            op::PlanStage {
                number: 1,
                percent: Some(50),
                hosts: 1,
                advance: op::StageAdvance::Auto.into(),
                members: vec![op::PlanHost {
                    node_id: "db-1".to_owned(),
                    role: "storage".to_owned(),
                    state: op::PlanHostState::Done.into(),
                    deferring: false,
                }],
            },
            op::PlanStage {
                number: 2,
                percent: None,
                hosts: 2,
                advance: op::StageAdvance::Manual.into(),
                members: vec![op::PlanHost {
                    node_id: "db-2".to_owned(),
                    role: "storage".to_owned(),
                    state: op::PlanHostState::Waiting.into(),
                    deferring: true,
                }],
            },
        ],
        current: 1,
        eligible: 2,
        waiting: op::PlanWait::ManualAdvance.into(),
        paused: true,
        paused_by: Some("alice".to_owned()),
        pause_reason: Some("watching the first host".to_owned()),
        finished: false,
    }
}

/// One message of each type the update plan added, every field set, by full name.
fn plan_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("hostkeeper.operator.v1.RolloutPlan", round_trip(&plan())),
        (
            "hostkeeper.operator.v1.PlanStage",
            round_trip(&plan().stages[0]),
        ),
        (
            "hostkeeper.operator.v1.PlanHost",
            round_trip(&plan().stages[1].members[0]),
        ),
        (
            "hostkeeper.operator.v1.AdvanceRequest",
            round_trip(&op::AdvanceRequest {
                service: "db".to_owned(),
                reason: "first host looks fine".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.AdvanceResponse",
            round_trip(&op::AdvanceResponse {
                entry: Some(audit_entry("rollout_advance")),
                plan: Some(plan()),
            }),
        ),
        (
            "hostkeeper.operator.v1.PauseRequest",
            round_trip(&op::PauseRequest {
                service: "db".to_owned(),
                reason: "watching the first host".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PauseResponse",
            round_trip(&op::PauseResponse {
                entry: Some(audit_entry("rollout_pause")),
                plan: Some(plan()),
            }),
        ),
        (
            "hostkeeper.operator.v1.ResumeRequest",
            round_trip(&op::ResumeRequest {
                service: "db".to_owned(),
                reason: "the first host is fine".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ResumeResponse",
            round_trip(&op::ResumeResponse {
                entry: Some(audit_entry("rollout_resume")),
                plan: Some(plan()),
            }),
        ),
    ]
}

/// Catches: a message of the update plan that does not read back as written, or that a reader of
/// the release before drops: it knows none of them, and a message it reads with no field keeps
/// every field of the new ones unknown and writes them back unchanged.
#[test]
fn the_plan_messages_round_trip_and_an_older_reader_keeps_unknown_fields() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    let messages = plan_messages();
    assert_eq!(messages.len(), 9);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(!bytes.is_empty(), "{name} sets its fields");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
}

/// Catches: the update plan of a service, the stage of a host and the new gate reasons that a
/// reader of the release before drops or cannot read past. The baseline's `ServiceStatus` knows
/// no `plan` and its `HostService` no `stage`, and its `GateReason` has no value above 24:
/// it reads the fields it knows and writes the plan, the stage and the reason's number back
/// unchanged. A status of the release before reads with the plan and the stage absent, not zero.
#[test]
fn an_older_reader_keeps_the_plan_the_stage_and_the_new_gate_reasons() {
    let older = baseline_pool();

    let status = op::ServiceStatus {
        service: "db".to_owned(),
        up: 1,
        plan: Some(plan()),
        ..op::ServiceStatus::default()
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ServiceStatus")
        .expect("released");
    assert!(message.get_field_by_name("plan").is_none());
    let read =
        DynamicMessage::decode(message.clone(), round_trip(&status).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("db".to_owned()))
    );
    let back = op::ServiceStatus::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, status);

    let mut released = DynamicMessage::new(message);
    released.set_field_by_name("service", Value::String("db".to_owned()));
    let read = op::ServiceStatus::decode(released.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!((read.service.as_str(), read.plan), ("db", None));

    let new_reasons = [
        (op::GateReason::Paused, 25),
        (op::GateReason::AdvanceWait, 26),
        (op::GateReason::StageFailed, 27),
        (op::GateReason::StageWait, 28),
        (op::GateReason::BelowMinUp, 29),
        (op::GateReason::RolloutElsewhere, 30),
    ];
    let reasons = older
        .get_enum_by_name("hostkeeper.operator.v1.GateReason")
        .expect("released");
    let view = older
        .get_message_by_name("hostkeeper.operator.v1.HostService")
        .expect("released");
    assert!(view.get_field_by_name("stage").is_none());
    for (reason, number) in new_reasons {
        assert_eq!(reason as i32, number, "{reason:?}");
        assert!(reasons.get_value(number).is_none(), "{reason:?} is new");
        let host = op::HostService {
            service: "db".to_owned(),
            up: true,
            gate_reason: reason.into(),
            stage: Some(2),
            ..op::HostService::default()
        };
        let read =
            DynamicMessage::decode(view.clone(), round_trip(&host).as_slice()).expect("decodes");
        assert_eq!(
            read.get_field_by_name("gate_reason").as_deref(),
            Some(&Value::EnumNumber(number)),
            "{reason:?}"
        );
        let back = op::HostService::decode(read.encode_to_vec().as_slice()).expect("decodes");
        assert_eq!(back, host, "{reason:?}");
    }
    for value in reasons.values() {
        assert_eq!(
            op::GateReason::try_from(value.number())
                .expect("a value of the release before")
                .as_str_name(),
            value.name()
        );
    }

    // A host the release before wrote reads with no stage (a stage 0 would be a stage), and
    // keeps the reason it gave.
    let mut released = DynamicMessage::new(view);
    released.set_field_by_name("service", Value::String("db".to_owned()));
    released.set_field_by_name(
        "gate_reason",
        Value::EnumNumber(op::GateReason::NoSlot as i32),
    );
    let read = op::HostService::decode(released.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        (read.gate_reason(), read.stage),
        (op::GateReason::NoSlot, None)
    );
    let at_zero = op::HostService {
        stage: Some(0),
        ..op::HostService::default()
    };
    assert!(!round_trip(&at_zero).is_empty(), "stage 0 is written");

    // The plan's own enums and service are new with it.
    for name in [
        "hostkeeper.operator.v1.StageAdvance",
        "hostkeeper.operator.v1.PlanHostState",
        "hostkeeper.operator.v1.PlanWait",
        "hostkeeper.operator.v1.RolloutsRefusal",
    ] {
        assert!(older.get_enum_by_name(name).is_none(), "{name} is new");
    }
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Rollouts")
            .is_none()
    );
}
