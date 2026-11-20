//! What a BMC's discovered address adds to the wire:
//!
//! - `HeartbeatRequest.bmc` (17), a `BmcReport` of the BMC's address and MAC that a Linux daemon
//!   reads from its BMC in-band, new since the committed baseline (`proto/compat/baseline.binpb`,
//!   the release before): a reader of that release keeps the field as one it does not know and
//!   writes it back byte for byte; this build reads what that release wrote with no report.
//! - `address` on `OpenConsoleResponse` (5), `ConsolePowerResponse` (3) and
//!   `SetBootDeviceResponse` (4), a `ConsoleAddress` naming where the coordinator found the
//!   device's address. The console shipped after the baseline, so the release before is written
//!   here as the messages it shipped (every field but `address`): a client of it reads this
//!   build's answers with every field it knows as written, and this build reads that release's
//!   answers with no address.
//!
//! Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

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

fn report() -> agent::BmcReport {
    agent::BmcReport {
        ip: "192.0.2.12".to_owned(),
        mac: "0c:c4:7a:00:00:01".to_owned(),
    }
}

/// Catches: a heartbeat with a BMC that the release before refuses, reads with other values in
/// the fields it knows, or writes back without the report (a coordinator of that release that
/// relays it would lose it): the release before has no field 17 and no `BmcReport`, keeps 17 as
/// an unknown field, and writes the message back byte for byte.
#[test]
fn an_older_reader_keeps_a_heartbeats_bmc() {
    let older = baseline_pool();
    let beat = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat.get_field_by_name("bmc").is_none());
    assert!(beat.get_field(17).is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.BmcReport")
            .is_none()
    );

    let new = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["heartbeat-v2".to_owned()],
        bmc: Some(report()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = new.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        new
    );
    assert!(
        bytes.windows(2).any(|tag| tag == [0x8a, 0x01]),
        "field 17 is on the wire"
    );
    let read = DynamicMessage::decode(beat, bytes.as_slice()).expect("the release before reads it");
    let unknown: Vec<u32> = read.unknown_fields().map(|field| field.number()).collect();
    assert_eq!(unknown, [17]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::LinuxX8664 as i32))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a heartbeat of the release before read with a made-up report, or with the fields it
/// wrote changed: it reads with no BMC.
#[test]
fn a_heartbeat_of_the_release_before_reads_without_a_bmc() {
    let older = baseline_pool();
    let mut written = DynamicMessage::new(message(&older, "hostkeeper.agent.v1.HeartbeatRequest"));
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxArm64 as i32),
    );
    written.set_field_by_name(
        "features",
        Value::List(vec![Value::String("heartbeat-v2".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(written.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(read.bmc, None);
    assert_eq!(read.platform(), agent::Platform::LinuxArm64);
    assert_eq!(read.features, ["heartbeat-v2"]);
}

/// `ConsolePowerResponse` as the release that shipped the console wrote and read it.
#[derive(Clone, PartialEq, prost::Message)]
struct PowerResponseBefore {
    #[prost(enumeration = "op::PowerState", tag = "1")]
    state: i32,
    #[prost(enumeration = "op::PowerAction", tag = "2")]
    sent: i32,
}

/// `OpenConsoleResponse` as the release that shipped the console wrote and read it.
#[derive(Clone, PartialEq, prost::Message)]
struct OpenResponseBefore {
    #[prost(enumeration = "op::ConsoleKind", tag = "1")]
    kind: i32,
    #[prost(string, tag = "2")]
    session: String,
    #[prost(uint64, tag = "3")]
    expires_in_secs: u64,
    #[prost(uint64, tag = "4")]
    quiet_secs: u64,
}

/// `SetBootDeviceResponse` as the release that shipped the console wrote and read it.
#[derive(Clone, PartialEq, prost::Message)]
struct BootResponseBefore {
    #[prost(enumeration = "op::BootDevice", tag = "1")]
    device: i32,
    #[prost(bool, tag = "2")]
    persistent: bool,
    #[prost(bool, tag = "3")]
    efi: bool,
}

fn address() -> op::ConsoleAddress {
    op::ConsoleAddress {
        address: "192.0.2.12".to_owned(),
        source: op::ConsoleAddressSource::Heartbeat.into(),
        stale: true,
        reported_unix: 1_800_000_000,
    }
}

/// Catches: an answer with an address that a client of the release that shipped the console
/// (`hk`, the web app) refuses or reads with other values, and an answer of that release read
/// by this build with a made-up address: each direction reads every field the writer and the
/// reader share as written, and the address is absent where it was not written.
#[test]
fn the_console_answers_with_an_address_and_the_release_before_read_each_other() {
    let power = op::ConsolePowerResponse {
        state: op::PowerState::Off.into(),
        sent: op::PowerAction::On.into(),
        address: Some(address()),
    };
    let before = PowerResponseBefore::decode(power.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        before,
        PowerResponseBefore {
            state: op::PowerState::Off.into(),
            sent: op::PowerAction::On.into(),
        }
    );
    let read = op::ConsolePowerResponse::decode(before.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        read,
        op::ConsolePowerResponse {
            address: None,
            ..power
        }
    );

    let open = op::OpenConsoleResponse {
        kind: op::ConsoleKind::Serial.into(),
        session: "ab".repeat(32),
        expires_in_secs: 30,
        quiet_secs: 10,
        address: Some(address()),
    };
    let before = OpenResponseBefore::decode(open.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        (
            before.kind,
            before.session.as_str(),
            before.expires_in_secs,
            before.quiet_secs
        ),
        (
            op::ConsoleKind::Serial as i32,
            "ab".repeat(32).as_str(),
            30,
            10
        )
    );
    let read = op::OpenConsoleResponse::decode(before.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        read,
        op::OpenConsoleResponse {
            address: None,
            ..open
        }
    );

    let boot = op::SetBootDeviceResponse {
        device: op::BootDevice::Pxe.into(),
        persistent: true,
        efi: true,
        address: Some(address()),
    };
    let before = BootResponseBefore::decode(boot.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        (before.device, before.persistent, before.efi),
        (op::BootDevice::Pxe as i32, true, true)
    );
    let read = op::SetBootDeviceResponse::decode(before.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(
        read,
        op::SetBootDeviceResponse {
            address: None,
            ..boot
        }
    );
}

/// Catches: a new field renumbered, retyped or made required, a source's number moved (a client
/// would name the wrong source), and an empty address read as absent.
#[test]
fn the_new_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let fields = [
        ("hostkeeper.agent.v1.HeartbeatRequest", "bmc", 17, true),
        ("hostkeeper.agent.v1.BmcReport", "ip", 1, false),
        ("hostkeeper.agent.v1.BmcReport", "mac", 2, false),
        (
            "hostkeeper.operator.v1.OpenConsoleResponse",
            "address",
            5,
            true,
        ),
        (
            "hostkeeper.operator.v1.ConsolePowerResponse",
            "address",
            3,
            true,
        ),
        (
            "hostkeeper.operator.v1.SetBootDeviceResponse",
            "address",
            4,
            true,
        ),
        ("hostkeeper.operator.v1.ConsoleAddress", "address", 1, false),
        ("hostkeeper.operator.v1.ConsoleAddress", "source", 2, false),
        ("hostkeeper.operator.v1.ConsoleAddress", "stale", 3, false),
        (
            "hostkeeper.operator.v1.ConsoleAddress",
            "reported_unix",
            4,
            false,
        ),
    ];
    for (name, field, number, presence) in fields {
        let found = message(&now, name)
            .get_field_by_name(field)
            .unwrap_or_else(|| panic!("{name}.{field}"));
        assert_eq!(
            (found.number(), found.supports_presence(), found.is_list()),
            (number, presence, false),
            "{name}.{field}"
        );
    }
    assert!(matches!(
        message(&now, "hostkeeper.agent.v1.HeartbeatRequest")
            .get_field_by_name("bmc")
            .map(|field| field.kind()),
        Some(Kind::Message(_))
    ));
    assert_eq!(
        message(&now, "hostkeeper.agent.v1.BmcReport")
            .fields()
            .count(),
        2
    );
    assert_eq!(
        message(&now, "hostkeeper.operator.v1.ConsoleAddress")
            .fields()
            .count(),
        4
    );
    assert_eq!(
        [
            op::ConsoleAddressSource::Unspecified as i32,
            op::ConsoleAddressSource::TargetsFile as i32,
            op::ConsoleAddressSource::Heartbeat as i32,
            op::ConsoleAddressSource::NeighborTable as i32,
        ],
        [0, 1, 2, 3]
    );
    let empty = agent::HeartbeatRequest {
        bmc: Some(agent::BmcReport::default()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = empty.encode_to_vec();
    assert_eq!(bytes, [0x8a, 0x01, 0]);
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice())
            .expect("decodes")
            .bmc,
        Some(agent::BmcReport::default())
    );
}
