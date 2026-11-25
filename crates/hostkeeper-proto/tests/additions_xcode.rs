//! What the Xcode store adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Xcode` service of the operator API
//! with its messages and `XcodeRefusal` (`proto/hostkeeper/operator/v1/xcode.proto`,
//! docs/design/maintenance-updates.md), and the alert kind `XCODE_VERSION_TAKEN` (18) of
//! `AlertKind`.
//!
//! Nothing existing changed: the release before knows none of the new names, a reader of it that
//! meets one of the new messages keeps every byte, and this build reads what that release wrote
//! (a message of no field) as the default of each. Those are the tests of rule B8 in both
//! directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::ctl::{COMMANDS, Input};
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

/// An archive with every field set, its size past `u32`.
fn archive() -> op::XcodeArchive {
    op::XcodeArchive {
        sha256: "ab".repeat(32),
        version: "26.1.1".to_owned(),
        build: "17B100".to_owned(),
        size_bytes: 19_000_000_000,
        uploaded_by: "alice".to_owned(),
        uploaded_at_unix: 1_800_000_000,
    }
}

/// A waiting release with every field set.
fn waiting() -> op::XcodeReleaseWaiting {
    op::XcodeReleaseWaiting {
        version: "26.2".to_owned(),
        build: "17C52".to_owned(),
        requires_macos: "15.6".to_owned(),
    }
}

/// One message of each type the Xcode store added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.operator.v1.ListXcodeRequest",
            round_trip(&op::ListXcodeRequest {}),
        ),
        (
            "hostkeeper.operator.v1.XcodeArchive",
            round_trip(&archive()),
        ),
        (
            "hostkeeper.operator.v1.XcodeReleaseWaiting",
            round_trip(&waiting()),
        ),
        (
            "hostkeeper.operator.v1.ListXcodeResponse",
            round_trip(&op::ListXcodeResponse {
                configured: true,
                archives: vec![archive()],
                waiting: Some(waiting()),
                keep: 2,
                max_bytes: 21_474_836_480,
                uploads: Vec::new(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveXcodeRequest",
            round_trip(&op::RemoveXcodeRequest {
                sha256: "ab".repeat(32),
                reason: "the build was withdrawn".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveXcodeResponse",
            round_trip(&op::RemoveXcodeResponse {
                entry: Some(op::AuditEntry {
                    seq: 7,
                    action: "xcode_remove".to_owned(),
                    ..op::AuditEntry::default()
                }),
            }),
        ),
    ]
}

/// Proves: every message, the refusal enum and the service of the Xcode store are new since the
/// release before, read back as written, and kept byte for byte by a reader of that release.
/// Catches: a name the release before already knows (so not an addition), a message that does
/// not read back as written, and a reader of the release before that drops a byte of one.
#[test]
fn the_xcode_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 6);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.XcodeRefusal")
            .is_none()
    );
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Xcode")
            .is_none()
    );
}

/// Proves: an empty message reads as the default of each Xcode message: a store that is not
/// configured, no archive, no waiting release, nothing kept and no upload limit; a removal that
/// names no archive and gives no reason.
/// Catches: a request of this build that a caller of the release before could not have sent, and
/// a default that reads as a store (`configured` true) or as an archive.
#[test]
fn an_empty_message_reads_as_the_defaults_of_each_xcode_message() {
    let list = op::ListXcodeResponse::decode(&[][..]).expect("decodes");
    assert_eq!(
        (
            list.configured,
            list.archives.len(),
            list.waiting.is_some(),
            list.keep,
            list.max_bytes
        ),
        (false, 0, false, 0, 0)
    );
    assert_eq!(list, op::ListXcodeResponse::default());
    let remove = op::RemoveXcodeRequest::decode(&[][..]).expect("decodes");
    assert_eq!(remove, op::RemoveXcodeRequest::default());
    assert_eq!((remove.sha256.as_str(), remove.reason.as_str()), ("", ""));
    let archive = op::XcodeArchive::decode(&[][..]).expect("decodes");
    assert_eq!(archive, op::XcodeArchive::default());
    let removed = op::RemoveXcodeResponse::decode(&[][..]).expect("decodes");
    assert!(removed.entry.is_none());
}

/// Proves: the numbers of the refusal values and of the new alert kind are the ones the protos
/// fix, an unknown number is read back as the number and judged unspecified, and the tokens
/// callers branch on (the value's name without its prefix, in lower case) are the design's.
/// Catches: a value renumbered, renamed or dropped, which would turn one refusal into another for
/// an older client.
#[test]
fn the_xcode_refusals_and_the_alert_kind_have_fixed_numbers_and_tokens() {
    assert_eq!(
        [
            op::XcodeRefusal::Unspecified as i32,
            op::XcodeRefusal::Unauthorized as i32,
            op::XcodeRefusal::Forbidden as i32,
            op::XcodeRefusal::InvalidRequest as i32,
            op::XcodeRefusal::NoStore as i32,
            op::XcodeRefusal::UnknownArchive as i32,
            op::XcodeRefusal::StoreFailed as i32,
            op::XcodeRefusal::AuditUnavailable as i32,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7]
    );
    let current = current_pool();
    let refusal = current
        .get_enum_by_name("hostkeeper.operator.v1.XcodeRefusal")
        .expect("the enum");
    let tokens: Vec<String> = refusal
        .values()
        .skip(1)
        .map(|v| {
            v.name()
                .trim_start_matches("XCODE_REFUSAL_")
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "unauthorized",
            "forbidden",
            "invalid_request",
            "no_store",
            "unknown_archive",
            "store_failed",
            "audit_unavailable",
        ]
    );
    assert_eq!(op::AlertKind::XcodeVersionTaken as i32, 18);
    assert_eq!(
        op::AlertKind::XcodeVersionTaken.as_str_name(),
        "ALERT_KIND_XCODE_VERSION_TAKEN"
    );
    let future = op::Alert {
        kind: 99,
        ..op::Alert::default()
    };
    let read = op::Alert::decode(round_trip(&future).as_slice()).expect("decodes");
    assert_eq!(read.kind, 99);
    assert_eq!(read.kind(), op::AlertKind::Unspecified);
}

/// Proves: the service has exactly `List` and `Remove`, each unary and carrying the messages the
/// design names, and the command table (so `hk` and the web app's client) lists them as
/// `xcode list` and `xcode remove`.
/// Catches: a call missing, renamed or streaming, and one the command table does not list.
#[test]
fn the_xcode_service_has_its_two_unary_calls_in_the_command_table() {
    let pool = current_pool();
    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Xcode")
        .expect("this build serves Xcode");
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
        ("List", "ListXcodeRequest", "ListXcodeResponse"),
        ("Remove", "RemoveXcodeRequest", "RemoveXcodeResponse"),
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
        .filter(|command| command.service == "Xcode")
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
            ("xcode", "list", "/hostkeeper.operator.v1.Xcode/List"),
            ("xcode", "remove", "/hostkeeper.operator.v1.Xcode/Remove"),
        ]
    );
}

/// Proves: `xcode list` takes no flag, `xcode remove` takes `--sha256` and `--reason` as plain
/// flags, and no field of the Xcode store is marked secret.
/// Catches: a request field that `hk` would not take as a flag, and a SHA-256 or a reason
/// marked secret, which would hide it from the command line.
#[test]
fn every_xcode_request_field_is_a_flag_and_none_is_secret() {
    let mut flags = Vec::new();
    for command in COMMANDS.iter().filter(|command| command.service == "Xcode") {
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
    assert_eq!(flags, [("remove", "sha256"), ("remove", "reason")]);
}
