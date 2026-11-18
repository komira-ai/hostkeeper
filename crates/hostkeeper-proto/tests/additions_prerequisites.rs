//! What the host prerequisites add to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `HeartbeatResponse.prerequisites` (8), a
//! `HostPrerequisites` set the coordinator sends a daemon that announced `prerequisites-v1`, and
//! `HeartbeatRequest.prerequisites` (16), the daemon's `PrerequisitesReport` with its
//! `PrerequisitesRefusal` and one `PrerequisiteState` for each name, whose `PrerequisiteStatus` is
//! a new enum. A reader of the release before keeps each field as one it does not know and writes
//! it back byte for byte; this build reads what that release wrote with neither. Those are the
//! tests of rule B8 in both directions.

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

/// A report with every field of it, of its refusal and of its two states set.
fn report() -> agent::PrerequisitesReport {
    agent::PrerequisitesReport {
        digest: "ab".repeat(32),
        refused: Some(agent::PrerequisitesRefusal {
            digest: "cd".repeat(32),
            reason: "the digest is not the names'".to_owned(),
        }),
        states: vec![
            agent::PrerequisiteState {
                name: "xcode-command-line-tools".to_owned(),
                status: agent::PrerequisiteStatus::Present.into(),
                version: "16.2.0.0.1.1733547573".to_owned(),
                seen_unix: 1_800_000_000,
                ..agent::PrerequisiteState::default()
            },
            agent::PrerequisiteState {
                name: "another".to_owned(),
                status: agent::PrerequisiteStatus::Failed.into(),
                reason: "softwareupdate --install exited 1".to_owned(),
                failures: 2,
                next_try_unix: 1_800_003_600,
                seen_unix: 1_800_000_000,
                ..agent::PrerequisiteState::default()
            },
        ],
    }
}

/// Every message the release before does not know, by name.
const NEW_MESSAGES: [&str; 4] = [
    "hostkeeper.agent.v1.HostPrerequisites",
    "hostkeeper.agent.v1.PrerequisitesReport",
    "hostkeeper.agent.v1.PrerequisitesRefusal",
    "hostkeeper.agent.v1.PrerequisiteState",
];

/// Catches: a heartbeat with a report that a coordinator of the release before refuses, drops,
/// reads other fields of differently, or writes back changed. Proves the tag on the wire: field
/// 16 (`0x82 0x01`), and that the fields beside it stay as written.
#[test]
fn an_older_coordinator_keeps_a_heartbeats_report() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("prerequisites").is_none());
    for name in NEW_MESSAGES {
        assert!(older.get_message_by_name(name).is_none(), "{name}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.agent.v1.PrerequisiteStatus")
            .is_none()
    );
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::DarwinArm64.into(),
        features: vec!["prerequisites-v1".to_owned()],
        prerequisites: Some(report()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    assert!(bytes.windows(2).any(|w| w == [0x82, 0x01]));

    let read = DynamicMessage::decode(beat_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [16]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::DarwinArm64.into()))
    );
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String(
            "prerequisites-v1".to_owned()
        )]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an answer with a set of prerequisites that a daemon of the release before refuses, or
/// reads its order differently. Proves the tag on the wire: field 8 (`0x42`), and that the
/// features beside it stay as written.
#[test]
fn an_older_daemon_keeps_an_answers_prerequisites_beside_its_features() {
    let older = baseline_pool();
    let answer_d = message(&older, "hostkeeper.agent.v1.HeartbeatResponse");
    assert!(answer_d.get_field_by_name("prerequisites").is_none());
    let answer = agent::HeartbeatResponse {
        features: vec!["facts-v1".to_owned()],
        prerequisites: Some(agent::HostPrerequisites {
            digest: "ef".repeat(32),
            names: vec!["xcode-command-line-tools".to_owned(), "another".to_owned()],
        }),
        ..agent::HeartbeatResponse::default()
    };
    let bytes = answer.encode_to_vec();
    assert!(bytes.contains(&0x42));
    let read = DynamicMessage::decode(answer_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [8]);
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String("facts-v1".to_owned())]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a heartbeat or an answer of the release before read with a report or a set, so that
/// a coordinator shows a report no daemon sent, or a daemon takes a set nobody sent it (and
/// keeps making sure of nothing).
#[test]
fn the_release_befores_messages_read_without_prerequisites_or_report() {
    let older = baseline_pool();
    let mut beat = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    beat.set_field_by_name(
        "features",
        Value::List(vec![Value::String("facts-v1".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.prerequisites, None, "absent: no report");
    assert_eq!(read.features, ["facts-v1"]);

    let mut answer = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatResponse"));
    answer.set_field_by_name(
        "features",
        Value::List(vec![Value::String("facts-v1".to_owned())]),
    );
    let read =
        agent::HeartbeatResponse::decode(answer.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.prerequisites, None, "absent: keep what it holds");
    assert_eq!(read.features, ["facts-v1"]);
}

/// Catches: the empty set (a host whose group needs nothing) read as absent (keep what it holds),
/// and an empty report read as no report: each would make the daemon or the coordinator do the
/// other thing.
#[test]
fn the_empty_set_and_the_empty_report_are_distinct_from_absent() {
    let empty = agent::HeartbeatResponse {
        prerequisites: Some(agent::HostPrerequisites::default()),
        ..agent::HeartbeatResponse::default()
    };
    assert_eq!(empty.encode_to_vec(), [0x42, 0]);
    assert_eq!(
        agent::HeartbeatResponse::decode([0x42, 0].as_slice()).expect("decodes"),
        empty
    );
    assert_eq!(
        agent::HeartbeatResponse::decode(&[][..])
            .expect("decodes")
            .prerequisites,
        None
    );
    let reported = agent::HeartbeatRequest {
        prerequisites: Some(agent::PrerequisitesReport::default()),
        ..agent::HeartbeatRequest::default()
    };
    assert_eq!(reported.encode_to_vec(), [0x82, 0x01, 0]);
    assert_eq!(
        agent::HeartbeatRequest::decode([0x82, 0x01, 0].as_slice()).expect("decodes"),
        reported
    );
    let unrefused = agent::PrerequisitesReport::decode(&[][..]).expect("decodes");
    assert_eq!(unrefused.refused, None);
    assert!(unrefused.states.is_empty());
}

/// Catches: a status number this build does not know read as a known status, or dropped: it
/// reads as the unspecified status (as every proto3 enum does) and the number is written back, so
/// a newer daemon's status reaches the coordinator's host view as it came.
#[test]
fn a_status_number_this_build_does_not_know_is_kept() {
    let state = agent::PrerequisiteState::decode([0x10, 0x63].as_slice()).expect("decodes");
    assert_eq!(state.status, 99);
    assert_eq!(
        agent::PrerequisiteStatus::try_from(state.status).ok(),
        None,
        "not a status of this build"
    );
    assert_eq!(state.encode_to_vec(), [0x10, 0x63]);
}

/// Catches: a new field or message given another number, label or type than the schema says,
/// which a peer built apart would read as something else.
#[test]
fn the_prerequisites_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let kind = |name: &str| Kind::Message(message(&now, &format!("hostkeeper.agent.v1.{name}")));
    let status = Kind::Enum(
        now.get_enum_by_name("hostkeeper.agent.v1.PrerequisiteStatus")
            .expect("the enum"),
    );
    let fields: [(&str, &str, u32, Kind, bool, bool); 14] = [
        (
            "HeartbeatResponse",
            "prerequisites",
            8,
            kind("HostPrerequisites"),
            false,
            true,
        ),
        (
            "HeartbeatRequest",
            "prerequisites",
            16,
            kind("PrerequisitesReport"),
            false,
            true,
        ),
        ("HostPrerequisites", "digest", 1, Kind::String, false, false),
        ("HostPrerequisites", "names", 2, Kind::String, true, false),
        (
            "PrerequisitesReport",
            "digest",
            1,
            Kind::String,
            false,
            false,
        ),
        (
            "PrerequisitesReport",
            "refused",
            2,
            kind("PrerequisitesRefusal"),
            false,
            true,
        ),
        (
            "PrerequisitesReport",
            "states",
            3,
            kind("PrerequisiteState"),
            true,
            false,
        ),
        (
            "PrerequisitesRefusal",
            "digest",
            1,
            Kind::String,
            false,
            false,
        ),
        (
            "PrerequisitesRefusal",
            "reason",
            2,
            Kind::String,
            false,
            false,
        ),
        ("PrerequisiteState", "name", 1, Kind::String, false, false),
        ("PrerequisiteState", "status", 2, status, false, false),
        (
            "PrerequisiteState",
            "version",
            3,
            Kind::String,
            false,
            false,
        ),
        ("PrerequisiteState", "reason", 4, Kind::String, false, false),
        (
            "PrerequisiteState",
            "failures",
            5,
            Kind::Uint32,
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
    let state = message(&now, "hostkeeper.agent.v1.PrerequisiteState");
    for (field, number) in [("next_try_unix", 6), ("seen_unix", 7)] {
        let found = state.get_field_by_name(field).expect(field);
        assert_eq!((found.number(), found.kind()), (number, Kind::Uint64));
    }
    let counts: Vec<usize> = NEW_MESSAGES
        .iter()
        .map(|name| message(&now, name).fields().count())
        .collect();
    assert_eq!(counts, [2, 3, 2, 7]);
}

/// Catches: a status value renumbered or renamed, or one inserted before the others: the numbers
/// are on the wire in every report, and the names are what `hkctl` shows.
#[test]
fn the_status_values_are_fixed() {
    let now = current_pool();
    let values: Vec<(String, i32)> = now
        .get_enum_by_name("hostkeeper.agent.v1.PrerequisiteStatus")
        .expect("the enum")
        .values()
        .map(|v| (v.name().to_owned(), v.number()))
        .collect();
    let want = [
        ("PREREQUISITE_STATUS_UNSPECIFIED", 0),
        ("PREREQUISITE_STATUS_PENDING", 1),
        ("PREREQUISITE_STATUS_PRESENT", 2),
        ("PREREQUISITE_STATUS_INSTALLING", 3),
        ("PREREQUISITE_STATUS_FAILED", 4),
        ("PREREQUISITE_STATUS_NOT_APPLICABLE", 5),
        ("PREREQUISITE_STATUS_UNKNOWN", 6),
    ];
    assert_eq!(values, want.map(|(name, n)| (name.to_owned(), n)));
    assert_eq!(agent::PrerequisiteStatus::Unknown as i32, 6);
}
