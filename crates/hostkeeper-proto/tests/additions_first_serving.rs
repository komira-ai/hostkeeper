//! `Host.first_serving_unix` (field 18), added after `first_heartbeat_unix`: when the
//! coordinator first heard an enrolled host report a service it manages for the host installed
//! and healthy. A reader of the release before (the baseline, `proto/compat/baseline.binpb`)
//! knows no such field, reads every field it knows from a host that carries it, and writes it
//! back unchanged; a host the release before wrote reads with the field absent, which is not the
//! time 0.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// Catches a host's serving time that a reader of the release before drops or cannot read past,
/// and a host of the release before read as serving since 0.
#[test]
fn an_older_reader_keeps_a_hosts_first_serving_time() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.Host")
        .expect("released");
    assert!(message.get_field_by_name("first_serving_unix").is_none());
    let host = op::Host {
        node_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        last_heartbeat_unix: 1_800_000_160,
        first_heartbeat_unix: Some(1_800_000_090),
        first_serving_unix: Some(1_800_000_150),
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host);
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("node_id").as_deref(),
        Some(&Value::String("db-1".to_owned()))
    );
    let back = op::Host::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, host);

    let mut written = DynamicMessage::new(message);
    written.set_field_by_name("node_id", Value::String("db-1".to_owned()));
    let read = op::Host::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.first_serving_unix, None);
    let at_zero = op::Host {
        first_serving_unix: Some(0),
        ..op::Host::default()
    };
    assert!(!at_zero.encode_to_vec().is_empty(), "0 is written");
}
