//! What the Remote Login probe adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `HeartbeatRequest.remote_login_probe`
//! (14), a `RemoteLoginProbe` of `ProbeStep`s that a Mac's daemon reports and the coordinator
//! only shows. A reader of the release before keeps the field as one it does not know and writes
//! it back byte for byte; this build reads what that release wrote with no probe. Those are the
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

/// A probe with every field of it and of its steps set.
fn probe() -> agent::RemoteLoginProbe {
    agent::RemoteLoginProbe {
        started_unix: 1_800_000_000,
        steps: vec![
            agent::ProbeStep {
                name: "remote_login_set_on".to_owned(),
                argv: vec![
                    "/usr/sbin/systemsetup".to_owned(),
                    "-setremotelogin".to_owned(),
                    "on".to_owned(),
                ],
                exit_code: Some(1),
                stdout: "out".to_owned(),
                stderr: "err".to_owned(),
                error: String::new(),
                truncated: true,
            },
            agent::ProbeStep {
                name: "ssh_banner".to_owned(),
                argv: vec!["connect 127.0.0.1:22".to_owned()],
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                error: "timed out".to_owned(),
                truncated: false,
            },
        ],
    }
}

/// Catches: a heartbeat with a probe that a coordinator of the release before refuses, drops,
/// reads other fields of differently, or writes back changed. Proves the tag on the wire: field
/// 14 (`0x72`), and that the fields beside it stay as written.
#[test]
fn an_older_reader_keeps_a_heartbeats_probe() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("remote_login_probe").is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.RemoteLoginProbe")
            .is_none()
    );
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.ProbeStep")
            .is_none()
    );
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["heartbeat-v2".to_owned()],
        remote_login_probe: Some(probe()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    assert!(bytes.contains(&0x72));

    let read = DynamicMessage::decode(beat_d, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [14]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::LinuxX8664.into()))
    );
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String("heartbeat-v2".to_owned())]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a heartbeat of a daemon of the release before read with a probe, so that a coordinator
/// shows a probe that no daemon sent; and the heartbeat's other fields lost.
#[test]
fn a_heartbeat_of_the_release_before_reads_without_a_probe() {
    let older = baseline_pool();
    let mut written = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxX8664.into()),
    );
    written.set_field_by_name(
        "features",
        Value::List(vec![Value::String("heartbeat-v2".to_owned())]),
    );
    let bytes = written.encode_to_vec();
    let read = agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read.remote_login_probe, None, "absent: no probe");
    assert_eq!(read.platform, i32::from(agent::Platform::LinuxX8664));
    assert_eq!(read.features, ["heartbeat-v2"]);
}

/// Catches: a probe that has run and found nothing (no steps, started at 0) that reads as absent,
/// so that a coordinator cannot tell a daemon that probed from one that did not; and a step with
/// an exit code of 0 that reads as one that ran nothing.
#[test]
fn an_empty_probe_and_a_zero_exit_are_distinct_from_absent() {
    let absent = agent::HeartbeatRequest::default();
    let empty = agent::HeartbeatRequest {
        remote_login_probe: Some(agent::RemoteLoginProbe::default()),
        ..agent::HeartbeatRequest::default()
    };
    assert_eq!(empty.encode_to_vec(), [0x72, 0]);
    assert_eq!(absent.encode_to_vec(), Vec::<u8>::new());
    assert_eq!(
        agent::HeartbeatRequest::decode([0x72, 0].as_slice()).expect("decodes"),
        empty
    );

    let zero = agent::ProbeStep {
        exit_code: Some(0),
        ..agent::ProbeStep::default()
    };
    assert_eq!(zero.encode_to_vec(), [0x18, 0]);
    let read = agent::ProbeStep::decode([0x18, 0].as_slice()).expect("decodes");
    assert_eq!(read.exit_code, Some(0));
    assert_eq!(
        agent::ProbeStep::decode(&[][..])
            .expect("decodes")
            .exit_code,
        None
    );
    let negative = agent::ProbeStep {
        exit_code: Some(-1),
        ..agent::ProbeStep::default()
    };
    let read = agent::ProbeStep::decode(negative.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.exit_code, Some(-1));
}

/// Catches: a new field or message given another number, label or type than the schema says,
/// which a peer built apart would read as something else.
#[test]
fn the_probe_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let probe_kind = Kind::Message(message(&now, "hostkeeper.agent.v1.RemoteLoginProbe"));
    let step_kind = Kind::Message(message(&now, "hostkeeper.agent.v1.ProbeStep"));
    let fields: [(&str, &str, u32, Kind, bool, bool); 10] = [
        (
            "hostkeeper.agent.v1.HeartbeatRequest",
            "remote_login_probe",
            14,
            probe_kind,
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.RemoteLoginProbe",
            "started_unix",
            1,
            Kind::Uint64,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.RemoteLoginProbe",
            "steps",
            2,
            step_kind,
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "name",
            1,
            Kind::String,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "argv",
            2,
            Kind::String,
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "exit_code",
            3,
            Kind::Int32,
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "stdout",
            4,
            Kind::String,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "stderr",
            5,
            Kind::String,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "error",
            6,
            Kind::String,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.ProbeStep",
            "truncated",
            7,
            Kind::Bool,
            false,
            false,
        ),
    ];
    for (name, field, number, kind, repeated, presence) in fields {
        let found = message(&now, name)
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
    for name in ["RemoteLoginProbe", "ProbeStep"] {
        let count = message(&now, &format!("hostkeeper.agent.v1.{name}"))
            .fields()
            .count();
        assert_eq!(count, if name == "ProbeStep" { 7 } else { 2 }, "{name}");
    }
}

/// Catches: a bound of the crate that the schema's comments give differently (a daemon cutting to
/// 4096 bytes where the schema says 1024), as the service limit once differed from its header.
#[test]
fn the_probe_bounds_are_the_schemas() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("proto/hostkeeper/agent/v1/agent.proto");
    let text = std::fs::read_to_string(path).expect("read the file");
    let comments = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("//"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(hostkeeper_proto::MAX_PROBE_STEPS, 24);
    assert_eq!(hostkeeper_proto::MAX_PROBE_ARGS, 8);
    assert_eq!(hostkeeper_proto::MAX_PROBE_TEXT, 4096);
    for sentence in [
        "Each step, in the order it ran: at most 24.",
        "At most 8, each at most 4096 bytes.",
        "Every text is at most 4096 bytes",
    ] {
        assert!(comments.contains(sentence), "{sentence}");
    }
}
