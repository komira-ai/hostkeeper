//! `HeartbeatRequest.trusted_root_age_secs` and `trusted_root_reason` (fields 8 and 9), with the
//! enum `TrustedRootReason`, and `Host.trusted_root_age_secs` and `trusted_root_reason` (fields 19
//! and 20), added after `first_serving_unix`: the age of the Sigstore trusted root a daemon checks
//! packages with, and why it needs attention. A reader of the release before (the baseline,
//! `proto/compat/baseline.binpb`) knows no such field, reads every field it knows from a message
//! that carries them, and writes them back unchanged; a heartbeat or host the release before wrote
//! reads with the age absent and the reason unspecified, which is not a root that needs no
//! attention.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// Catches a trusted root that a coordinator of the release before refuses or drops from a
/// heartbeat, and a heartbeat of a daemon of the release before read as a root of age 0 that
/// needs no attention.
#[test]
fn an_older_reader_keeps_a_heartbeats_trusted_root() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
        .expect("released");
    assert!(message.get_field_by_name("trusted_root_age_secs").is_none());
    assert!(message.get_field_by_name("trusted_root_reason").is_none());
    assert!(
        older
            .get_enum_by_name("hostkeeper.agent.v1.TrustedRootReason")
            .is_none()
    );
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        sent_unix: Some(1_800_000_000),
        trusted_root_age_secs: Some(91 * 86_400),
        trusted_root_reason: agent::TrustedRootReason::Stale.into(),
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
    assert_eq!(read.trusted_root_age_secs, None);
    assert_eq!(
        read.trusted_root_reason(),
        agent::TrustedRootReason::Unspecified
    );
    let fresh = agent::HeartbeatRequest {
        trusted_root_age_secs: Some(0),
        ..agent::HeartbeatRequest::default()
    };
    assert!(!fresh.encode_to_vec().is_empty(), "the age 0 is written");
}

/// Catches a host's trusted root that an operator client of the release before drops or cannot
/// read past, and a host of the release before read as a root that needs no attention.
#[test]
fn an_older_reader_keeps_a_hosts_trusted_root() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.Host")
        .expect("released");
    assert!(message.get_field_by_name("trusted_root_age_secs").is_none());
    assert!(message.get_field_by_name("trusted_root_reason").is_none());
    let host = op::Host {
        node_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        trusted_root_reason: agent::TrustedRootReason::Missing.into(),
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
    assert_eq!(read.trusted_root_age_secs, None);
    assert_eq!(
        read.trusted_root_reason(),
        agent::TrustedRootReason::Unspecified
    );
}
