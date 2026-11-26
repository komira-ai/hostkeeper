//! What the `probe` kind adds to the maintenance wire (`hostkeeper/agent/v1/maintenance.proto`):
//! `MaintenanceKind.PROBE` (5), `MaintenanceReport.probe` (10) and `MaintenanceReport.on_demand`
//! (11), with the messages `MaintenanceProbe` and `MaintenanceProbeStep`. The committed baseline
//! (`proto/compat/baseline.binpb`) predates the maintenance messages, so the release before is
//! this build's schema with those three additions taken out ([`release_before`]): a reader of that
//! release keeps a new report's fields as ones it does not know and writes them back byte for
//! byte, reads the kind as a number it does not know, and this build reads that release's report
//! with no probe and no kind on demand. These are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

/// The file of the maintenance messages.
const MAINTENANCE_FILE: &str = "hostkeeper/agent/v1/maintenance.proto";

/// This build's schema.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// This build's schema without what the probe adds: no `PROBE` value, no fields 10 and 11 of
/// `MaintenanceReport`, and neither of the probe's messages. What a reader of the release
/// before knows.
fn release_before() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes");
    let file = set
        .file
        .iter_mut()
        .find(|file| file.name() == MAINTENANCE_FILE)
        .expect("the maintenance file");
    file.message_type
        .retain(|message| !["MaintenanceProbe", "MaintenanceProbeStep"].contains(&message.name()));
    let report = file
        .message_type
        .iter_mut()
        .find(|message| message.name() == "MaintenanceReport")
        .expect("MaintenanceReport");
    let before = report.field.len();
    report.field.retain(|field| field.number() < 10);
    assert_eq!(before - report.field.len(), 2, "fields 10 and 11 taken out");
    let kind = file
        .enum_type
        .iter_mut()
        .find(|kind| kind.name() == "MaintenanceKind")
        .expect("MaintenanceKind");
    kind.value.retain(|value| value.number() != 5);
    DescriptorPool::decode(set.encode_to_vec().as_slice()).expect("the older set decodes")
}

/// The numbers of the fields `read` kept as unknown, each once.
fn kept(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// A Mac's report after a probe, as this build's daemon sends it.
fn probed() -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        looked_unix: 1_800_000_000,
        probe: Some(agent::MaintenanceProbe {
            run_id: "run-1800000000-1".to_owned(),
            probed_unix: 1_800_000_100,
            steps: vec![
                agent::MaintenanceProbeStep {
                    name: "console".to_owned(),
                    exit_code: Some(0),
                    output: "root".to_owned(),
                    cut: false,
                },
                agent::MaintenanceProbeStep {
                    name: "who".to_owned(),
                    exit_code: None,
                    output: String::new(),
                    cut: false,
                },
            ],
        }),
        on_demand: vec![agent::MaintenanceKind::Probe.into()],
        ..agent::MaintenanceReport::default()
    }
}

/// Catches: a report with a probe and the kinds on demand that a coordinator of the release
/// before refuses, drops, reads its other fields of differently, or writes back changed (it keeps
/// the report it read, and shows it). Proves the tags on the wire: field 10 (`0x52`) and field 11
/// (`0x5a`, packed) of `MaintenanceReport`.
#[test]
fn an_older_reader_keeps_a_probe_and_the_kinds_on_demand() {
    let older = release_before();
    let report_d = older
        .get_message_by_name("hostkeeper.agent.v1.MaintenanceReport")
        .expect("MaintenanceReport");
    assert!(report_d.get_field_by_name("probe").is_none());
    assert!(report_d.get_field_by_name("on_demand").is_none());
    let report = probed();
    let bytes = report.encode_to_vec();
    assert_eq!(
        agent::MaintenanceReport::decode(bytes.as_slice()).expect("decodes"),
        report
    );
    assert!(bytes.contains(&0x52));
    assert!(bytes.windows(3).any(|w| w == [0x5a, 1, 5]));
    let read = DynamicMessage::decode(report_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [10, 11]);
    assert_eq!(
        read.get_field_by_name("looked_unix").as_deref(),
        Some(&Value::U64(1_800_000_000))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an order of the new kind that a daemon of the release before reads as another kind:
/// it reads the number 5, which its `MaintenanceKind` does not name, so its hook refuses the
/// parameters (`UnknownKind`) rather than apply something else. The coordinator never sends it
/// one: such a daemon lists no `on_demand`.
#[test]
fn an_older_reader_reads_the_probe_kind_as_a_number_it_does_not_know() {
    let older = release_before();
    let kind = older
        .get_enum_by_name("hostkeeper.agent.v1.MaintenanceKind")
        .expect("MaintenanceKind");
    assert!(kind.get_value(5).is_none());
    let maintain = agent::Maintain {
        run_id: "run-1".to_owned(),
        kinds: vec![agent::MaintenanceKind::Probe.into()],
        ..agent::Maintain::default()
    };
    let bytes = maintain.encode_to_vec();
    let read = DynamicMessage::decode(
        older
            .get_message_by_name("hostkeeper.agent.v1.Maintain")
            .expect("Maintain"),
        bytes.as_slice(),
    )
    .expect("decodes");
    assert_eq!(
        read.get_field_by_name("kinds").as_deref(),
        Some(&Value::List(vec![Value::EnumNumber(5)]))
    );
}

/// Catches: a report of the release before read with a probe or a kind on demand nobody wrote:
/// it reads with none, and the fields it had as they were written.
#[test]
fn the_release_befores_reports_read_without_a_probe() {
    let older = release_before();
    let mut report = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.agent.v1.MaintenanceReport")
            .expect("MaintenanceReport"),
    );
    report.set_field_by_name("looked_unix", Value::U64(1_800_000_000));
    report.set_field_by_name("look_failed", Value::String("boot: exit 1".to_owned()));
    let read =
        agent::MaintenanceReport::decode(report.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.probe, None);
    assert!(read.on_demand.is_empty());
    assert_eq!(read.looked_unix, 1_800_000_000);
    assert_eq!(read.look_failed, "boot: exit 1");
}

/// Catches: a step that did not exit by itself read as one that exited 0, or a 0 lost on the
/// wire: `exit_code` keeps presence, so an exit of 0 is written and an absent one is not.
#[test]
fn an_exit_of_zero_is_distinct_from_none() {
    let exited = agent::MaintenanceProbeStep {
        exit_code: Some(0),
        ..agent::MaintenanceProbeStep::default()
    };
    assert_eq!(exited.encode_to_vec(), [0x10, 0]);
    assert_eq!(
        agent::MaintenanceProbeStep::decode(&[][..])
            .expect("decodes")
            .exit_code,
        None
    );
}

/// Catches: a new field or value given another number, label or type than the schema says, which
/// a peer built apart would read as something else.
#[test]
fn the_new_fields_and_the_kind_have_their_numbers_and_types() {
    let now = current_pool();
    let report = now
        .get_message_by_name("hostkeeper.agent.v1.MaintenanceReport")
        .expect("MaintenanceReport");
    let probe = report.get_field_by_name("probe").expect("probe");
    assert_eq!(
        (probe.number(), probe.supports_presence(), probe.is_list()),
        (10, true, false)
    );
    let on_demand = report.get_field_by_name("on_demand").expect("on_demand");
    assert_eq!((on_demand.number(), on_demand.is_list()), (11, true));
    let step = now
        .get_message_by_name("hostkeeper.agent.v1.MaintenanceProbeStep")
        .expect("MaintenanceProbeStep");
    let fields: Vec<(String, u32, bool)> = step
        .fields()
        .map(|f| (f.name().to_owned(), f.number(), f.supports_presence()))
        .collect();
    assert_eq!(
        fields,
        [
            ("name".to_owned(), 1, false),
            ("exit_code".to_owned(), 2, true),
            ("output".to_owned(), 3, false),
            ("cut".to_owned(), 4, false),
        ]
    );
    let kind = now
        .get_enum_by_name("hostkeeper.agent.v1.MaintenanceKind")
        .expect("MaintenanceKind");
    assert_eq!(
        kind.get_value_by_name("MAINTENANCE_KIND_PROBE")
            .expect("PROBE")
            .number(),
        5
    );
    assert_eq!(agent::MaintenanceKind::Probe as i32, 5);
}
