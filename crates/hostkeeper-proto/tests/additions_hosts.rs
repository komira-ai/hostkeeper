//! What build item A18 of `docs/design/packages-native.md` (section 10.6) adds to the wire, new
//! since the committed baseline (`proto/compat/baseline.binpb`, the release before):
//!
//! - `Host.running` (22), the services a host runs with the version and installer of each, and
//!   `Host.running_unix` (23), when the coordinator took them from a heartbeat;
//! - the message `RunningService`.
//!
//! A reader of the release before keeps each field as one it does not know and writes it back
//! byte for byte; this build reads what that release wrote with `running` empty and
//! `running_unix` absent. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set this build serves.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// A host that runs two services, one with an installer and one without.
fn host() -> op::Host {
    op::Host {
        node_id: "d1".to_owned(),
        running: vec![
            op::RunningService {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                installer: "deb".to_owned(),
            },
            op::RunningService {
                service: "db".to_owned(),
                version: "9.1".to_owned(),
                installer: String::new(),
            },
        ],
        running_unix: Some(1_700_000_000),
        ..op::Host::default()
    }
}

/// Catches: the services a host runs, or the time they were taken, that an operator client of
/// the release before refuses, drops, or writes back changed. Proves the tags on the wire: field
/// 22 (`0xb2 0x01`) and field 23 (`0xb8 0x01`).
#[test]
fn an_older_reader_keeps_what_a_host_runs() {
    let older = baseline_pool();
    let host_d = message(&older, "hostkeeper.operator.v1.Host");
    assert!(host_d.get_field_by_name("running").is_none());
    assert!(host_d.get_field_by_name("running_unix").is_none());
    let bytes = host().encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host());

    let read = DynamicMessage::decode(host_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.sort_unstable();
    kept.dedup();
    assert_eq!(kept, [22, 23]);
    assert_eq!(read.encode_to_vec(), bytes);
    assert!(bytes.windows(2).any(|w| w == [0xb2, 0x01]), "field 22");
    assert!(bytes.windows(2).any(|w| w == [0xb8, 0x01]), "field 23");
}

/// Catches: a host of the release before read as running something, or as running since time 0,
/// when it said nothing.
#[test]
fn a_host_the_release_before_wrote_runs_nothing_known() {
    let older = baseline_pool();
    let mut written = DynamicMessage::new(message(&older, "hostkeeper.operator.v1.Host"));
    written.set_field_by_name("node_id", Value::String("d1".to_owned()));
    let read = op::Host::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.running.is_empty());
    assert_eq!(read.running_unix, None);
}

/// Catches: a message the baseline already holds, so that "new" is no longer true of it, and a
/// host with `running_unix` set to 0 read as having none.
#[test]
fn the_service_message_is_new_and_a_zero_time_is_a_time() {
    assert!(
        baseline_pool()
            .get_message_by_name("hostkeeper.operator.v1.RunningService")
            .is_none()
    );
    let host = op::Host {
        running_unix: Some(0),
        ..op::Host::default()
    };
    let read = op::Host::decode(host.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.running_unix, Some(0));
}

/// Catches: a new field given another number, label or type than the design says, which a peer
/// built apart would read as something else.
#[test]
fn the_new_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let service = Kind::Message(message(&now, "hostkeeper.operator.v1.RunningService"));
    let host_d = message(&now, "hostkeeper.operator.v1.Host");
    let running = host_d.get_field_by_name("running").expect("running");
    assert_eq!(
        (running.number(), running.kind(), running.is_list()),
        (22, service, true)
    );
    let at = host_d.get_field_by_name("running_unix").expect("time");
    assert_eq!(
        (at.number(), at.kind(), at.is_list()),
        (23, Kind::Uint64, false)
    );
    assert!(at.supports_presence(), "absent means nothing is known");

    let service_d = message(&now, "hostkeeper.operator.v1.RunningService");
    for (name, number) in [("service", 1), ("version", 2), ("installer", 3)] {
        let found = service_d.get_field_by_name(name).expect(name);
        assert_eq!(
            (found.number(), found.kind(), found.is_list()),
            (number, Kind::String, false),
            "{name}"
        );
    }
}
