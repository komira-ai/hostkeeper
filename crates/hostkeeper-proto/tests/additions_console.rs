//! What the console adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Console` service of the operator API
//! with its messages, `ConsoleRefusal`, `ConsoleKind`, `PowerAction`, `PowerState`,
//! `BootDevice` and `ConsoleAddressSource` (`proto/hostkeeper/operator/v1/console.proto`,
//! docs/design/console-broker.md). The fields that name a BMC's address in the answers, added
//! after the console shipped, are tested against the release that shipped it in
//! `additions_bmc.rs`.
//!
//! Nothing existing changed: the release before knows none of them, a reader of it that meets one
//! of the new messages keeps every byte, and this build reads what that release wrote (a message of
//! no field) as the default of each. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::ctl::COMMANDS;
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

/// A session with every field set.
fn session() -> op::ConsoleSession {
    op::ConsoleSession {
        host_id: "node-02".to_owned(),
        kind: op::ConsoleKind::Serial.into(),
        opened_by: "alice".to_owned(),
        opened_unix: 1_000,
        attached: true,
        reason: "boot order".to_owned(),
    }
}

/// An address with every field set.
fn address() -> op::ConsoleAddress {
    op::ConsoleAddress {
        address: "192.0.2.12".to_owned(),
        source: op::ConsoleAddressSource::Heartbeat.into(),
        stale: true,
        reported_unix: 1_000,
    }
}

/// One message of each type the console added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.operator.v1.OpenConsoleRequest",
            round_trip(&op::OpenConsoleRequest {
                host_id: "node-02".to_owned(),
                reason: "boot order".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.OpenConsoleResponse",
            round_trip(&op::OpenConsoleResponse {
                kind: op::ConsoleKind::Screen.into(),
                session: "ab".repeat(32),
                expires_in_secs: 30,
                quiet_secs: 10,
                address: Some(address()),
            }),
        ),
        (
            "hostkeeper.operator.v1.ConsolePowerRequest",
            round_trip(&op::ConsolePowerRequest {
                host_id: "node-02".to_owned(),
                action: op::PowerAction::Cycle.into(),
                reason: "hung".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ConsolePowerResponse",
            round_trip(&op::ConsolePowerResponse {
                state: op::PowerState::Off.into(),
                sent: op::PowerAction::On.into(),
                address: Some(address()),
            }),
        ),
        (
            "hostkeeper.operator.v1.SetBootDeviceRequest",
            round_trip(&op::SetBootDeviceRequest {
                host_id: "node-02".to_owned(),
                device: op::BootDevice::Disk.into(),
                persistent: true,
                efi: true,
                reason: "boot order".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.SetBootDeviceResponse",
            round_trip(&op::SetBootDeviceResponse {
                device: op::BootDevice::Disk.into(),
                persistent: true,
                efi: true,
                address: Some(address()),
            }),
        ),
        (
            "hostkeeper.operator.v1.ConsoleAddress",
            round_trip(&address()),
        ),
        (
            "hostkeeper.operator.v1.ConsoleSession",
            round_trip(&session()),
        ),
        (
            "hostkeeper.operator.v1.ListConsoleSessionsRequest",
            round_trip(&op::ListConsoleSessionsRequest {}),
        ),
        (
            "hostkeeper.operator.v1.ListConsoleSessionsResponse",
            round_trip(&op::ListConsoleSessionsResponse {
                sessions: vec![session()],
            }),
        ),
        (
            "hostkeeper.operator.v1.CloseConsoleRequest",
            round_trip(&op::CloseConsoleRequest {
                host_id: "node-02".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.CloseConsoleResponse",
            round_trip(&op::CloseConsoleResponse {}),
        ),
    ]
}

/// Catches: a message, enum or service of the console that the release before already knows, or
/// that does not read back as written, or that a reader of that release drops: it knows none of
/// them, and a message it reads with no field keeps every byte of the new ones.
#[test]
fn the_console_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 12);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    for name in [
        "ConsoleRefusal",
        "ConsoleKind",
        "PowerAction",
        "PowerState",
        "BootDevice",
        "ConsoleAddressSource",
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
            .get_service_by_name("hostkeeper.operator.v1.Console")
            .is_none()
    );
}

/// Catches: a console request of this build that a caller of the release before could not have
/// sent: an empty message reads as the request with every field at its default, an unspecified
/// action, device and kind among them, which the coordinator refuses rather than guesses.
#[test]
fn an_empty_message_reads_as_the_defaults_of_each_console_message() {
    let open = op::OpenConsoleRequest::decode(&[][..]).expect("decodes");
    assert_eq!(open, op::OpenConsoleRequest::default());
    let power = op::ConsolePowerRequest::decode(&[][..]).expect("decodes");
    assert_eq!(power.action(), op::PowerAction::Unspecified);
    let boot = op::SetBootDeviceRequest::decode(&[][..]).expect("decodes");
    assert_eq!(
        (boot.device(), boot.persistent, boot.efi),
        (op::BootDevice::Unspecified, false, false)
    );
    let session = op::ConsoleSession::decode(&[][..]).expect("decodes");
    assert_eq!(session.kind(), op::ConsoleKind::Unspecified);
}

/// Catches: an enum value of the console renumbered, renamed or dropped, which would turn a
/// `cycle` of an older client into another command: every value has the number of the proto, the
/// zero is `UNSPECIFIED`, and an unknown number is read back as the number and judged
/// unspecified.
#[test]
fn the_console_enums_have_an_unspecified_zero_and_fixed_numbers() {
    let actions = [
        op::PowerAction::Unspecified,
        op::PowerAction::Status,
        op::PowerAction::On,
        op::PowerAction::Off,
        op::PowerAction::Soft,
        op::PowerAction::Cycle,
        op::PowerAction::Reset,
    ];
    let numbers: Vec<i32> = actions.iter().map(|action| *action as i32).collect();
    assert_eq!(numbers, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(
        [
            op::BootDevice::Unspecified as i32,
            op::BootDevice::Pxe as i32,
            op::BootDevice::Disk as i32,
            op::BootDevice::Bios as i32,
        ],
        [0, 1, 2, 3]
    );
    assert_eq!(
        [
            op::ConsoleKind::Unspecified as i32,
            op::ConsoleKind::Serial as i32,
            op::ConsoleKind::Screen as i32,
        ],
        [0, 1, 2]
    );
    assert_eq!(
        [
            op::PowerState::Unspecified as i32,
            op::PowerState::On as i32,
            op::PowerState::Off as i32,
        ],
        [0, 1, 2]
    );
    assert_eq!(
        [
            op::ConsoleRefusal::AuditUnavailable as i32,
            op::ConsoleRefusal::BmcFailed as i32,
            op::ConsoleRefusal::StoreFailed as i32,
        ],
        [19, 20, 21]
    );
    let future = op::ConsolePowerRequest {
        action: 9,
        ..op::ConsolePowerRequest::default()
    };
    let read = op::ConsolePowerRequest::decode(round_trip(&future).as_slice()).expect("decodes");
    assert_eq!(read.action, 9);
    assert_eq!(read.action(), op::PowerAction::Unspecified);
}

/// Catches: a `Console` call missing, renamed, streaming or carrying other messages than the
/// design names, and one that the command table (and so `hkctl` and the web app's client) does
/// not list: the service has exactly `Open`, `Power`, `SetBootDevice`, `List` and `Close`, each
/// unary, and the table names them `console open`, `console power`, `console set-boot-device`,
/// `console list` and `console close`.
#[test]
fn the_console_service_has_its_five_unary_calls_in_the_command_table() {
    let pool = current_pool();
    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Console")
        .expect("this build serves Console");
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
    let expected = [
        ("Open", "OpenConsoleRequest", "OpenConsoleResponse"),
        ("Power", "ConsolePowerRequest", "ConsolePowerResponse"),
        (
            "SetBootDevice",
            "SetBootDeviceRequest",
            "SetBootDeviceResponse",
        ),
        (
            "List",
            "ListConsoleSessionsRequest",
            "ListConsoleSessionsResponse",
        ),
        ("Close", "CloseConsoleRequest", "CloseConsoleResponse"),
    ];
    let expected: Vec<(String, String, String, bool)> = expected
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

    let commands: Vec<(&str, &str)> = COMMANDS
        .iter()
        .filter(|command| command.service == "Console")
        .map(|command| (command.service_command, command.method_command))
        .collect();
    assert_eq!(
        commands,
        [
            ("console", "open"),
            ("console", "power"),
            ("console", "set-boot-device"),
            ("console", "list"),
            ("console", "close"),
        ]
    );
}

/// Catches: a reason, a host id or an enum that `hkctl` would not take as a flag, or a field of
/// the console marked secret: every request field of `Console` is a plain flag, and the action
/// and device flags take exactly the values the design names (the unspecified zero is no flag
/// value).
#[test]
fn every_console_request_field_is_a_flag_and_none_is_secret() {
    let console: Vec<_> = COMMANDS
        .iter()
        .filter(|command| command.service == "Console")
        .collect();
    let mut flags = Vec::new();
    for command in &console {
        for field in command.fields {
            assert!(!field.secret, "{}.{}", command.method, field.name);
            assert_eq!(
                field.input,
                hostkeeper_proto::ctl::Input::Flag,
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
            ("open", "host-id"),
            ("open", "reason"),
            ("power", "host-id"),
            ("power", "action"),
            ("power", "reason"),
            ("set-boot-device", "host-id"),
            ("set-boot-device", "device"),
            ("set-boot-device", "persistent"),
            ("set-boot-device", "efi"),
            ("set-boot-device", "reason"),
            ("close", "host-id"),
        ]
    );
    let values = |method: &str, flag: &str| -> Vec<&str> {
        console
            .iter()
            .filter(|command| command.method_command == method)
            .flat_map(|command| command.fields)
            .filter(|field| field.flag == flag)
            .flat_map(|field| field.enum_values)
            .map(|value| value.flag)
            .collect()
    };
    assert_eq!(
        values("power", "action"),
        ["status", "on", "off", "soft", "cycle", "reset"]
    );
    assert_eq!(values("set-boot-device", "device"), ["pxe", "disk", "bios"]);
}
