//! What the operator keys add to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `HeartbeatResponse.operator_keys` (7), an
//! `OperatorKeys` set the coordinator sends a daemon that announced `operator-keys-v1`, and
//! `HeartbeatRequest.operator_keys` (15), the daemon's `OperatorKeysReport` with its
//! `OperatorKeysRefusal`. A reader of the release before keeps each field as one it does not know
//! and writes it back byte for byte; this build reads what that release wrote with neither. Those
//! are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

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

/// A report with every field of it and of its refusal set.
fn report() -> agent::OperatorKeysReport {
    agent::OperatorKeysReport {
        digest: "ab".repeat(32),
        refused: Some(agent::OperatorKeysRefusal {
            digest: "cd".repeat(32),
            reason: "key 2: a key blob that is not base64 of its key type".to_owned(),
        }),
        failed: "remote login did not start".to_owned(),
    }
}

/// Every message the release before does not know, by name.
const NEW_MESSAGES: [&str; 3] = [
    "hostkeeper.agent.v1.OperatorKeys",
    "hostkeeper.agent.v1.OperatorKeysReport",
    "hostkeeper.agent.v1.OperatorKeysRefusal",
];

/// Catches: a heartbeat with a report that a coordinator of the release before refuses, drops,
/// reads other fields of differently, or writes back changed. Proves the tag on the wire: field
/// 15 (`0x7a`), and that the fields beside it stay as written.
#[test]
fn an_older_coordinator_keeps_a_heartbeats_report() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("operator_keys").is_none());
    for name in NEW_MESSAGES {
        assert!(older.get_message_by_name(name).is_none(), "{name}");
    }
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::DarwinArm64.into(),
        features: vec!["operator-keys-v1".to_owned()],
        operator_keys: Some(report()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    assert!(bytes.contains(&0x7a));

    let read = DynamicMessage::decode(beat_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [15]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::DarwinArm64.into()))
    );
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String(
            "operator-keys-v1".to_owned()
        )]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an answer with a set of keys that a daemon of the release before refuses, or reads
/// its order differently. Proves the tag on the wire: field 7 (`0x3a`), and that the order beside
/// it stays as written.
#[test]
fn an_older_daemon_keeps_an_answers_keys_beside_its_order() {
    let older = baseline_pool();
    let answer_d = message(&older, "hostkeeper.agent.v1.HeartbeatResponse");
    assert!(answer_d.get_field_by_name("operator_keys").is_none());
    let answer = agent::HeartbeatResponse {
        features: vec!["facts-v1".to_owned()],
        operator_keys: Some(agent::OperatorKeys {
            digest: "ef".repeat(32),
            keys: vec!["ssh-ed25519 AAAA ops".to_owned(), "ssh-rsa BBBB".to_owned()],
        }),
        ..agent::HeartbeatResponse::default()
    };
    let bytes = answer.encode_to_vec();
    assert!(bytes.contains(&0x3a));
    let read = DynamicMessage::decode(answer_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [7]);
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String("facts-v1".to_owned())]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a heartbeat or an answer of the release before read with a report or a set of keys,
/// so that a coordinator shows a report no daemon sent, or a daemon takes away a block nobody
/// sent it the empty set for.
#[test]
fn the_release_befores_messages_read_without_keys_or_report() {
    let older = baseline_pool();
    let mut beat = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    beat.set_field_by_name(
        "features",
        Value::List(vec![Value::String("facts-v1".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.operator_keys, None, "absent: no report");
    assert_eq!(read.features, ["facts-v1"]);

    let mut answer = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatResponse"));
    answer.set_field_by_name(
        "features",
        Value::List(vec![Value::String("facts-v1".to_owned())]),
    );
    let read =
        agent::HeartbeatResponse::decode(answer.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.operator_keys, None, "absent: keep what it has");
    assert_eq!(read.features, ["facts-v1"]);
}

/// Catches: the empty set (which takes the managed block away) read as absent (which keeps
/// it), and an empty report read as no report: each would make the daemon or the coordinator
/// do the other thing.
#[test]
fn the_empty_set_and_the_empty_report_are_distinct_from_absent() {
    let empty = agent::HeartbeatResponse {
        operator_keys: Some(agent::OperatorKeys::default()),
        ..agent::HeartbeatResponse::default()
    };
    assert_eq!(empty.encode_to_vec(), [0x3a, 0]);
    assert_eq!(
        agent::HeartbeatResponse::decode([0x3a, 0].as_slice()).expect("decodes"),
        empty
    );
    assert_eq!(
        agent::HeartbeatResponse::decode(&[][..])
            .expect("decodes")
            .operator_keys,
        None
    );
    let reported = agent::HeartbeatRequest {
        operator_keys: Some(agent::OperatorKeysReport::default()),
        ..agent::HeartbeatRequest::default()
    };
    assert_eq!(reported.encode_to_vec(), [0x7a, 0]);
    assert_eq!(
        agent::HeartbeatRequest::decode([0x7a, 0].as_slice()).expect("decodes"),
        reported
    );
    let unrefused = agent::OperatorKeysReport::decode(&[][..]).expect("decodes");
    assert_eq!(unrefused.refused, None);
}

/// Catches: a new field or message given another number, label or type than the schema says,
/// which a peer built apart would read as something else.
#[test]
fn the_operator_keys_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let kind = |name: &str| Kind::Message(message(&now, &format!("hostkeeper.agent.v1.{name}")));
    let fields: [(&str, &str, u32, Kind, bool, bool); 8] = [
        (
            "HeartbeatResponse",
            "operator_keys",
            7,
            kind("OperatorKeys"),
            false,
            true,
        ),
        (
            "HeartbeatRequest",
            "operator_keys",
            15,
            kind("OperatorKeysReport"),
            false,
            true,
        ),
        ("OperatorKeys", "digest", 1, Kind::String, false, false),
        ("OperatorKeys", "keys", 2, Kind::String, true, false),
        (
            "OperatorKeysReport",
            "digest",
            1,
            Kind::String,
            false,
            false,
        ),
        (
            "OperatorKeysReport",
            "refused",
            2,
            kind("OperatorKeysRefusal"),
            false,
            true,
        ),
        (
            "OperatorKeysReport",
            "failed",
            3,
            Kind::String,
            false,
            false,
        ),
        (
            "OperatorKeysRefusal",
            "digest",
            1,
            Kind::String,
            false,
            false,
        ),
    ];
    for (name, field, number, kind, repeated, presence) in fields {
        let found = message(&now, &format!("hostkeeper.agent.v1.{name}"))
            .get_field_by_name(field)
            .unwrap_or_else(|| panic!("{name}.{field}"));
        assert_eq!(
            (
                found.number(),
                found.kind(),
                found.is_list(),
                found.supports_presence()
            ),
            (number, kind, repeated, presence),
            "{name}.{field}"
        );
    }
    let reason = message(&now, "hostkeeper.agent.v1.OperatorKeysRefusal")
        .get_field_by_name("reason")
        .expect("reason");
    assert_eq!((reason.number(), reason.kind()), (2, Kind::String));
    let counts: Vec<usize> = NEW_MESSAGES
        .iter()
        .map(|name| message(&now, name).fields().count())
        .collect();
    assert_eq!(counts, [2, 3, 2]);
}
