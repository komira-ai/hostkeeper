//! What the serial console baseline adds to the wire: `HeartbeatRequest.serial_console` (20), a
//! `SerialConsoleReport` of the port, the state and the step that failed, new since the committed
//! baseline (`proto/compat/baseline.binpb`, the release before). A reader of that release keeps
//! the field as one it does not know and writes it back byte for byte; this build reads what that
//! release wrote with no report. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

fn report() -> agent::SerialConsoleReport {
    agent::SerialConsoleReport {
        port: "ttyS1".to_owned(),
        state: "failed".to_owned(),
        detail: "update_grub".to_owned(),
    }
}

/// Proves the release before keeps a heartbeat's serial console report: it has no field 20 and no
/// `SerialConsoleReport`, reads the heartbeat with the fields it knows as written, keeps 20 as an
/// unknown field, and writes the message back byte for byte.
#[test]
fn an_older_reader_keeps_a_heartbeats_serial_console_report_byte_for_byte() {
    let older = baseline_pool();
    let beat = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
        .expect("the heartbeat");
    assert!(beat.get_field_by_name("serial_console").is_none());
    assert!(beat.get_field(20).is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.SerialConsoleReport")
            .is_none()
    );

    let new = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["heartbeat-v2".to_owned()],
        serial_console: Some(report()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = new.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        new
    );
    let read = DynamicMessage::decode(beat, bytes.as_slice()).expect("the release before reads it");
    let unknown: Vec<u32> = read.unknown_fields().map(|field| field.number()).collect();
    assert_eq!(unknown, [20]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::LinuxX8664 as i32))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Proves a heartbeat of the release before reads in this build with no serial console report and
/// with the fields it wrote unchanged.
#[test]
fn a_heartbeat_of_the_release_before_reads_without_a_serial_console_report() {
    let older = baseline_pool();
    let mut written = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
            .expect("the heartbeat"),
    );
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxArm64 as i32),
    );
    written.set_field_by_name(
        "features",
        Value::List(vec![Value::String("heartbeat-v2".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(written.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(read.serial_console, None);
    assert_eq!(read.platform(), agent::Platform::LinuxArm64);
    assert_eq!(read.features, ["heartbeat-v2"]);
}

/// Proves the report's fields keep their names and numbers in this build (`port` 1, `state` 2,
/// `detail` 3), as `wire.rs` pins the heartbeat's own; its field 20 is pinned there.
///
/// Catches a renumbered or renamed field of the report, which an older coordinator would read as
/// another field.
#[test]
fn the_serial_console_report_keeps_its_names_and_numbers() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let message = pool
        .get_message_by_name("hostkeeper.agent.v1.SerialConsoleReport")
        .expect("the report");
    let fields: Vec<(String, u32)> = message
        .fields()
        .map(|field| (field.name().to_owned(), field.number()))
        .collect();
    assert_eq!(
        fields,
        [
            ("port".to_owned(), 1),
            ("state".to_owned(), 2),
            ("detail".to_owned(), 3)
        ]
    );
}
