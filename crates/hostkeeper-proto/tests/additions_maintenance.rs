//! What maintenance adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `HeartbeatRequest.maintenance` (18), the
//! daemon's `MaintenanceReport` with what the host runs, its Xcodes, a Mac's update facts, the
//! items due and the run it holds; `Order.maintain` (11), the `Maintain` verb with its Xcode
//! entries; `hostkeeper.operator.v1.Host.maintenance` (26), where the coordinator can carry the
//! last report; and their enums. A reader of the release before keeps each field as one it does
//! not know and writes it back byte for byte; this build reads what that release wrote with none
//! of them. Those are the tests of rule B8 in both directions. The bounds of a report are tested
//! in `additions_maintenance_bounds.rs`.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

#[path = "support/maintenance_reports.rs"]
mod maintenance_reports;
use maintenance_reports::{at_every_bound, linux_report};

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

/// The numbers of the fields `read` kept as unknown, each once.
fn kept(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// A `maintain` order with every argument set.
fn maintain() -> agent::Maintain {
    agent::Maintain {
        run_id: "run-7".to_owned(),
        action: agent::MaintainAction::Run.into(),
        kinds: vec![
            agent::MaintenanceKind::Macos.into(),
            agent::MaintenanceKind::Xcode.into(),
        ],
        linux_scope: agent::LinuxScope::Security.into(),
        drain_deadline_secs: Some(7_200),
        macos_path: agent::MacosPath::Install.into(),
        xcodes: vec![agent::XcodeEntry {
            version: "27.2".to_owned(),
            build: "27B52".to_owned(),
            size: 3_100_000_000,
            sha256: "ab".repeat(32),
        }],
        xcode_remove: vec!["26.4".to_owned()],
        xcode_default: "27.2".to_owned(),
        return_secs: 1_800,
        macos_major: true,
    }
}

/// Every message the release before does not know, by name.
const NEW_MESSAGES: [&str; 10] = [
    "hostkeeper.agent.v1.MaintenanceReport",
    "hostkeeper.agent.v1.MaintenanceHost",
    "hostkeeper.agent.v1.InstalledXcode",
    "hostkeeper.agent.v1.MacUpdateFacts",
    "hostkeeper.agent.v1.UpdateAccountState",
    "hostkeeper.agent.v1.CommandLineToolsState",
    "hostkeeper.agent.v1.DueItem",
    "hostkeeper.agent.v1.MaintenanceRunState",
    "hostkeeper.agent.v1.Maintain",
    "hostkeeper.agent.v1.XcodeEntry",
];

/// Every enum the release before does not know, by name.
const NEW_ENUMS: [&str; 6] = [
    "hostkeeper.agent.v1.FileVaultState",
    "hostkeeper.agent.v1.MaintenanceKind",
    "hostkeeper.agent.v1.MaintenanceStep",
    "hostkeeper.agent.v1.MaintainAction",
    "hostkeeper.agent.v1.LinuxScope",
    "hostkeeper.agent.v1.MacosPath",
];

/// Catches: a heartbeat with a report that a coordinator of the release before refuses, drops,
/// reads other fields of differently, or writes back changed. Proves the tag on the wire: field
/// 18 (`0x92 0x01`), and that the fields beside it stay as written.
#[test]
fn an_older_coordinator_keeps_a_heartbeats_maintenance_report() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("maintenance").is_none());
    for name in NEW_MESSAGES {
        assert!(older.get_message_by_name(name).is_none(), "{name}");
    }
    for name in NEW_ENUMS {
        assert!(older.get_enum_by_name(name).is_none(), "{name}");
    }
    for report in [at_every_bound(), linux_report()] {
        let beat = agent::HeartbeatRequest {
            platform: agent::Platform::DarwinArm64.into(),
            features: vec!["maintenance-v1".to_owned()],
            maintenance: Some(report),
            ..agent::HeartbeatRequest::default()
        };
        let bytes = beat.encode_to_vec();
        assert_eq!(
            agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
            beat
        );
        assert!(bytes.windows(2).any(|w| w == [0x92, 0x01]));

        let read = DynamicMessage::decode(beat_d.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(kept(&read), [18]);
        assert_eq!(
            read.get_field_by_name("platform").as_deref(),
            Some(&Value::EnumNumber(agent::Platform::DarwinArm64.into()))
        );
        assert_eq!(
            read.get_field_by_name("features").as_deref(),
            Some(&Value::List(vec![Value::String(
                "maintenance-v1".to_owned()
            )]))
        );
        assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
    }
}

/// Catches: an answer with a `maintain` order that a daemon of the release before refuses whole,
/// or reads as another verb, or whose order id it loses: it reads the order with no verb it
/// knows (so it runs nothing), keeps the case as a field it does not know, and writes it back
/// byte for byte. Proves the tag on the wire: field 11 of `Order` (`0x5a`).
#[test]
fn an_older_daemon_reads_a_maintain_order_as_no_verb_and_keeps_it() {
    let older = baseline_pool();
    let order_d = message(&older, "hostkeeper.agent.v1.Order");
    assert!(order_d.get_field_by_name("maintain").is_none());
    let order = agent::Order {
        order_id: "o-9".to_owned(),
        r#try: 2,
        verb: Some(agent::order::Verb::Maintain(maintain())),
    };
    let bytes = order.encode_to_vec();
    assert_eq!(
        agent::Order::decode(bytes.as_slice()).expect("decodes"),
        order
    );
    assert!(bytes.contains(&0x5a));
    let read = DynamicMessage::decode(order_d.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [11]);
    let verb = order_d
        .oneofs()
        .find(|o| o.name() == "verb")
        .expect("the verb");
    assert!(
        verb.fields().all(|f| !read.has_field(&f)),
        "no verb of the release before"
    );
    assert_eq!(
        read.get_field_by_name("order_id").as_deref(),
        Some(&Value::String("o-9".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("try").as_deref(),
        Some(&Value::U64(2))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");

    let answer = agent::HeartbeatResponse {
        order: Some(order),
        features: vec!["maintenance-v1".to_owned()],
        ..agent::HeartbeatResponse::default()
    };
    let bytes = answer.encode_to_vec();
    let answer_d = message(&older, "hostkeeper.agent.v1.HeartbeatResponse");
    let read = DynamicMessage::decode(answer_d, bytes.as_slice()).expect("decodes");
    assert_eq!(
        kept(&read),
        Vec::<u32>::new(),
        "the answer's own fields all known"
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a host of the operator API with the report that a client of the release before
/// refuses, or writes back changed. Proves the tag on the wire: field 26 of `Host` (`0xd2 0x01`).
#[test]
fn an_older_operator_client_keeps_a_hosts_maintenance_report() {
    let older = baseline_pool();
    let host_d = message(&older, "hostkeeper.operator.v1.Host");
    assert!(host_d.get_field_by_name("maintenance").is_none());
    let host = op::Host {
        node_id: "mac-3".to_owned(),
        live: true,
        maintenance: Some(at_every_bound()),
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host);
    assert!(bytes.windows(2).any(|w| w == [0xd2, 0x01]));
    let read = DynamicMessage::decode(host_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [26]);
    assert_eq!(
        read.get_field_by_name("node_id").as_deref(),
        Some(&Value::String("mac-3".to_owned()))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a heartbeat, an order or a host of the release before read with a report or a
/// `maintain` order, so that a coordinator shows a report no daemon sent, or a daemon takes a
/// verb nobody ordered: each reads with none, and the order's verb as it was written.
#[test]
fn the_release_befores_messages_read_without_maintenance() {
    let older = baseline_pool();
    let mut beat = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    beat.set_field_by_name(
        "features",
        Value::List(vec![Value::String("prerequisites-v1".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.maintenance, None, "absent: no report");
    assert_eq!(read.features, ["prerequisites-v1"]);

    let mut order = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.Order"));
    order.set_field_by_name("order_id", Value::String("o-1".to_owned()));
    let mut clear = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.ClearHalt"));
    clear.set_field_by_name("service", Value::String("web".to_owned()));
    clear.set_field_by_name("halt_generation", Value::U64(3));
    order.set_field_by_name("clear_halt", Value::Message(clear));
    let read = agent::Order::decode(order.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read.verb,
        Some(agent::order::Verb::ClearHalt(agent::ClearHalt {
            service: "web".to_owned(),
            halt_generation: 3,
        }))
    );

    let mut host = DynamicMessage::new(message(&older, "hostkeeper.operator.v1.Host"));
    host.set_field_by_name("node_id", Value::String("db-1".to_owned()));
    let read = op::Host::decode(host.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.maintenance, None, "absent: the coordinator keeps none");
    assert_eq!(read.node_id, "db-1");
}

/// Catches: an empty report read as no report, or no report as an empty one: a coordinator would
/// show a host that sent nothing as one with nothing due.
#[test]
fn the_empty_report_is_distinct_from_absent() {
    let reported = agent::HeartbeatRequest {
        maintenance: Some(agent::MaintenanceReport::default()),
        ..agent::HeartbeatRequest::default()
    };
    assert_eq!(reported.encode_to_vec(), [0x92, 0x01, 0]);
    assert_eq!(
        agent::HeartbeatRequest::decode([0x92, 0x01, 0].as_slice()).expect("decodes"),
        reported
    );
    assert_eq!(
        agent::HeartbeatRequest::decode(&[][..])
            .expect("decodes")
            .maintenance,
        None
    );
    let empty = agent::MaintenanceReport::decode(&[][..]).expect("decodes");
    assert_eq!((empty.host, empty.mac, empty.run), (None, None, None));
    assert!(empty.xcodes.is_empty() && empty.due.is_empty());
}

/// Catches: a number this build does not know (a newer daemon's step or kind) read as a known
/// one, or dropped: it reads as the number it is, which no value of this build has, and is
/// written back, so a newer daemon's report reaches the coordinator's host view as it came.
#[test]
fn an_enum_number_this_build_does_not_know_is_kept() {
    let run = agent::MaintenanceRunState::decode([0x18, 0x63, 0x22, 0x01, 0x63].as_slice())
        .expect("decodes");
    assert_eq!(run.step, 99);
    assert_eq!(agent::MaintenanceStep::try_from(run.step).ok(), None);
    assert_eq!(run.applied, [99]);
    assert_eq!(agent::MaintenanceKind::try_from(99).ok(), None);
    assert_eq!(run.encode_to_vec(), [0x18, 0x63, 0x22, 0x01, 0x63]);
}

/// Catches: a new field or message given another number, label or type than the schema says,
/// which a peer built apart would read as something else.
#[test]
fn the_maintenance_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let kind = |name: &str| Kind::Message(message(&now, &format!("hostkeeper.agent.v1.{name}")));
    let enum_kind = |name: &str| {
        Kind::Enum(
            now.get_enum_by_name(&format!("hostkeeper.agent.v1.{name}"))
                .unwrap_or_else(|| panic!("{name}")),
        )
    };
    let fields = [
        (
            "hostkeeper.agent.v1.HeartbeatRequest",
            "maintenance",
            18,
            kind("MaintenanceReport"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.Order",
            "maintain",
            11,
            kind("Maintain"),
            false,
            true,
        ),
        (
            "hostkeeper.operator.v1.Host",
            "maintenance",
            26,
            kind("MaintenanceReport"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "host",
            1,
            kind("MaintenanceHost"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "xcodes",
            2,
            kind("InstalledXcode"),
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "mac",
            3,
            kind("MacUpdateFacts"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "due",
            4,
            kind("DueItem"),
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "due_left_out",
            5,
            Kind::Uint32,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "looked_unix",
            6,
            Kind::Uint64,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "unparsed_lines",
            7,
            Kind::Uint32,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "run",
            8,
            kind("MaintenanceRunState"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceReport",
            "look_failed",
            9,
            Kind::String,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceHost",
            "boot_unix",
            8,
            Kind::Uint64,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.InstalledXcode",
            "selected",
            4,
            Kind::Bool,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "filevault",
            1,
            enum_kind("FileVaultState"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "authrestart_supported",
            2,
            Kind::Bool,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "data_free_bytes",
            3,
            Kind::Uint64,
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "update_account",
            4,
            kind("UpdateAccountState"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "command_line_tools",
            5,
            kind("CommandLineToolsState"),
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.MacUpdateFacts",
            "mdm_conflicts",
            6,
            Kind::String,
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.DueItem",
            "kind",
            1,
            enum_kind("MaintenanceKind"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.DueItem",
            "needs_reboot",
            4,
            Kind::Bool,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.DueItem",
            "security",
            6,
            Kind::Bool,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceRunState",
            "step",
            3,
            enum_kind("MaintenanceStep"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.MaintenanceRunState",
            "applied",
            4,
            enum_kind("MaintenanceKind"),
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "action",
            2,
            enum_kind("MaintainAction"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "kinds",
            3,
            enum_kind("MaintenanceKind"),
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "linux_scope",
            4,
            enum_kind("LinuxScope"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "drain_deadline_secs",
            5,
            Kind::Uint64,
            false,
            true,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "macos_path",
            6,
            enum_kind("MacosPath"),
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "xcodes",
            7,
            kind("XcodeEntry"),
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "xcode_remove",
            8,
            Kind::String,
            true,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "return_secs",
            10,
            Kind::Uint64,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.Maintain",
            "macos_major",
            11,
            Kind::Bool,
            false,
            false,
        ),
        (
            "hostkeeper.agent.v1.XcodeEntry",
            "size",
            3,
            Kind::Uint64,
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
    // Every other field of the new messages is a singular text, at its number.
    let texts: [(&str, &[(&str, u32)]); 7] = [
        (
            "MaintenanceHost",
            &[
                ("os_name", 1),
                ("os_version", 2),
                ("os_build", 3),
                ("hardware_serial", 4),
                ("running_kernel", 5),
                ("newest_kernel", 6),
                ("boot_id", 7),
            ],
        ),
        (
            "InstalledXcode",
            &[("version", 1), ("build", 2), ("path", 3)],
        ),
        ("CommandLineToolsState", &[("version", 3)]),
        ("DueItem", &[("from", 2), ("to", 3), ("label", 5)]),
        (
            "MaintenanceRunState",
            &[("run_id", 1), ("order_id", 2), ("halted_reason", 5)],
        ),
        ("Maintain", &[("run_id", 1), ("xcode_default", 9)]),
        ("XcodeEntry", &[("version", 1), ("build", 2), ("sha256", 4)]),
    ];
    for (name, fields) in texts {
        let m = message(&now, &format!("hostkeeper.agent.v1.{name}"));
        for (field, number) in fields.iter() {
            let found = m
                .get_field_by_name(field)
                .unwrap_or_else(|| panic!("{name}.{field}"));
            assert_eq!(
                (found.number(), found.kind(), found.is_list()),
                (*number, Kind::String, false),
                "{name}.{field}"
            );
        }
    }
    let account = message(&now, "hostkeeper.agent.v1.UpdateAccountState");
    for (field, number) in [("present", 1), ("admin", 2), ("secure_token", 3)] {
        let found = account.get_field_by_name(field).expect(field);
        assert_eq!((found.number(), found.kind()), (number, Kind::Bool));
    }
    let tools = message(&now, "hostkeeper.agent.v1.CommandLineToolsState");
    for (field, number) in [("receipt", 1), ("files", 2)] {
        let found = tools.get_field_by_name(field).expect(field);
        assert_eq!((found.number(), found.kind()), (number, Kind::Bool));
    }
    let counts: Vec<usize> = NEW_MESSAGES
        .iter()
        .map(|name| message(&now, name).fields().count())
        .collect();
    assert_eq!(counts, [9, 8, 4, 6, 3, 3, 6, 5, 11, 4]);
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
/// the wire in every report and order, and the names are what the operator API's JSON shows.
#[test]
fn the_enum_values_are_fixed() {
    let now = current_pool();
    let want: [(&str, &[&str]); 6] = [
        (
            "FileVaultState",
            &[
                "FILE_VAULT_STATE_UNSPECIFIED",
                "FILE_VAULT_STATE_OFF",
                "FILE_VAULT_STATE_ON",
                "FILE_VAULT_STATE_ENCRYPTING",
                "FILE_VAULT_STATE_DECRYPTING",
            ],
        ),
        (
            "MaintenanceKind",
            &[
                "MAINTENANCE_KIND_UNSPECIFIED",
                "MAINTENANCE_KIND_MACOS",
                "MAINTENANCE_KIND_XCODE",
                "MAINTENANCE_KIND_LINUX",
                "MAINTENANCE_KIND_REBOOT",
            ],
        ),
        (
            "MaintenanceStep",
            &[
                "MAINTENANCE_STEP_UNSPECIFIED",
                "MAINTENANCE_STEP_WAITING",
                "MAINTENANCE_STEP_STAGING",
                "MAINTENANCE_STEP_DRAINING",
                "MAINTENANCE_STEP_APPLYING",
                "MAINTENANCE_STEP_REBOOTING",
                "MAINTENANCE_STEP_CHECKING",
                "MAINTENANCE_STEP_PUTTING_BACK",
                "MAINTENANCE_STEP_DONE",
                "MAINTENANCE_STEP_DEFERRED",
                "MAINTENANCE_STEP_HALTED",
            ],
        ),
        (
            "MaintainAction",
            &[
                "MAINTAIN_ACTION_UNSPECIFIED",
                "MAINTAIN_ACTION_RUN",
                "MAINTAIN_ACTION_RETRY",
                "MAINTAIN_ACTION_PUT_BACK",
            ],
        ),
        (
            "LinuxScope",
            &[
                "LINUX_SCOPE_UNSPECIFIED",
                "LINUX_SCOPE_SECURITY",
                "LINUX_SCOPE_ALL",
            ],
        ),
        (
            "MacosPath",
            &[
                "MACOS_PATH_UNSPECIFIED",
                "MACOS_PATH_INSTALL",
                "MACOS_PATH_WAIT",
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
    assert_eq!(agent::MaintenanceStep::Halted as i32, 10);
    assert_eq!(agent::MaintenanceKind::Reboot as i32, 4);
}
