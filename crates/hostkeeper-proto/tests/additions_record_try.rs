//! `ServiceBeat.record_try` (field 12), added after `pinned`: the try a daemon's step record
//! holds, which a coordinator numbers its next order past. A reader of the release before (the
//! baseline, `proto/compat/baseline.binpb`) knows no such field, reads every field it knows from
//! an entry that carries it, and writes it back unchanged; an entry the release before wrote
//! reads with the field absent, which is not the try 0.
//!
//! `StepRun.deferrals` (field 3, a `StepDeferrals`), the same way: what a host waits for while its
//! update's `verify` defers. That reader keeps it, unknown, and writes it back unchanged; a step
//! that reader wrote reads with no deferrals.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// Catches a record try that a coordinator of the release before refuses or drops, and an entry
/// of a daemon of the release before read as the record holding try 0 (which would number the
/// next order from 1 as if a record had been heard).
#[test]
fn an_older_reader_keeps_a_services_record_try() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.ServiceBeat")
        .expect("released");
    assert!(message.get_field_by_name("record_try").is_none());
    let entry = agent::ServiceBeat {
        name: "hostkeeper".to_owned(),
        version: "0.1.111".to_owned(),
        health: agent::Health::Healthy.into(),
        record_try: Some(1),
        ..agent::ServiceBeat::default()
    };
    let bytes = entry.encode_to_vec();
    assert_eq!(
        agent::ServiceBeat::decode(bytes.as_slice()).expect("decodes"),
        entry
    );
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("version").as_deref(),
        Some(&Value::String("0.1.111".to_owned()))
    );
    let back = agent::ServiceBeat::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, entry);

    let mut written = DynamicMessage::new(message);
    written.set_field_by_name("name", Value::String("hostkeeper".to_owned()));
    written.set_field_by_name("version", Value::String("0.1.111".to_owned()));
    let read = agent::ServiceBeat::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.record_try, None);
    let at_zero = agent::ServiceBeat {
        record_try: Some(0),
        ..agent::ServiceBeat::default()
    };
    assert!(!at_zero.encode_to_vec().is_empty(), "0 is written");
}

/// Catches the deferrals of a running `verify` that a coordinator of the release before refuses
/// or drops on its way back out (the operator view copies the heartbeat), and a step it wrote read
/// as deferred: it keeps them as an unknown field, byte for byte, and a step of only a name and a
/// try reads with none.
#[test]
fn an_older_reader_keeps_a_steps_deferrals() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.StepRun")
        .expect("released");
    assert!(message.get_field_by_name("deferrals").is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.StepDeferrals")
            .is_none()
    );
    let step = agent::StepRun {
        name: "verify".to_owned(),
        attempt: 3,
        deferrals: Some(agent::StepDeferrals {
            count: 2,
            reason: Some("provider_busy".to_owned()),
            detail: Some("read of /Volumes/build: Operation not permitted".to_owned()),
            since_unix: Some(1_790_000_000),
        }),
    };
    let bytes = step.encode_to_vec();
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("attempt").as_deref(),
        Some(&Value::U32(3))
    );
    assert_eq!(read.encode_to_vec(), bytes);
    assert_eq!(
        agent::StepRun::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        step
    );

    let mut written = DynamicMessage::new(message);
    written.set_field_by_name("name", Value::String("verify".to_owned()));
    written.set_field_by_name("attempt", Value::U32(3));
    let read = agent::StepRun::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.deferrals, None);
    assert_eq!((read.name.as_str(), read.attempt), ("verify", 3));
}
