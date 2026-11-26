//! What host health adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before), docs/design/host-health-alerts.md
//! section 4.1: the heartbeat's `health` (21) with `HostHealth`, `HardwareEvent`, `Baseline` and
//! `SourceState`; `ServiceBeat.health_reason`, `health_body` and `restarts` (16 to 18);
//! `HeartbeatResponse.health_written` (10); `MAINTENANCE_KIND_REPAIR` (6); the operator API's
//! `Alert` fields 15 to 19 and `AlertKind` 25 to 32; and `InventoryRecord.installed_memory_bytes`
//! and `memory_modules` (18, 19). A reader of the release before keeps each field as one it does
//! not know, and each new enum number as the number, and writes them back byte for byte; this
//! build reads what that release wrote with none of them. Those are the tests of rule B8 in both
//! directions. The bounds are tested in `additions_host_health_bounds.rs`.
//!
//! The committed baseline predates `Alert`, `AlertKind` and `MaintenanceKind`, so the reader of
//! the release before is [`release_before`]: this build's descriptors with exactly these additions
//! taken out. The baseline still proves the new messages and enums are unknown to it.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

#[path = "support/host_health.rs"]
mod host_health;
use host_health::{fullest, incident};

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set this build serves.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// What a reader of the release before knows: this build's descriptors less the fields and enum
/// values this change adds (`HeartbeatRequest` 21, `ServiceBeat` 16 to 18, `HeartbeatResponse` 10,
/// `Alert` 15 to 19, `InventoryRecord` 18 and 19, `AlertKind` 25 to 32, `MaintenanceKind` 6).
fn release_before() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes");
    for file in &mut set.file {
        let package = file.package().to_owned();
        for message in &mut file.message_type {
            let keep: fn(i32) -> bool = match (package.as_str(), message.name()) {
                ("hostkeeper.agent.v1", "HeartbeatRequest") => |n| n != 21,
                ("hostkeeper.agent.v1", "ServiceBeat") => |n| n < 16,
                ("hostkeeper.agent.v1", "HeartbeatResponse") => |n| n != 10,
                ("hostkeeper.operator.v1", "Alert") => |n| n < 15,
                ("hostkeeper.operator.v1", "InventoryRecord") => |n| n < 18,
                _ => |_| true,
            };
            message.field.retain(|field| keep(field.number()));
        }
        for kinds in &mut file.enum_type {
            let keep: fn(i32) -> bool = match (package.as_str(), kinds.name()) {
                ("hostkeeper.operator.v1", "AlertKind") => |n| n < 25,
                ("hostkeeper.agent.v1", "MaintenanceKind") => |n| n < 6,
                _ => |_| true,
            };
            kinds.value.retain(|value| keep(value.number()));
        }
    }
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// The numbers of the fields `read` kept as unknown, each once.
fn kept(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// Every message the release before does not know, by name.
const NEW_MESSAGES: [&str; 8] = [
    "hostkeeper.agent.v1.HostHealth",
    "hostkeeper.agent.v1.HardwareEvent",
    "hostkeeper.agent.v1.Baseline",
    "hostkeeper.agent.v1.MemoryDevice",
    "hostkeeper.agent.v1.BaselineDisk",
    "hostkeeper.agent.v1.BaselineNic",
    "hostkeeper.agent.v1.SourceState",
    "hostkeeper.agent.v1.ServiceRestarts",
];

/// Every enum the release before does not know, by name.
const NEW_ENUMS: [&str; 4] = [
    "hostkeeper.agent.v1.HardwareComponent",
    "hostkeeper.agent.v1.HardwareSeverity",
    "hostkeeper.agent.v1.HardwareEventState",
    "hostkeeper.agent.v1.SourceStatus",
];

/// A service of a crash loop, with its health reason, body and restarts.
fn crash_looping() -> agent::ServiceBeat {
    agent::ServiceBeat {
        name: "js-agent".to_owned(),
        version: "1.4.0".to_owned(),
        health: agent::Health::Unhealthy.into(),
        health_reason: Some("health_url_status".to_owned()),
        health_body: Some("503 not placeable: memory short".to_owned()),
        restarts: Some(agent::ServiceRestarts {
            count: 89,
            recent: 7,
            last_result: "exit-code".to_owned(),
            exit_status: Some(1),
        }),
        ..agent::ServiceBeat::default()
    }
}

/// Catches: a heartbeat with host health, or with a service's health facts, that a coordinator of
/// the release before refuses, drops, reads other fields of differently, or writes back changed.
/// Proves the tags on the wire: field 21 of the request (`0xaa 0x01`), fields 16 to 18 of a
/// service (`0x82 0x01`, `0x8a 0x01`, `0x92 0x01`), and that the fields beside them stay as
/// written.
#[test]
fn an_older_coordinator_keeps_a_heartbeats_health() {
    let baseline = baseline_pool();
    for name in NEW_MESSAGES {
        assert!(baseline.get_message_by_name(name).is_none(), "{name}");
    }
    for name in NEW_ENUMS {
        assert!(baseline.get_enum_by_name(name).is_none(), "{name}");
    }
    let older = release_before();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("health").is_none());
    let service_d = message(&older, "hostkeeper.agent.v1.ServiceBeat");
    for name in ["health_reason", "health_body", "restarts"] {
        assert!(service_d.get_field_by_name(name).is_none(), "{name}");
    }
    for health in [incident(), fullest()] {
        let beat = agent::HeartbeatRequest {
            platform: agent::Platform::LinuxX8664.into(),
            features: vec!["host-health-v1".to_owned()],
            services: vec![crash_looping()],
            health: Some(health),
            ..agent::HeartbeatRequest::default()
        };
        let bytes = beat.encode_to_vec();
        assert_eq!(
            agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
            beat
        );
        let alone = agent::HeartbeatRequest {
            health: beat.health.clone(),
            ..agent::HeartbeatRequest::default()
        }
        .encode_to_vec();
        assert_eq!(alone[..2], [0xaa, 0x01], "field 21, length-delimited");
        let facts = crash_looping();
        let only = |service: agent::ServiceBeat| service.encode_to_vec()[..2].to_vec();
        let tags = [
            only(agent::ServiceBeat {
                health_reason: facts.health_reason.clone(),
                ..agent::ServiceBeat::default()
            }),
            only(agent::ServiceBeat {
                health_body: facts.health_body.clone(),
                ..agent::ServiceBeat::default()
            }),
            only(agent::ServiceBeat {
                restarts: facts.restarts.clone(),
                ..agent::ServiceBeat::default()
            }),
        ];
        assert_eq!(tags, [[0x82, 0x01], [0x8a, 0x01], [0x92, 0x01]]);

        let read = DynamicMessage::decode(beat_d.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(kept(&read), [21]);
        let Some(Value::List(services)) = read.get_field_by_name("services").as_deref().cloned()
        else {
            panic!("the services");
        };
        let [Value::Message(service)] = services.as_slice() else {
            panic!("one service");
        };
        assert_eq!(kept(service), [16, 17, 18]);
        assert_eq!(
            service.get_field_by_name("name").as_deref(),
            Some(&Value::String("js-agent".to_owned()))
        );
        assert_eq!(
            service.get_field_by_name("health").as_deref(),
            Some(&Value::EnumNumber(agent::Health::Unhealthy.into()))
        );
        assert_eq!(
            read.get_field_by_name("features").as_deref(),
            Some(&Value::List(vec![Value::String(
                "host-health-v1".to_owned()
            )]))
        );
        assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
    }
}

/// Catches: an answer naming the records written that a daemon of the release before refuses, or
/// whose order or features it reads differently. Proves the tag on the wire: field 10 of the
/// answer (`0x52`).
#[test]
fn an_older_daemon_keeps_the_records_written() {
    let older = release_before();
    let answer_d = message(&older, "hostkeeper.agent.v1.HeartbeatResponse");
    assert!(answer_d.get_field_by_name("health_written").is_none());
    let answer = agent::HeartbeatResponse {
        features: vec!["host-health-v1".to_owned()],
        health_written: vec!["sel:01a4@1790000000".to_owned(), "panic:2b6f".to_owned()],
        ..agent::HeartbeatResponse::default()
    };
    let bytes = answer.encode_to_vec();
    assert_eq!(
        agent::HeartbeatResponse::decode(bytes.as_slice()).expect("decodes"),
        answer
    );
    let alone = agent::HeartbeatResponse {
        health_written: vec!["e".to_owned()],
        ..agent::HeartbeatResponse::default()
    };
    assert_eq!(
        alone.encode_to_vec(),
        [0x52, 0x01, b'e'],
        "field 10, length-delimited"
    );
    let read = DynamicMessage::decode(answer_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [10]);
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String(
            "host-health-v1".to_owned()
        )]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a `repair` kind that a daemon or coordinator of the release before reads as another
/// kind, or drops: in a `maintain` order's kinds and a run's applied kinds it is the number 6,
/// which that release has no value for, kept and written back.
#[test]
fn an_older_reader_keeps_the_repair_kind_as_its_number() {
    let older = release_before();
    let kind = older
        .get_enum_by_name("hostkeeper.agent.v1.MaintenanceKind")
        .expect("the kinds");
    assert!(
        kind.get_value(6).is_none(),
        "6 is no kind of the release before"
    );
    let maintain = agent::Maintain {
        run_id: "run-1".to_owned(),
        action: agent::MaintainAction::Run.into(),
        kinds: vec![agent::MaintenanceKind::Repair.into()],
        ..agent::Maintain::default()
    };
    let bytes = maintain.encode_to_vec();
    let read = DynamicMessage::decode(message(&older, "hostkeeper.agent.v1.Maintain"), &*bytes)
        .expect("decodes");
    assert_eq!(
        read.get_field_by_name("kinds").as_deref(),
        Some(&Value::List(vec![Value::EnumNumber(6)]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an alert with the new fields or of a new kind that an operator client of the release
/// before (an older `hk` or console) refuses, or reads other fields of differently: the fields
/// 15 to 19 are kept as unknown, each new kind is its number, and the alert writes back byte for
/// byte. An inventory record with the installed memory likewise keeps 18 and 19.
#[test]
fn an_older_operator_client_keeps_the_alerts_and_records_fields() {
    let older = release_before();
    let alert_d = message(&older, "hostkeeper.operator.v1.Alert");
    let kinds = older
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("the kinds");
    for number in 25..=32 {
        assert!(kinds.get_value(number).is_none(), "{number}");
        let alert = op::Alert {
            kind: number,
            severity: op::AlertSeverity::Critical.into(),
            host_id: "host-a".to_owned(),
            message: "uncorrectable memory error on DIMM D1".to_owned(),
            last_seen_unix: 1_795_662_540,
            acknowledged: true,
            likely_cause: "memory 16 GiB short since the reboot".to_owned(),
            events: vec!["sel:01a4@1790000000".to_owned()],
            finding: "memory_uncorrectable/DIMM D1".to_owned(),
            ..op::Alert::default()
        };
        let bytes = alert.encode_to_vec();
        let read = DynamicMessage::decode(alert_d.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(kept(&read), [15, 16, 17, 18, 19]);
        assert_eq!(
            read.get_field_by_name("kind").as_deref(),
            Some(&Value::EnumNumber(number))
        );
        assert_eq!(
            read.get_field_by_name("host_id").as_deref(),
            Some(&Value::String("host-a".to_owned()))
        );
        assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
    }

    let record = op::InventoryRecord {
        host_id: "host-a".to_owned(),
        installed_memory_bytes: 256 << 30,
        memory_modules: 16,
        ..op::InventoryRecord::default()
    };
    let bytes = record.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&older, "hostkeeper.operator.v1.InventoryRecord"),
        bytes.as_slice(),
    )
    .expect("decodes");
    assert_eq!(kept(&read), [18, 19]);
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: this build reading what the release before wrote as something it is not: a heartbeat
/// with no health reads as none (never an empty one), a service with no health facts reads none
/// of them, an answer names no record written, an alert has none of the new fields, and a record
/// no installed memory.
#[test]
fn the_release_befores_messages_read_without_health() {
    let older = release_before();
    let mut service = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.ServiceBeat"));
    service.set_field_by_name("name", Value::String("js-agent".to_owned()));
    let mut beat = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    beat.set_field_by_name(
        "features",
        Value::List(vec![Value::String("maintenance-v1".to_owned())]),
    );
    beat.set_field_by_name("services", Value::List(vec![Value::Message(service)]));
    let read = agent::HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.health, None, "absent: no health");
    assert_eq!(read.features, ["maintenance-v1"]);
    let [service] = read.services.as_slice() else {
        panic!("one service");
    };
    assert_eq!(service.name, "js-agent");
    assert_eq!(
        (
            &service.health_reason,
            &service.health_body,
            &service.restarts
        ),
        (&None, &None, &None)
    );

    let mut answer = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatResponse"));
    answer.set_field_by_name(
        "features",
        Value::List(vec![Value::String("maintenance-v1".to_owned())]),
    );
    let read =
        agent::HeartbeatResponse::decode(answer.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.health_written.is_empty());

    let mut alert = DynamicMessage::new(message(&older, "hostkeeper.operator.v1.Alert"));
    alert.set_field_by_name("kind", Value::EnumNumber(8));
    alert.set_field_by_name("host_id", Value::String("host-a".to_owned()));
    let read = op::Alert::decode(alert.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read,
        op::Alert {
            kind: op::AlertKind::SilentHost.into(),
            host_id: "host-a".to_owned(),
            ..op::Alert::default()
        }
    );

    let mut record = DynamicMessage::new(message(&older, "hostkeeper.operator.v1.InventoryRecord"));
    record.set_field_by_name("host_id", Value::String("host-a".to_owned()));
    let read = op::InventoryRecord::decode(record.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        (read.installed_memory_bytes, read.memory_modules),
        (0, 0),
        "no installed memory known"
    );
}

/// Catches: an empty health read as no health, or none as an empty one: a coordinator would read
/// a daemon that sent nothing as one whose sources all reported nothing.
#[test]
fn the_empty_health_is_distinct_from_absent() {
    let reported = agent::HeartbeatRequest {
        health: Some(agent::HostHealth::default()),
        ..agent::HeartbeatRequest::default()
    };
    assert_eq!(reported.encode_to_vec(), [0xaa, 0x01, 0]);
    assert_eq!(
        agent::HeartbeatRequest::decode([0xaa, 0x01, 0].as_slice()).expect("decodes"),
        reported
    );
    assert_eq!(
        agent::HeartbeatRequest::decode(&[][..])
            .expect("decodes")
            .health,
        None
    );
}

/// Catches: a number this build does not know (a later daemon's component, severity, state or
/// source status) read as a known one, or dropped: it reads as the number it is and is written
/// back, so a later daemon's record reaches the log as it came.
#[test]
fn an_enum_number_this_build_does_not_know_is_kept() {
    let bytes = [0x20, 0x63, 0x30, 0x63, 0x38, 0x63];
    let event = agent::HardwareEvent::decode(bytes.as_slice()).expect("decodes");
    assert_eq!((event.component, event.severity, event.state), (99, 99, 99));
    assert_eq!(agent::HardwareComponent::try_from(99).ok(), None);
    assert_eq!(agent::HardwareSeverity::try_from(99).ok(), None);
    assert_eq!(agent::HardwareEventState::try_from(99).ok(), None);
    assert_eq!(event.encode_to_vec(), bytes);
    let source = agent::SourceState::decode([0x10, 0x63].as_slice()).expect("decodes");
    assert_eq!(source.status, 99);
    assert_eq!(agent::SourceStatus::try_from(99).ok(), None);
    assert_eq!(source.encode_to_vec(), [0x10, 0x63]);
}

/// The fields of the message `name` of `pool`, by name, number and kind (a message or an enum
/// by its full name), and whether each is a list.
fn fields(pool: &DescriptorPool, name: &str) -> Vec<(String, u32, String, bool)> {
    message(pool, name)
        .fields()
        .map(|f| {
            let kind = match f.kind() {
                Kind::Message(m) => m.full_name().to_owned(),
                Kind::Enum(e) => e.full_name().to_owned(),
                other => format!("{other:?}"),
            };
            (f.name().to_owned(), f.number(), kind, f.is_list())
        })
        .collect()
}

/// Catches: a new field given another number, label or type than the schema says, which a peer
/// built apart would read as something else.
#[test]
fn the_health_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let f = |name: &str, number: u32, kind: &str, list: bool| {
        (name.to_owned(), number, kind.to_owned(), list)
    };
    let a = |name: &str| format!("hostkeeper.agent.v1.{name}");
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.HostHealth"),
        [
            f("events", 1, &a("HardwareEvent"), true),
            f("baseline", 2, &a("Baseline"), false),
            f("boot_id", 3, "string", false),
            f("boot_unix", 4, "uint64", false),
            f("clean_previous_shutdown", 5, "bool", false),
            f("sources", 6, &a("SourceState"), true),
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.HardwareEvent"),
        [
            f("event_id", 1, "string", false),
            f("time_unix", 2, "uint64", false),
            f("of_previous_boot", 3, "bool", false),
            f("component", 4, &a("HardwareComponent"), false),
            f("subject", 5, "string", false),
            f("severity", 6, &a("HardwareSeverity"), false),
            f("state", 7, &a("HardwareEventState"), false),
            f("source", 8, "string", false),
            f("code", 9, "string", false),
            f("message", 10, "string", false),
            f("raw", 11, "string", false),
            f("about", 12, "string", false),
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.Baseline"),
        [
            f("mem_total_kib", 1, "uint64", false),
            f("memory_devices", 2, &a("MemoryDevice"), true),
            f("cpus_present", 3, "uint32", false),
            f("cpus_online", 4, "uint32", false),
            f("smt", 5, "string", false),
            f("disks", 6, &a("BaselineDisk"), true),
            f("nics", 7, &a("BaselineNic"), true),
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.MemoryDevice"),
        [
            f("locator", 1, "string", false),
            f("size_bytes", 2, "uint64", false)
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.BaselineDisk"),
        [
            f("name", 1, "string", false),
            f("size_bytes", 2, "uint64", false),
            f("serial_digest", 3, "fixed64", false),
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.BaselineNic"),
        [
            f("name", 1, "string", false),
            f("speed_mbps", 2, "uint32", false)
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.SourceState"),
        [
            f("source", 1, "string", false),
            f("status", 2, &a("SourceStatus"), false),
            f("reason", 3, "string", false),
        ]
    );
    assert_eq!(
        fields(&now, "hostkeeper.agent.v1.ServiceRestarts"),
        [
            f("count", 1, "uint32", false),
            f("recent", 2, "uint32", false),
            f("last_result", 3, "string", false),
            f("exit_status", 4, "int32", false),
        ]
    );
    let beat = fields(&now, "hostkeeper.agent.v1.HeartbeatRequest");
    assert_eq!(beat.last(), Some(&f("health", 21, &a("HostHealth"), false)));
    let service = fields(&now, "hostkeeper.agent.v1.ServiceBeat");
    assert_eq!(
        service[service.len() - 3..],
        [
            f("health_reason", 16, "string", false),
            f("health_body", 17, "string", false),
            f("restarts", 18, &a("ServiceRestarts"), false),
        ]
    );
    let answer = fields(&now, "hostkeeper.agent.v1.HeartbeatResponse");
    assert_eq!(
        answer.last(),
        Some(&f("health_written", 10, "string", true))
    );
    let alert = fields(&now, "hostkeeper.operator.v1.Alert");
    assert_eq!(
        alert[alert.len() - 5..],
        [
            f("last_seen_unix", 15, "uint64", false),
            f("acknowledged", 16, "bool", false),
            f("likely_cause", 17, "string", false),
            f("events", 18, "string", true),
            f("finding", 19, "string", false),
        ]
    );
    let record = fields(&now, "hostkeeper.operator.v1.InventoryRecord");
    assert_eq!(
        record[record.len() - 2..],
        [
            f("installed_memory_bytes", 18, "uint64", false),
            f("memory_modules", 19, "uint32", false),
        ]
    );
}

/// The values of the enum `name` of `pool`, by name and number.
fn values(pool: &DescriptorPool, name: &str) -> Vec<(String, i32)> {
    pool.get_enum_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
        .values()
        .map(|v| (v.name().to_owned(), v.number()))
        .collect()
}

/// Catches: a value renumbered or renamed, or one inserted before the others: the numbers are on
/// the wire in every record, and the names are what the operator API's JSON will show.
#[test]
fn the_enum_values_are_fixed() {
    let now = current_pool();
    let want: [(&str, &[&str]); 4] = [
        (
            "HardwareComponent",
            &[
                "HARDWARE_COMPONENT_UNSPECIFIED",
                "HARDWARE_COMPONENT_MEMORY",
                "HARDWARE_COMPONENT_CPU",
                "HARDWARE_COMPONENT_PCIE",
                "HARDWARE_COMPONENT_DISK",
                "HARDWARE_COMPONENT_THERMAL",
                "HARDWARE_COMPONENT_POWER",
                "HARDWARE_COMPONENT_BOARD",
                "HARDWARE_COMPONENT_HOST",
            ],
        ),
        (
            "HardwareSeverity",
            &[
                "HARDWARE_SEVERITY_UNSPECIFIED",
                "HARDWARE_SEVERITY_INFO",
                "HARDWARE_SEVERITY_WARNING",
                "HARDWARE_SEVERITY_CRITICAL",
            ],
        ),
        (
            "HardwareEventState",
            &[
                "HARDWARE_EVENT_STATE_UNSPECIFIED",
                "HARDWARE_EVENT_STATE_EVENT",
                "HARDWARE_EVENT_STATE_ASSERTED",
                "HARDWARE_EVENT_STATE_DEASSERTED",
            ],
        ),
        (
            "SourceStatus",
            &[
                "SOURCE_STATUS_UNSPECIFIED",
                "SOURCE_STATUS_OK",
                "SOURCE_STATUS_UNSUPPORTED",
                "SOURCE_STATUS_UNREADABLE",
            ],
        ),
    ];
    for (name, names) in want {
        let numbered: Vec<(String, i32)> = names
            .iter()
            .zip(0..)
            .map(|(value, n)| ((*value).to_owned(), n))
            .collect();
        assert_eq!(
            values(&now, &format!("hostkeeper.agent.v1.{name}")),
            numbered,
            "{name}"
        );
    }
    assert_eq!(
        values(&now, "hostkeeper.agent.v1.MaintenanceKind").last(),
        Some(&("MAINTENANCE_KIND_REPAIR".to_owned(), 6))
    );
    let kinds = values(&now, "hostkeeper.operator.v1.AlertKind");
    assert_eq!(
        kinds[kinds.len() - 8..],
        [
            ("ALERT_KIND_HARDWARE_FAULT".to_owned(), 25),
            ("ALERT_KIND_CAPACITY_REGRESSED".to_owned(), 26),
            ("ALERT_KIND_STORAGE_FAILING".to_owned(), 27),
            ("ALERT_KIND_UNEXPECTED_REBOOT".to_owned(), 28),
            ("ALERT_KIND_SERVICE_UNHEALTHY".to_owned(), 29),
            ("ALERT_KIND_SERVICE_CRASH_LOOP".to_owned(), 30),
            ("ALERT_KIND_HOST_NOT_MONITORED".to_owned(), 31),
            ("ALERT_KIND_BMC_MISSING".to_owned(), 32),
        ]
    );
}
