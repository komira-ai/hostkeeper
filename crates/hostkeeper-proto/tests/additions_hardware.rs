//! What the hardware event log adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Hardware` service of the operator
//! API with its messages and `HardwareRefusal` (`proto/hostkeeper/operator/v1/hardware.proto`,
//! docs/design/host-health-alerts.md section 2).
//!
//! Nothing existing changed: the release before knows none of the new names, a reader of it that
//! meets one of the new messages keeps every byte, and this build reads what that release could
//! send (a message of no field) as the default of each. Those are the tests of rule B8 in both
//! directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

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

/// A record with every field set.
fn record() -> op::HardwareRecord {
    op::HardwareRecord {
        host_id: "host-a".to_owned(),
        machine_serial: "SN-0001".to_owned(),
        observed_unix: 1_795_662_540,
        boot_id: "b7c1".to_owned(),
        by: "alice".to_owned(),
        event: Some(agent::HardwareEvent {
            event_id: "sel:01a4@1790000000".to_owned(),
            time_unix: 1_795_662_012,
            of_previous_boot: true,
            component: agent::HardwareComponent::Memory.into(),
            subject: "DIMM D1".to_owned(),
            severity: agent::HardwareSeverity::Critical.into(),
            state: agent::HardwareEventState::Event.into(),
            source: "sel".to_owned(),
            code: "memory_uncorrectable".to_owned(),
            message: "Uncorrectable ECC error on DIMM D1".to_owned(),
            raw: "01a4 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted".to_owned(),
            about: String::new(),
        }),
    }
}

/// Proves: every message, the refusal enum and the service of the log are new since the release
/// before, read back as written, and kept byte for byte by a reader of that release.
/// Catches: a name the release before already knows (so not an addition), a message that does
/// not read back as written, and a reader of the release before that drops a byte of one.
#[test]
fn the_hardware_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = [
        (
            "hostkeeper.operator.v1.HardwareLogRequest",
            round_trip(&op::HardwareLogRequest {
                host_id: Some("host-a".to_owned()),
                serial: Some("SN-0001".to_owned()),
                since_unix: 1_795_000_000,
                component: agent::HardwareComponent::Disk.into(),
                severity: agent::HardwareSeverity::Warning.into(),
                limit: 50,
                page_token: "1795662012.host-a.sel:01a4@1790000000".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.HardwareRecord",
            round_trip(&record()),
        ),
        (
            "hostkeeper.operator.v1.HardwareLogResponse",
            round_trip(&op::HardwareLogResponse {
                records: vec![record(), record()],
                next_page_token: "1795662012.host-a.sel:01a4@1790000000".to_owned(),
            }),
        ),
    ];
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.HardwareRefusal")
            .is_none()
    );
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Hardware")
            .is_none()
    );
}

/// Proves: an empty message reads as the default of each: a request with no filter (every host,
/// every machine, every time, part and severity, the cap, the first page), a response of no
/// record and no next page, and a record of nothing.
/// Catches: a request of this build that a caller of the release before could not have sent, and
/// a default that reads as a filter.
#[test]
fn an_empty_message_reads_as_the_defaults_of_each_hardware_message() {
    let request = op::HardwareLogRequest::decode(&[][..]).expect("decodes");
    assert_eq!(request, op::HardwareLogRequest::default());
    assert_eq!(
        (request.host_id.as_deref(), request.serial.as_deref()),
        (None, None)
    );
    assert_eq!(request.component(), agent::HardwareComponent::Unspecified);
    assert_eq!(request.severity(), agent::HardwareSeverity::Unspecified);
    let response = op::HardwareLogResponse::decode(&[][..]).expect("decodes");
    assert_eq!(response, op::HardwareLogResponse::default());
    let record = op::HardwareRecord::decode(&[][..]).expect("decodes");
    assert!(record.event.is_none());
}

/// Proves: the numbers of the refusal values are the ones the proto fixes, and the tokens callers
/// branch on (the value's name without its prefix, in lower case) are the operator API's.
/// Catches: a value renumbered, renamed or dropped, which would turn one refusal into another for
/// an older client.
#[test]
fn the_hardware_refusals_have_fixed_numbers_and_tokens() {
    assert_eq!(
        [
            op::HardwareRefusal::Unspecified as i32,
            op::HardwareRefusal::Unauthorized as i32,
            op::HardwareRefusal::Forbidden as i32,
            op::HardwareRefusal::InvalidRequest as i32,
            op::HardwareRefusal::StoreFailed as i32,
        ],
        [0, 1, 2, 3, 4]
    );
    let current = current_pool();
    let refusal = current
        .get_enum_by_name("hostkeeper.operator.v1.HardwareRefusal")
        .expect("the enum");
    let tokens: Vec<String> = refusal
        .values()
        .skip(1)
        .map(|v| {
            v.name()
                .trim_start_matches("HARDWARE_REFUSAL_")
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "unauthorized",
            "forbidden",
            "invalid_request",
            "store_failed"
        ]
    );
}

/// Proves: the service has exactly `Log`, unary, carrying the messages the design names, and the
/// command table (so `hk` and the web app's client) lists it as `hardware log`, every request
/// field a plain flag, none secret, in the order the request declares them.
/// Catches: a call missing, renamed or streaming, one the command table does not list, and a
/// filter `hk` would not take as a flag.
#[test]
fn the_hardware_service_has_its_one_unary_call_in_the_command_table() {
    let pool = current_pool();
    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Hardware")
        .expect("this build serves Hardware");
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
    assert_eq!(
        methods,
        [(
            "Log".to_owned(),
            "HardwareLogRequest".to_owned(),
            "HardwareLogResponse".to_owned(),
            false
        )]
    );
    let commands: Vec<_> = COMMANDS
        .iter()
        .filter(|command| command.service == "Hardware")
        .collect();
    assert_eq!(commands.len(), 1);
    let command = commands[0];
    assert_eq!(
        (
            command.service_command,
            command.method_command,
            command.path
        ),
        ("hardware", "log", "/hostkeeper.operator.v1.Hardware/Log")
    );
    for field in command.fields {
        assert!(!field.secret, "{}", field.name);
        assert_eq!(field.input, Input::Flag, "{}", field.name);
    }
    let flags: Vec<&str> = command.fields.iter().map(|field| field.flag).collect();
    assert_eq!(
        flags,
        [
            "host-id",
            "serial",
            "since-unix",
            "component",
            "severity",
            "limit",
            "page-token"
        ]
    );
}
