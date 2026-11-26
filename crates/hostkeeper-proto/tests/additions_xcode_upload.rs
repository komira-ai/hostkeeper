//! What the uploads that resume add to the wire (`proto/hostkeeper/operator/v1/xcode.proto`,
//! docs/design/xcode-one-action.md, section 3): `ListXcodeResponse.uploads` (6), a list of the new
//! message `XcodeUpload`, and the refusal value `XCODE_REFUSAL_PARTIAL_LOCKED` (8).
//!
//! The committed baseline (`proto/compat/baseline.binpb`) predates the `Xcode` service, so the
//! release before is this build's descriptor set with field 6 of `ListXcodeResponse`, the message
//! `XcodeUpload` and the value 8 of `XcodeRefusal` taken out. Rule B8 in both directions:
//!
//! - that reader reads a list that names uploads, keeps the field as an unknown field and writes
//!   the list back byte for byte, with every field it knows read as written;
//! - this build reads a list that release wrote, which has no uploads, as one with none;
//! - the value 8 of the refusal, which travels as a status message and not as a field, has the
//!   number and token the design names, and the release before does not know it.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// The descriptor set of the release before: this build's, with field 6 of `ListXcodeResponse`,
/// the message `XcodeUpload` and the value 8 of `XcodeRefusal` taken out, every other message and
/// value as it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let (mut fields, mut messages, mut values) = (0, 0, 0);
    for file in &mut set.file {
        if file.package() != "hostkeeper.operator.v1" {
            continue;
        }
        for message in &mut file.message_type {
            if message.name() == "ListXcodeResponse" {
                let before = message.field.len();
                message.field.retain(|f| f.number() != 6);
                fields += before - message.field.len();
            }
        }
        let before = file.message_type.len();
        file.message_type.retain(|m| m.name() != "XcodeUpload");
        messages += before - file.message_type.len();
        for refusal in &mut file.enum_type {
            if refusal.name() == "XcodeRefusal" {
                let before = refusal.value.len();
                refusal.value.retain(|v| v.number() != 8);
                values += before - refusal.value.len();
            }
        }
    }
    assert_eq!(
        (fields, messages, values),
        (1, 1, 1),
        "this build has exactly one field, one message and one value past the release before"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// An upload with every field set, its sizes past `u32`.
fn upload(digit: &str) -> op::XcodeUpload {
    op::XcodeUpload {
        sha256: digit.repeat(64),
        version: "27.2".to_owned(),
        build: "27B52".to_owned(),
        received_bytes: 7_310_000_000,
        size_bytes: 11_920_523_904,
        started_by: "alice".to_owned(),
        last_write_unix: 1_800_000_000,
    }
}

/// A list with every field of it set, two uploads among them.
fn list() -> op::ListXcodeResponse {
    op::ListXcodeResponse {
        configured: true,
        archives: vec![op::XcodeArchive {
            sha256: "ab".repeat(32),
            version: "27.1".to_owned(),
            build: "27B40".to_owned(),
            size_bytes: 11_000_000_000,
            uploaded_by: "bob".to_owned(),
            uploaded_at_unix: 1_799_000_000,
        }],
        waiting: Some(op::XcodeReleaseWaiting {
            version: "27.2".to_owned(),
            build: "27B52".to_owned(),
            requires_macos: "26.6".to_owned(),
        }),
        keep: 2,
        max_bytes: 21_474_836_480,
        uploads: vec![upload("c"), upload("d")],
    }
}

/// Proves: a reader of the release before reads a list that names uploads, keeps field 6 as one it
/// does not know and every other field as written, and writes the list back byte for byte; the
/// generated type reads those bytes back equal to the original, and field 6 is on the wire as a
/// length-delimited field (`0x32`).
/// Catches: a list with uploads that an older client or coordinator refuses, drops, or changes,
/// or that makes it lose a field it knows.
#[test]
fn the_reader_of_the_release_before_keeps_the_uploads_and_writes_them_back() {
    let before = before_pool();
    let current = current_pool();
    assert!(
        message(&before, "hostkeeper.operator.v1.ListXcodeResponse")
            .get_field_by_name("uploads")
            .is_none()
    );
    assert!(
        before
            .get_message_by_name("hostkeeper.operator.v1.XcodeUpload")
            .is_none()
    );
    assert_eq!(
        message(&current, "hostkeeper.operator.v1.ListXcodeResponse")
            .get_field_by_name("uploads")
            .map(|field| field.number()),
        Some(6)
    );

    let original = list();
    let bytes = original.encode_to_vec();
    assert_eq!(
        op::ListXcodeResponse::decode(bytes.as_slice()).expect("decodes"),
        original
    );
    assert!(bytes.contains(&0x32));
    let read = DynamicMessage::decode(
        message(&before, "hostkeeper.operator.v1.ListXcodeResponse"),
        bytes.as_slice(),
    )
    .expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [6]);
    assert_eq!(
        read.get_field_by_name("configured").as_deref(),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        read.get_field_by_name("keep").as_deref(),
        Some(&Value::U32(2))
    );
    assert_eq!(
        read.get_field_by_name("max_bytes").as_deref(),
        Some(&Value::U64(21_474_836_480))
    );
    let Some(archives) = read.get_field_by_name("archives") else {
        panic!("archives");
    };
    assert_eq!(archives.as_list().map(<[Value]>::len), Some(1));
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
    assert_eq!(
        op::ListXcodeResponse::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        original
    );
}

/// Proves: a list the release before wrote, which has no field 6, reads in this build as a list
/// with no uploads and every other field as written, and a message that release wrote of no field
/// reads as the default.
/// Catches: a default that reads as an upload in progress, and a list of the release before that
/// this build refuses.
#[test]
fn this_build_reads_a_list_of_the_release_before_as_having_no_uploads() {
    let before = before_pool();
    let mut older =
        DynamicMessage::new(message(&before, "hostkeeper.operator.v1.ListXcodeResponse"));
    older.set_field_by_name("configured", Value::Bool(true));
    older.set_field_by_name("keep", Value::U32(2));
    older.set_field_by_name("max_bytes", Value::U64(21_474_836_480));
    let bytes = older.encode_to_vec();

    let read = op::ListXcodeResponse::decode(bytes.as_slice()).expect("decodes");

    assert_eq!(
        read,
        op::ListXcodeResponse {
            configured: true,
            keep: 2,
            max_bytes: 21_474_836_480,
            ..op::ListXcodeResponse::default()
        }
    );
    assert!(read.uploads.is_empty());
    assert_eq!(
        op::ListXcodeResponse::decode(&[][..]).expect("decodes"),
        op::ListXcodeResponse::default()
    );
}

/// Proves: the numbers of the fields of `XcodeUpload` are the ones the proto fixes (1 to 7, in
/// the order of its message), an upload reads back as written, and an empty one reads as the
/// default of each field.
/// Catches: a field renumbered or retyped, which would turn one value into another for an older
/// reader of the same bytes.
#[test]
fn the_upload_message_has_fixed_field_numbers_and_reads_back_as_written() {
    let current = current_pool();
    let fields: Vec<(String, u32)> = message(&current, "hostkeeper.operator.v1.XcodeUpload")
        .fields()
        .map(|field| (field.name().to_owned(), field.number()))
        .collect();
    assert_eq!(
        fields,
        [
            ("sha256".to_owned(), 1),
            ("version".to_owned(), 2),
            ("build".to_owned(), 3),
            ("received_bytes".to_owned(), 4),
            ("size_bytes".to_owned(), 5),
            ("started_by".to_owned(), 6),
            ("last_write_unix".to_owned(), 7),
        ]
    );
    let original = upload("e");
    assert_eq!(
        op::XcodeUpload::decode(original.encode_to_vec().as_slice()).expect("decodes"),
        original
    );
    assert_eq!(
        op::XcodeUpload::decode(&[][..]).expect("decodes"),
        op::XcodeUpload::default()
    );
}

/// Proves: `XCODE_REFUSAL_PARTIAL_LOCKED` is the value 8, its token (the name without the prefix,
/// in lower case) is `partial_locked`, the values before it keep their numbers, and the release
/// before does not know the value.
/// Catches: a value renumbered or renamed, which would turn one refusal into another for an older
/// client, and a value the release before already had (so not an addition).
#[test]
fn the_partial_locked_refusal_is_value_eight_and_new() {
    assert_eq!(op::XcodeRefusal::PartialLocked as i32, 8);
    assert_eq!(
        op::XcodeRefusal::PartialLocked.as_str_name(),
        "XCODE_REFUSAL_PARTIAL_LOCKED"
    );
    assert_eq!(op::XcodeRefusal::AuditUnavailable as i32, 7);
    let current = current_pool();
    let refusal = current
        .get_enum_by_name("hostkeeper.operator.v1.XcodeRefusal")
        .expect("the enum");
    let tokens: Vec<String> = refusal
        .values()
        .filter(|value| value.number() >= 7)
        .map(|value| {
            value
                .name()
                .trim_start_matches("XCODE_REFUSAL_")
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(tokens, ["audit_unavailable", "partial_locked"]);
    let before = before_pool();
    let older = before
        .get_enum_by_name("hostkeeper.operator.v1.XcodeRefusal")
        .expect("the enum");
    assert!(older.get_value(8).is_none());
    assert_eq!(older.values().count(), 8);
}
