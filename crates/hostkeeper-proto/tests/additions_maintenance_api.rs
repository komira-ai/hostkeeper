//! What the maintenance API adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Maintenance` service of the operator
//! API with its messages, `MaintenanceRefusal`, `MaintenanceState`, `MaintenanceHostStep` and
//! `MaintenanceSkip` (`proto/hostkeeper/operator/v1/maintenance.proto`,
//! docs/design/maintenance-updates.md).
//!
//! Nothing existing changed: the release before knows none of the new names, a reader of it that
//! meets one of the new messages keeps every byte, and this build reads what that release wrote
//! (a message of no field) as the default of each. Those are the tests of rule B8 in both
//! directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind};

use hostkeeper_proto::ctl::{COMMANDS, Input};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("this build's set decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// A run with every field set.
fn run() -> op::MaintenanceRun {
    op::MaintenanceRun {
        id: "run-1".to_owned(),
        state: op::MaintenanceState::Halted.into(),
        kinds: vec![
            agent::MaintenanceKind::Macos.into(),
            agent::MaintenanceKind::Reboot.into(),
        ],
        host_ids: vec!["node-01".to_owned(), "node-02".to_owned()],
        host_group: "macs".to_owned(),
        window_start_unix: 1_800_000_000,
        window_end_unix: 1_800_003_600,
        batch: Some(2),
        drain_deadline_secs: Some(600),
        take_last_host: true,
        reason: "monthly update".to_owned(),
        by_name: "alice".to_owned(),
        scheduled_unix: 1_799_999_000,
        ended_unix: 1_800_004_000,
        halted_groups: vec!["macs".to_owned()],
        host_count: 2,
        xcode_sha256: "a".repeat(64),
    }
}

/// A host's turn with every field set.
fn host() -> op::MaintenanceHost {
    op::MaintenanceHost {
        host_id: "node-01".to_owned(),
        host_group: "macs".to_owned(),
        step: op::MaintenanceHostStep::Halted.into(),
        since_unix: 1_800_000_100,
        kinds: vec![
            agent::MaintenanceKind::Macos.into(),
            agent::MaintenanceKind::Xcode.into(),
        ],
        applied: vec![agent::MaintenanceKind::Macos.into()],
        skipped_because: op::MaintenanceSkip::FilevaultNotReady.into(),
        reason: "apply: exit 1".to_owned(),
    }
}

/// An audit entry with a few fields set.
fn entry() -> op::AuditEntry {
    op::AuditEntry {
        seq: 7,
        action: "maintenance_schedule".to_owned(),
        ..op::AuditEntry::default()
    }
}

/// One message of each type the maintenance API added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.operator.v1.ScheduleMaintenanceRequest",
            round_trip(&op::ScheduleMaintenanceRequest {
                kinds: vec![
                    agent::MaintenanceKind::Linux.into(),
                    agent::MaintenanceKind::Reboot.into(),
                ],
                host_ids: vec!["node-01".to_owned()],
                host_group: "macs".to_owned(),
                window_start_unix: 1_800_000_000,
                window_end_unix: 1_800_003_600,
                batch: Some(3),
                drain_deadline_secs: Some(0),
                reason: "monthly update".to_owned(),
                take_last_host: true,
                confirm: true,
                xcode_sha256: "a".repeat(64),
            }),
        ),
        (
            "hostkeeper.operator.v1.ScheduleMaintenanceResponse",
            round_trip(&op::ScheduleMaintenanceResponse {
                run: Some(run()),
                hosts: vec![host()],
                entry: Some(entry()),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListMaintenanceRunsRequest",
            round_trip(&op::ListMaintenanceRunsRequest {
                states: vec![
                    op::MaintenanceState::Running.into(),
                    op::MaintenanceState::Halted.into(),
                ],
                limit: 50,
            }),
        ),
        (
            "hostkeeper.operator.v1.ListMaintenanceRunsResponse",
            round_trip(&op::ListMaintenanceRunsResponse { runs: vec![run()] }),
        ),
        (
            "hostkeeper.operator.v1.GetMaintenanceRunRequest",
            round_trip(&op::GetMaintenanceRunRequest {
                run_id: "run-1".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.GetMaintenanceRunResponse",
            round_trip(&op::GetMaintenanceRunResponse {
                run: Some(run()),
                hosts: vec![host()],
            }),
        ),
        (
            "hostkeeper.operator.v1.CancelMaintenanceRunRequest",
            round_trip(&op::CancelMaintenanceRunRequest {
                run_id: "run-1".to_owned(),
                reason: "freeze".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.CancelMaintenanceRunResponse",
            round_trip(&op::CancelMaintenanceRunResponse {
                run: Some(run()),
                entry: Some(entry()),
            }),
        ),
        (
            "hostkeeper.operator.v1.RetryMaintenanceHostRequest",
            round_trip(&op::RetryMaintenanceHostRequest {
                run_id: "run-1".to_owned(),
                host_id: "node-01".to_owned(),
                reason: "disk freed".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RetryMaintenanceHostResponse",
            round_trip(&op::RetryMaintenanceHostResponse {
                host: Some(host()),
                entry: Some(entry()),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutBackMaintenanceHostRequest",
            round_trip(&op::PutBackMaintenanceHostRequest {
                run_id: "run-1".to_owned(),
                host_id: "node-01".to_owned(),
                reason: "give up".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutBackMaintenanceHostResponse",
            round_trip(&op::PutBackMaintenanceHostResponse {
                host: Some(host()),
                entry: Some(entry()),
            }),
        ),
        ("hostkeeper.operator.v1.MaintenanceRun", round_trip(&run())),
        (
            "hostkeeper.operator.v1.MaintenanceHost",
            round_trip(&host()),
        ),
    ]
}

/// Proves: every message, the four enums and the service of the maintenance API are new since the
/// release before, read back as written with every field set, and kept byte for byte by a reader
/// of that release.
/// Catches: a name the release before already knows (so not an addition), a message that does
/// not read back as written, and a reader of the release before that drops a byte of one.
#[test]
fn the_maintenance_api_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 14);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    for name in [
        "MaintenanceRefusal",
        "MaintenanceState",
        "MaintenanceHostStep",
        "MaintenanceSkip",
    ] {
        assert!(
            older
                .get_enum_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name} is new"
        );
    }
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Maintenance")
            .is_none()
    );
}

/// Proves: an empty message reads as the default of each maintenance message: a request naming no
/// kind, host or group, with no window, no batch and no drain deadline of its own (absent, so the
/// group's own apply) and no confirmation; a run in no state; a host in no step, skipped for no
/// reason.
/// Catches: a field made required, so that the empty request of a caller that knows none of the
/// new fields no longer reads as "everything due, now, as the group says, not stored"; and a
/// default that reads as a confirmed run, a batch of 0 or a deadline of 0 (no deadline).
#[test]
fn an_empty_message_reads_as_the_defaults_of_each_maintenance_message() {
    let schedule = op::ScheduleMaintenanceRequest::decode(&[][..]).expect("decodes");
    assert_eq!(schedule, op::ScheduleMaintenanceRequest::default());
    assert_eq!(schedule.kinds().count(), 0);
    assert!(schedule.host_ids.is_empty());
    assert_eq!(schedule.host_group, "");
    assert_eq!(
        (schedule.window_start_unix, schedule.window_end_unix),
        (0, 0)
    );
    assert_eq!(schedule.batch, None);
    assert_eq!(schedule.drain_deadline_secs, None);
    assert_eq!(schedule.reason, "");
    assert!(!schedule.take_last_host);
    assert!(!schedule.confirm);

    let answer = op::ScheduleMaintenanceResponse::decode(&[][..]).expect("decodes");
    assert!(answer.run.is_none() && answer.entry.is_none() && answer.hosts.is_empty());

    let list = op::ListMaintenanceRunsRequest::decode(&[][..]).expect("decodes");
    assert_eq!((list.states().count(), list.limit), (0, 0));
    assert!(
        op::ListMaintenanceRunsResponse::decode(&[][..])
            .expect("decodes")
            .runs
            .is_empty()
    );

    let get = op::GetMaintenanceRunRequest::decode(&[][..]).expect("decodes");
    assert_eq!(get.run_id, "");
    let got = op::GetMaintenanceRunResponse::decode(&[][..]).expect("decodes");
    assert!(got.run.is_none() && got.hosts.is_empty());

    let cancel = op::CancelMaintenanceRunRequest::decode(&[][..]).expect("decodes");
    assert_eq!((cancel.run_id.as_str(), cancel.reason.as_str()), ("", ""));
    let cancelled = op::CancelMaintenanceRunResponse::decode(&[][..]).expect("decodes");
    assert!(cancelled.run.is_none() && cancelled.entry.is_none());

    let retry = op::RetryMaintenanceHostRequest::decode(&[][..]).expect("decodes");
    assert_eq!(retry, op::RetryMaintenanceHostRequest::default());
    let retried = op::RetryMaintenanceHostResponse::decode(&[][..]).expect("decodes");
    assert!(retried.host.is_none() && retried.entry.is_none());

    let put_back = op::PutBackMaintenanceHostRequest::decode(&[][..]).expect("decodes");
    assert_eq!(put_back, op::PutBackMaintenanceHostRequest::default());
    let put = op::PutBackMaintenanceHostResponse::decode(&[][..]).expect("decodes");
    assert!(put.host.is_none() && put.entry.is_none());

    let run = op::MaintenanceRun::decode(&[][..]).expect("decodes");
    assert_eq!(run, op::MaintenanceRun::default());
    assert_eq!(run.state(), op::MaintenanceState::Unspecified);
    assert_eq!(run.kinds().count(), 0);
    assert_eq!((run.batch, run.drain_deadline_secs), (None, None));
    assert!(!run.take_last_host);
    assert_eq!((run.ended_unix, run.host_count), (0, 0));
    assert!(run.halted_groups.is_empty());

    let host = op::MaintenanceHost::decode(&[][..]).expect("decodes");
    assert_eq!(host, op::MaintenanceHost::default());
    assert_eq!(host.step(), op::MaintenanceHostStep::Unspecified);
    assert_eq!(host.skipped_because(), op::MaintenanceSkip::Unspecified);
    assert_eq!((host.kinds().count(), host.applied().count()), (0, 0));
}

/// Proves: the numbers of the refusal values and their tokens (the value's name without its
/// prefix, in lower case), and the numbers of the state, host step and skip values, are the ones
/// the proto fixes; the host step values 1 to 10 are the host's own `MaintenanceStep`, by the same
/// number and name; an unknown number is read back as the number and judged unspecified.
/// Catches: a value renumbered, renamed or dropped, which would turn one refusal, state or step
/// into another for an older client, and a host step that drifts from the daemon's report.
#[test]
fn the_maintenance_enums_have_fixed_numbers_and_tokens() {
    assert_eq!(
        [
            op::MaintenanceRefusal::Unspecified as i32,
            op::MaintenanceRefusal::Unauthorized as i32,
            op::MaintenanceRefusal::Forbidden as i32,
            op::MaintenanceRefusal::InvalidRequest as i32,
            op::MaintenanceRefusal::UnknownGroup as i32,
            op::MaintenanceRefusal::UnknownHost as i32,
            op::MaintenanceRefusal::UnknownRun as i32,
            op::MaintenanceRefusal::NothingDue as i32,
            op::MaintenanceRefusal::WindowEnded as i32,
            op::MaintenanceRefusal::LastHost as i32,
            op::MaintenanceRefusal::HostInMaintenance as i32,
            op::MaintenanceRefusal::RunEnded as i32,
            op::MaintenanceRefusal::NotInRun as i32,
            op::MaintenanceRefusal::NotHalted as i32,
            op::MaintenanceRefusal::StoreFailed as i32,
            op::MaintenanceRefusal::AuditUnavailable as i32,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    let current = current_pool();
    let refusal = current
        .get_enum_by_name("hostkeeper.operator.v1.MaintenanceRefusal")
        .expect("the enum");
    let tokens: Vec<String> = refusal
        .values()
        .skip(1)
        .map(|v| {
            v.name()
                .trim_start_matches("MAINTENANCE_REFUSAL_")
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "unauthorized",
            "forbidden",
            "invalid_request",
            "unknown_group",
            "unknown_host",
            "unknown_run",
            "nothing_due",
            "window_ended",
            "last_host",
            "host_in_maintenance",
            "run_ended",
            "not_in_run",
            "not_halted",
            "store_failed",
            "audit_unavailable",
        ]
    );

    assert_eq!(
        [
            op::MaintenanceState::Unspecified as i32,
            op::MaintenanceState::Scheduled as i32,
            op::MaintenanceState::Running as i32,
            op::MaintenanceState::Halted as i32,
            op::MaintenanceState::Done as i32,
            op::MaintenanceState::Cancelled as i32,
            op::MaintenanceState::WindowEnded as i32,
        ],
        [0, 1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        [
            op::MaintenanceHostStep::Unspecified as i32,
            op::MaintenanceHostStep::Waiting as i32,
            op::MaintenanceHostStep::Staging as i32,
            op::MaintenanceHostStep::Draining as i32,
            op::MaintenanceHostStep::Applying as i32,
            op::MaintenanceHostStep::Rebooting as i32,
            op::MaintenanceHostStep::Checking as i32,
            op::MaintenanceHostStep::PuttingBack as i32,
            op::MaintenanceHostStep::Done as i32,
            op::MaintenanceHostStep::Deferred as i32,
            op::MaintenanceHostStep::Halted as i32,
            op::MaintenanceHostStep::NotBack as i32,
            op::MaintenanceHostStep::NotReached as i32,
            op::MaintenanceHostStep::Skipped as i32,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]
    );
    assert_eq!(
        [
            op::MaintenanceSkip::Unspecified as i32,
            op::MaintenanceSkip::NoMaintenanceV1 as i32,
            op::MaintenanceSkip::Held as i32,
            op::MaintenanceSkip::Excluded as i32,
            op::MaintenanceSkip::MdmConflict as i32,
            op::MaintenanceSkip::NoUpdateAccount as i32,
            op::MaintenanceSkip::InPackageUpdate as i32,
            op::MaintenanceSkip::GroupChanged as i32,
            op::MaintenanceSkip::FilevaultNotReady as i32,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8]
    );

    let host_step = current
        .get_enum_by_name("hostkeeper.operator.v1.MaintenanceHostStep")
        .expect("the enum");
    let agent_step = current
        .get_enum_by_name("hostkeeper.agent.v1.MaintenanceStep")
        .expect("the enum");
    let pairs = |enum_: &prost_reflect::EnumDescriptor, prefix: &str| -> Vec<(i32, String)> {
        enum_
            .values()
            .filter(|v| (1..=10).contains(&v.number()))
            .map(|v| (v.number(), v.name().trim_start_matches(prefix).to_owned()))
            .collect()
    };
    let host_pairs = pairs(&host_step, "MAINTENANCE_HOST_STEP_");
    assert_eq!(host_pairs.len(), 10);
    assert_eq!(host_pairs, pairs(&agent_step, "MAINTENANCE_STEP_"));

    let future_host = op::MaintenanceHost {
        step: 99,
        ..op::MaintenanceHost::default()
    };
    let read = op::MaintenanceHost::decode(round_trip(&future_host).as_slice()).expect("decodes");
    assert_eq!(read.step, 99);
    assert_eq!(read.step(), op::MaintenanceHostStep::Unspecified);
    let future_run = op::MaintenanceRun {
        state: 99,
        ..op::MaintenanceRun::default()
    };
    let read = op::MaintenanceRun::decode(round_trip(&future_run).as_slice()).expect("decodes");
    assert_eq!(read.state, 99);
    assert_eq!(read.state(), op::MaintenanceState::Unspecified);
}

/// Proves: the service has exactly `Schedule`, `List`, `Get`, `Cancel`, `RetryHost` and `PutBack`,
/// each unary and carrying the messages the design names, and the command table (so `hk` and
/// the web app's client) lists them as `maintenance schedule`, `list`, `get`, `cancel`,
/// `retry-host` and `put-back`, at their paths.
/// Catches: a call missing, renamed or streaming, and one the command table does not list.
#[test]
fn the_maintenance_service_has_its_six_unary_calls_in_the_command_table() {
    let pool = current_pool();
    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Maintenance")
        .expect("this build serves Maintenance");
    let methods: Vec<(String, String, String, bool)> = service
        .methods()
        .map(|method| {
            (
                method.name().to_owned(),
                method.input().name().to_owned(),
                method.output().name().to_owned(),
                method.is_client_streaming() || method.is_server_streaming(),
            )
        })
        .collect();
    let expected: Vec<(String, String, String, bool)> = [
        (
            "Schedule",
            "ScheduleMaintenanceRequest",
            "ScheduleMaintenanceResponse",
        ),
        (
            "List",
            "ListMaintenanceRunsRequest",
            "ListMaintenanceRunsResponse",
        ),
        (
            "Get",
            "GetMaintenanceRunRequest",
            "GetMaintenanceRunResponse",
        ),
        (
            "Cancel",
            "CancelMaintenanceRunRequest",
            "CancelMaintenanceRunResponse",
        ),
        (
            "RetryHost",
            "RetryMaintenanceHostRequest",
            "RetryMaintenanceHostResponse",
        ),
        (
            "PutBack",
            "PutBackMaintenanceHostRequest",
            "PutBackMaintenanceHostResponse",
        ),
    ]
    .iter()
    .map(|(name, input, output)| {
        (
            (*name).to_owned(),
            (*input).to_owned(),
            (*output).to_owned(),
            false,
        )
    })
    .collect();
    assert_eq!(methods, expected);

    let commands: Vec<(&str, &str, &str)> = COMMANDS
        .iter()
        .filter(|command| command.service == "Maintenance")
        .map(|command| {
            (
                command.service_command,
                command.method_command,
                command.path,
            )
        })
        .collect();
    assert_eq!(
        commands,
        [
            (
                "maintenance",
                "schedule",
                "/hostkeeper.operator.v1.Maintenance/Schedule",
            ),
            (
                "maintenance",
                "list",
                "/hostkeeper.operator.v1.Maintenance/List",
            ),
            (
                "maintenance",
                "get",
                "/hostkeeper.operator.v1.Maintenance/Get",
            ),
            (
                "maintenance",
                "cancel",
                "/hostkeeper.operator.v1.Maintenance/Cancel",
            ),
            (
                "maintenance",
                "retry-host",
                "/hostkeeper.operator.v1.Maintenance/RetryHost",
            ),
            (
                "maintenance",
                "put-back",
                "/hostkeeper.operator.v1.Maintenance/PutBack",
            ),
        ]
    );
}

/// Proves: every request field of `Maintenance` is a plain flag, none is secret, each command
/// takes its flags in the proto's order, and the enum flags take exactly the values the design
/// names (the unspecified zero is no flag value): `--kinds` the five kinds, `--states` the six
/// states.
/// Catches: a request field that `hk` would not take as a flag, a reason marked secret, a flag
/// that moved, and an enum flag that offers `unspecified` or lacks a value.
#[test]
fn every_maintenance_request_field_is_a_flag_and_none_is_secret() {
    let maintenance: Vec<_> = COMMANDS
        .iter()
        .filter(|command| command.service == "Maintenance")
        .collect();
    let mut flags = Vec::new();
    for command in &maintenance {
        for field in command.fields {
            assert!(!field.secret, "{}.{}", command.method, field.name);
            assert_eq!(
                field.input,
                Input::Flag,
                "{}.{}",
                command.method,
                field.name
            );
            flags.push((command.method_command, field.flag));
        }
    }
    assert_eq!(
        flags,
        [
            ("schedule", "kinds"),
            ("schedule", "host-ids"),
            ("schedule", "host-group"),
            ("schedule", "window-start-unix"),
            ("schedule", "window-end-unix"),
            ("schedule", "batch"),
            ("schedule", "drain-deadline-secs"),
            ("schedule", "reason"),
            ("schedule", "take-last-host"),
            ("schedule", "confirm"),
            ("schedule", "xcode-sha256"),
            ("list", "states"),
            ("list", "limit"),
            ("get", "run-id"),
            ("cancel", "run-id"),
            ("cancel", "reason"),
            ("retry-host", "run-id"),
            ("retry-host", "host-id"),
            ("retry-host", "reason"),
            ("put-back", "run-id"),
            ("put-back", "host-id"),
            ("put-back", "reason"),
        ]
    );
    let values = |method: &str, flag: &str| -> Vec<&str> {
        maintenance
            .iter()
            .filter(|command| command.method_command == method)
            .flat_map(|command| command.fields)
            .filter(|field| field.flag == flag)
            .flat_map(|field| field.enum_values)
            .map(|value| value.flag)
            .collect()
    };
    assert_eq!(
        values("schedule", "kinds"),
        ["macos", "xcode", "linux", "reboot", "probe", "repair"]
    );
    assert_eq!(
        values("list", "states"),
        [
            "scheduled",
            "running",
            "halted",
            "done",
            "cancelled",
            "window_ended",
        ]
    );
}

/// Proves: the optional fields keep presence on the wire: a batch or a drain deadline of 0 is sent
/// and read back as `Some(0)`, which is not the absent field (the group's own value), in a
/// request and in a run.
/// Catches: a drain deadline of 0 (no deadline) that decays into the absent field and so into the
/// group's deadline, or an optional field turned into a plain one.
#[test]
fn an_optional_field_of_zero_differs_from_an_absent_one() {
    let absent = op::ScheduleMaintenanceRequest::default();
    assert!(round_trip(&absent).is_empty());
    let zero = op::ScheduleMaintenanceRequest {
        drain_deadline_secs: Some(0),
        ..op::ScheduleMaintenanceRequest::default()
    };
    // Field 7, varint, value 0.
    assert_eq!(round_trip(&zero), [0x38, 0]);
    let read = op::ScheduleMaintenanceRequest::decode(&[0x38, 0][..]).expect("decodes");
    assert_eq!(read.drain_deadline_secs, Some(0));
    assert_eq!(read.batch, None);
    let batch = op::ScheduleMaintenanceRequest {
        batch: Some(0),
        ..op::ScheduleMaintenanceRequest::default()
    };
    // Field 6, varint, value 0.
    assert_eq!(round_trip(&batch), [0x30, 0]);

    let run = op::MaintenanceRun {
        drain_deadline_secs: Some(0),
        ..op::MaintenanceRun::default()
    };
    // Field 9, varint, value 0.
    assert_eq!(round_trip(&run), [0x48, 0]);
    let read = op::MaintenanceRun::decode(&[0x48, 0][..]).expect("decodes");
    assert_eq!(read.drain_deadline_secs, Some(0));
    assert_eq!(read.batch, None);
    assert!(round_trip(&op::MaintenanceRun::default()).is_empty());
}

/// Proves: the kinds of a request, a run and a host, and the kinds a host applied, are the agent's
/// `hostkeeper.agent.v1.MaintenanceKind`, one enum on both sides of the wire.
/// Catches: a second enum of kinds in the operator package that would drift from the daemon's.
#[test]
fn the_kinds_of_the_maintenance_messages_are_the_agents_kind_enum() {
    let pool = current_pool();
    for (message, field) in [
        ("ScheduleMaintenanceRequest", "kinds"),
        ("MaintenanceRun", "kinds"),
        ("MaintenanceHost", "kinds"),
        ("MaintenanceHost", "applied"),
    ] {
        let descriptor = pool
            .get_message_by_name(&format!("hostkeeper.operator.v1.{message}"))
            .expect("the message");
        let field_descriptor = descriptor.get_field_by_name(field).expect("the field");
        assert!(field_descriptor.is_list(), "{message}.{field}");
        match field_descriptor.kind() {
            Kind::Enum(kind) => assert_eq!(
                kind.full_name(),
                "hostkeeper.agent.v1.MaintenanceKind",
                "{message}.{field}"
            ),
            other => panic!("{message}.{field} is {other:?}"),
        }
    }
}
