//! `HeartbeatRequest.full_disk_access` (field 19) with the enum `FullDiskAccess`, and
//! `Host.full_disk_access` (field 29), added after `maintenance` and `maintenance_stale`: whether a
//! Mac's Full Disk Access grant is in force for its daemon. A reader of the release before (the
//! baseline, `proto/compat/baseline.binpb`) knows no such field, reads every field it knows from a
//! message that carries it, and writes it back unchanged; a heartbeat or host the release before
//! wrote reads as UNSPECIFIED, which is not a grant in force.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// Proves: the field numbers and the enum's numbers are the ones the design fixed, and the release
/// before knows neither the enum nor the fields.
///
/// Catches: a renumbered field or enum value, which would make an older peer read another state.
#[test]
fn the_numbers_and_types_are_the_fixed_ones() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    for (message, number) in [
        ("hostkeeper.agent.v1.HeartbeatRequest", 19),
        ("hostkeeper.operator.v1.Host", 29),
    ] {
        let message = pool.get_message_by_name(message).expect("the message");
        let field = message
            .get_field_by_name("full_disk_access")
            .expect("field");
        assert_eq!(field.number(), number);
        assert_eq!(
            field.kind().as_enum().expect("an enum").full_name(),
            "hostkeeper.agent.v1.FullDiskAccess"
        );
    }
    let numbers: Vec<(String, i32)> = pool
        .get_enum_by_name("hostkeeper.agent.v1.FullDiskAccess")
        .expect("the enum")
        .values()
        .map(|value| (value.name().to_owned(), value.number()))
        .collect();
    assert_eq!(
        numbers,
        [
            ("FULL_DISK_ACCESS_UNSPECIFIED".to_owned(), 0),
            ("FULL_DISK_ACCESS_GRANTED".to_owned(), 1),
            ("FULL_DISK_ACCESS_NOT_GRANTED".to_owned(), 2),
            ("FULL_DISK_ACCESS_NOT_APPLICABLE".to_owned(), 3),
            ("FULL_DISK_ACCESS_UNDETERMINED".to_owned(), 4),
        ]
    );
    assert!(
        baseline_pool()
            .get_enum_by_name("hostkeeper.agent.v1.FullDiskAccess")
            .is_none()
    );
}

/// Proves: a reader of the release before reads a heartbeat that carries the state, and writes
/// the state back byte for byte; a heartbeat it wrote reads as UNSPECIFIED.
///
/// Catches: a state that a coordinator of the release before refuses or drops from a heartbeat,
/// and a heartbeat of a daemon of the release before read as a grant that is in force.
#[test]
fn an_older_reader_keeps_a_heartbeats_full_disk_access() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
        .expect("released");
    assert!(message.get_field_by_name("full_disk_access").is_none());
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        sent_unix: Some(1_800_000_000),
        full_disk_access: agent::FullDiskAccess::NotGranted.into(),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::LinuxX8664.into()))
    );
    let back = agent::HeartbeatRequest::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, beat);

    let mut written = DynamicMessage::new(message);
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxX8664.into()),
    );
    let read =
        agent::HeartbeatRequest::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.full_disk_access(), agent::FullDiskAccess::Unspecified);
}

/// Proves: a reader of the release before keeps the state of a host it reads, and a host it wrote
/// reads as UNSPECIFIED.
///
/// Catches: a host's state that an operator client of the release before drops or cannot read
/// past, and a host of the release before read as one whose grant is in force.
#[test]
fn an_older_reader_keeps_a_hosts_full_disk_access() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.Host")
        .expect("released");
    assert!(message.get_field_by_name("full_disk_access").is_none());
    let host = op::Host {
        node_id: "mac-1".to_owned(),
        role: "build".to_owned(),
        full_disk_access: agent::FullDiskAccess::Undetermined.into(),
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host);
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("node_id").as_deref(),
        Some(&Value::String("mac-1".to_owned()))
    );
    let back = op::Host::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, host);

    let mut written = DynamicMessage::new(message);
    written.set_field_by_name("node_id", Value::String("mac-1".to_owned()));
    let read = op::Host::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.full_disk_access(), agent::FullDiskAccess::Unspecified);
}

/// Proves: an enum number this release does not know reads as UNSPECIFIED, and the heartbeat
/// carrying it keeps the number when written back.
///
/// Catches: a state a later daemon sends read as granted, or dropped on the way through.
#[test]
fn an_unknown_state_number_reads_as_unspecified_and_is_kept() {
    let beat = agent::HeartbeatRequest {
        full_disk_access: 99,
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    let read = agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read.full_disk_access(), agent::FullDiskAccess::Unspecified);
    assert_eq!(read.full_disk_access, 99);
    assert_eq!(read.encode_to_vec(), bytes);
}
