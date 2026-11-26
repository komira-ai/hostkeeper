//! What the archive check of a `probe` run adds to the operator's maintenance wire
//! (`hostkeeper/operator/v1/maintenance.proto`): `ScheduleMaintenanceRequest.xcode_sha256` (11)
//! and `MaintenanceRun.xcode_sha256` (17). The release before is this build's schema with those
//! two fields taken out ([`release_before`]): its coordinator keeps a request's field as one it
//! does not know, and so stores a run without it (a run of plain probes, which `hk` sees in the
//! answer's run and cancels); its `hk` keeps a run's field the same way; and this build reads that
//! release's request and run with no archive. The daemon's order needs no new field: the archive
//! goes in `Maintain.xcodes` (7), which a daemon of the release before reads and its probe ignores.
//! These are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as operator;

/// The file of the operator's maintenance messages.
const MAINTENANCE_FILE: &str = "hostkeeper/operator/v1/maintenance.proto";

/// A SHA-256 as the store names an archive.
const SHA256: &str = "3f1c00000000000000000000000000000000000000000000000000000000e09a";

/// This build's schema.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// This build's schema without the field `number` of the message `message`.
fn without(set: &mut FileDescriptorSet, message: &str, number: i32) {
    let file = set
        .file
        .iter_mut()
        .find(|file| file.name() == MAINTENANCE_FILE)
        .expect("the maintenance file");
    let found = file
        .message_type
        .iter_mut()
        .find(|found| found.name() == message)
        .expect("the message");
    let before = found.field.len();
    found.field.retain(|field| field.number() != number);
    assert_eq!(before - found.field.len(), 1, "{message}.{number} taken out");
}

/// This build's schema without the two fields: what a peer of the release before knows.
fn release_before() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes");
    without(&mut set, "ScheduleMaintenanceRequest", 11);
    without(&mut set, "MaintenanceRun", 17);
    DescriptorPool::decode(set.encode_to_vec().as_slice()).expect("the older set decodes")
}

/// The numbers of the fields `read` kept as unknown, each once.
fn kept(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// Catches: a request naming an archive that a coordinator of the release before refuses or
/// reads other fields of differently: it keeps field 11 (tag `0x5a`) as unknown, reads the
/// probe and the host as they were sent, and writes it back byte for byte.
#[test]
fn an_older_coordinator_keeps_the_archive_of_a_request_as_unknown() {
    let older = release_before();
    let request_d = older
        .get_message_by_name("hostkeeper.operator.v1.ScheduleMaintenanceRequest")
        .expect("ScheduleMaintenanceRequest");
    assert!(request_d.get_field_by_name("xcode_sha256").is_none());
    let request = operator::ScheduleMaintenanceRequest {
        kinds: vec![hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind::Probe.into()],
        host_ids: vec!["mac-01".to_owned()],
        confirm: true,
        reason: "check Xcode 27.2".to_owned(),
        xcode_sha256: SHA256.to_owned(),
        ..operator::ScheduleMaintenanceRequest::default()
    };
    let bytes = request.encode_to_vec();
    let mut tagged = vec![0x5a, 64];
    tagged.extend_from_slice(SHA256.as_bytes());
    assert!(bytes.windows(tagged.len()).any(|w| w == tagged.as_slice()));
    let read = DynamicMessage::decode(request_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [11]);
    assert_eq!(
        read.get_field_by_name("host_ids").as_deref(),
        Some(&Value::List(vec![Value::String("mac-01".to_owned())]))
    );
    assert_eq!(
        read.get_field_by_name("kinds").as_deref(),
        Some(&Value::List(vec![Value::EnumNumber(5)]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a run naming an archive that an `hk` of the release before refuses or reads
/// differently: it keeps field 17 (tag `0x8a 0x01`) as unknown and the run's other fields as
/// they were.
#[test]
fn an_older_reader_keeps_the_archive_of_a_run_as_unknown() {
    let older = release_before();
    let run_d = older
        .get_message_by_name("hostkeeper.operator.v1.MaintenanceRun")
        .expect("MaintenanceRun");
    assert!(run_d.get_field_by_name("xcode_sha256").is_none());
    let run = operator::MaintenanceRun {
        id: "run-1800000000-1".to_owned(),
        host_ids: vec!["mac-01".to_owned()],
        xcode_sha256: SHA256.to_owned(),
        ..operator::MaintenanceRun::default()
    };
    let bytes = run.encode_to_vec();
    assert!(bytes.windows(3).any(|w| w == [0x8a, 0x01, 64]));
    let read = DynamicMessage::decode(run_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [17]);
    assert_eq!(
        read.get_field_by_name("id").as_deref(),
        Some(&Value::String("run-1800000000-1".to_owned()))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a request or a run of the release before read with an archive nobody named: both
/// read with an empty `xcode_sha256`, a run of plain probes.
#[test]
fn the_release_befores_request_and_run_read_without_an_archive() {
    let older = release_before();
    let mut request = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.operator.v1.ScheduleMaintenanceRequest")
            .expect("ScheduleMaintenanceRequest"),
    );
    request.set_field_by_name("host_ids", Value::List(vec![Value::String("mac-01".into())]));
    let read = operator::ScheduleMaintenanceRequest::decode(request.encode_to_vec().as_slice())
        .expect("decodes");
    assert_eq!(read.xcode_sha256, "");
    assert_eq!(read.host_ids, ["mac-01"]);
    let mut run = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.operator.v1.MaintenanceRun")
            .expect("MaintenanceRun"),
    );
    run.set_field_by_name("id", Value::String("run-1".into()));
    let read =
        operator::MaintenanceRun::decode(run.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.xcode_sha256, "");
    assert_eq!(read.id, "run-1");
}

/// Catches: a new field given another number or type than the schema says, which a peer built
/// apart would read as something else.
#[test]
fn the_new_fields_have_their_numbers_and_types() {
    let now = current_pool();
    for (message, number) in [
        ("hostkeeper.operator.v1.ScheduleMaintenanceRequest", 11),
        ("hostkeeper.operator.v1.MaintenanceRun", 17),
    ] {
        let field = now
            .get_message_by_name(message)
            .expect("the message")
            .get_field_by_name("xcode_sha256")
            .expect("xcode_sha256");
        assert_eq!(
            (
                field.number(),
                field.is_list(),
                field.supports_presence(),
                field.kind().as_message().is_none()
            ),
            (number, false, false, true),
            "{message}"
        );
        assert!(matches!(field.kind(), prost_reflect::Kind::String));
    }
}
