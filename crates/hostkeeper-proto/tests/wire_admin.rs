//! The frozen `hostkeeper.local.v1` admin messages on the wire, built dynamically: an older
//! `hostkeeper admin` on a host still speaks them, so what they carry stays pinned.

use prost_reflect::Value;

use hostkeeper_proto::hostkeeper::agent::v1::FailurePhase;

#[path = "support/frozen.rs"]
mod frozen;

/// Catches: an Admin status that cannot say a step record is unreadable, an abort that cannot
/// return the record it closed, and a clear that cannot say what it cleared.
#[test]
fn the_admin_calls_carry_a_record_problem_and_the_closed_record() {
    let status = frozen::local(
        "StatusResponse",
        [(
            "services",
            Value::List(vec![Value::Message(frozen::local(
                "ServiceRecord",
                [
                    ("service", Value::String("db".to_owned())),
                    (
                        "record_problem",
                        frozen::variant("RecordProblem", "RECORD_PROBLEM_UNREADABLE"),
                    ),
                ],
            ))]),
        )],
    );
    let read = frozen::read_back(&status);
    let services = read.get_field_by_name("services").expect("services");
    let Some(Value::Message(service)) = services.as_list().and_then(<[Value]>::first) else {
        panic!("a service");
    };
    assert_eq!(
        frozen::enum_name(service, "record_problem"),
        "RECORD_PROBLEM_UNREADABLE"
    );
    assert!(!service.has_field_by_name("current"));

    let aborted = frozen::local(
        "AbortResponse",
        [(
            "record",
            Value::Message(frozen::local(
                "StepRecord",
                [
                    ("order_id", Value::String("o-17".to_owned())),
                    ("order_try", Value::U64(2)),
                    (
                        "outcome",
                        frozen::variant("StepOutcome", "STEP_OUTCOME_FAILED"),
                    ),
                    ("phase", Value::EnumNumber(FailurePhase::Halt.into())),
                ],
            )),
        )],
    );
    let read = frozen::read_back(&aborted);
    let record = read.get_field_by_name("record").expect("a record");
    let record = record.as_message().expect("a message");
    assert_eq!(
        record.get_field_by_name("phase").as_deref(),
        Some(&Value::EnumNumber(FailurePhase::Halt.into()))
    );
    assert_eq!(
        record.get_field_by_name("order_id").as_deref(),
        Some(&Value::String("o-17".to_owned()))
    );
    assert_eq!(
        record.get_field_by_name("order_try").as_deref(),
        Some(&Value::U64(2))
    );

    let cleared = frozen::local(
        "ClearRecordResponse",
        [(
            "problem",
            frozen::variant("RecordProblem", "RECORD_PROBLEM_NEWER_SCHEMA"),
        )],
    );
    let read = frozen::read_back(&cleared);
    assert_eq!(
        frozen::enum_name(&read, "problem"),
        "RECORD_PROBLEM_NEWER_SCHEMA"
    );
}
