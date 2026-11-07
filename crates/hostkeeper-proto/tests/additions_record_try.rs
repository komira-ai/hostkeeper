//! `ServiceBeat.record_try` (field 12), added after `pinned`: the try a daemon's step record
//! holds, which a coordinator numbers its next order past. A reader of the release before (the
//! baseline, `proto/compat/baseline.binpb`) knows no such field, reads every field it knows from
//! an entry that carries it, and writes it back unchanged; an entry the release before wrote
//! reads with the field absent, which is not the try 0.

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
