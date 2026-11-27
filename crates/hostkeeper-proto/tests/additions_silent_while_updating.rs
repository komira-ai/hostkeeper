//! What the alert of a host silent mid-update adds to the operator API: the kind
//! `ALERT_KIND_SILENT_WHILE_UPDATING` (33), and nothing else.
//!
//! The release before's reader is this build's descriptor set with the value 33 of `AlertKind`
//! taken out. Rule B8 in both directions:
//!
//! - that reader reads an alert of the new kind, keeps the kind as the number it does not name and
//!   every other field as written, and writes it back byte for byte;
//! - this build reads an alert of the release before (a `silent_host`) as written.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set of the release before: this build's, with the value 33 of `AlertKind` taken
/// out, every other message and value as it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let mut values = 0;
    for file in &mut set.file {
        if file.package() != "hostkeeper.operator.v1" {
            continue;
        }
        for kind in &mut file.enum_type {
            if kind.name() == "AlertKind" {
                let before = kind.value.len();
                kind.value.retain(|v| v.number() != 33);
                values += before - kind.value.len();
            }
        }
    }
    assert_eq!(
        values, 1,
        "this build has exactly one value past the release before"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
}

/// The message `Alert` of `pool`.
fn alert_of(pool: &DescriptorPool) -> MessageDescriptor {
    pool.get_message_by_name("hostkeeper.operator.v1.Alert")
        .expect("Alert")
}

/// Proves: a reader of the release before reads an alert of kind 33 as the number it does not
/// name, its host, service, version and message as written, and writes back the bytes it read.
/// Catches: the alert of a host silent mid-update that a client of the release before refuses,
/// reads as another kind, or writes back changed.
#[test]
fn an_older_reader_keeps_a_silent_while_updating_alert_and_writes_it_back() {
    let alert = op::Alert {
        kind: op::AlertKind::SilentWhileUpdating.into(),
        severity: op::AlertSeverity::Critical.into(),
        host_id: "d1".to_owned(),
        service: "store".to_owned(),
        version: "2.0".to_owned(),
        message: "d1 has not heartbeated for 3600 s".to_owned(),
        first_observed_unix: 1_800_000_000,
        ..op::Alert::default()
    };
    let bytes = alert.encode_to_vec();
    let read = DynamicMessage::decode(alert_of(&before_pool()), bytes.as_slice())
        .expect("the release before reads it");
    assert_eq!(
        read.get_field_by_name("kind").as_deref(),
        Some(&Value::EnumNumber(33))
    );
    for (field, want) in [("host_id", "d1"), ("service", "store"), ("version", "2.0")] {
        assert_eq!(
            read.get_field_by_name(field).as_deref(),
            Some(&Value::String(want.to_owned())),
            "{field}"
        );
    }
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Proves: this build reads an alert the release before wrote (a `silent_host` of a host) as
/// written, and its kind is still `ALERT_KIND_SILENT_HOST`.
/// Catches: a kind of the release before renumbered or read as the new one.
#[test]
fn this_build_reads_an_alert_of_the_release_before_as_written() {
    let mut written = DynamicMessage::new(alert_of(&before_pool()));
    written.set_field_by_name("kind", Value::EnumNumber(8));
    written.set_field_by_name("host_id", Value::String("d1".to_owned()));
    let read = op::Alert::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read,
        op::Alert {
            kind: op::AlertKind::SilentHost.into(),
            host_id: "d1".to_owned(),
            ..op::Alert::default()
        }
    );
}

/// Proves: `ALERT_KIND_SILENT_WHILE_UPDATING` is 33 by name in the descriptors and in the
/// generated type.
/// Catches: the value given another number than the schema says, which a peer built apart would
/// read as another kind.
#[test]
fn the_new_value_has_its_number() {
    let now = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let kinds = now
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("AlertKind");
    let value = kinds
        .get_value_by_name("ALERT_KIND_SILENT_WHILE_UPDATING")
        .expect("value");
    assert_eq!(value.number(), 33);
    assert_eq!(op::AlertKind::SilentWhileUpdating as i32, 33);
}
