//! What the maintenance alerts add to the operator API, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): `AlertKind.MAINTENANCE_HALTED` (22) and
//! `AlertKind.MAINTENANCE_NOT_BACK` (23). `Alert` and `AlertKind` are new since the baseline, so
//! no reader of the release before has them: what is proved is that the two values are on the wire
//! as written, that a reader that does not know a kind's number keeps it as the number and writes
//! it back byte for byte, and that an alert of a kind of the release before still reads as it was
//! written: the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

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

/// Catches: a new kind given another number than the schema says, or an `Alert` that the release
/// before knew (which would then have to read the new values), since a peer built apart would
/// read the number as something else.
#[test]
fn the_two_kinds_have_their_numbers_and_the_release_before_has_no_alert() {
    let older = baseline_pool();
    assert!(
        older
            .get_message_by_name("hostkeeper.operator.v1.Alert")
            .is_none(),
        "Alert is new since the baseline"
    );
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
            .is_none(),
        "AlertKind is new since the baseline"
    );
    let kind = current_pool()
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("AlertKind");
    for (name, number) in [
        ("ALERT_KIND_MAINTENANCE_HALTED", 22),
        ("ALERT_KIND_MAINTENANCE_NOT_BACK", 23),
    ] {
        let value = kind.get_value_by_name(name).expect("value");
        assert_eq!(value.number(), number, "{name}");
    }
    assert_eq!(op::AlertKind::MaintenanceHalted as i32, 22);
    assert_eq!(op::AlertKind::MaintenanceNotBack as i32, 23);
}

/// Catches: an alert of a new kind, or its host group, lost or read as another kind: proves the
/// tag on the wire, field 1 (`0x08`) holding 22 or 23, and the group in field 10 (`0x52`), read
/// back as written by this build and by a reader that builds the message from the descriptor.
#[test]
fn a_maintenance_alert_reads_back_as_written() {
    for (kind, number) in [
        (op::AlertKind::MaintenanceHalted, 22),
        (op::AlertKind::MaintenanceNotBack, 23),
    ] {
        let alert = op::Alert {
            kind: kind.into(),
            severity: op::AlertSeverity::Critical.into(),
            host_id: "h1".to_owned(),
            message: "h1 halted".to_owned(),
            host_group: "web".to_owned(),
            ..op::Alert::default()
        };
        let bytes = alert.encode_to_vec();
        assert_eq!(&bytes[..2], [0x08, number]);
        assert!(bytes.windows(4).any(|w| w == [0x52, 3, b'w', b'e']));
        assert_eq!(op::Alert::decode(bytes.as_slice()).expect("decodes"), alert);
        let described = DynamicMessage::decode(
            current_pool()
                .get_message_by_name("hostkeeper.operator.v1.Alert")
                .expect("Alert"),
            bytes.as_slice(),
        )
        .expect("decodes");
        assert_eq!(
            described.get_field_by_name("kind").as_deref(),
            Some(&Value::EnumNumber(i32::from(number)))
        );
        assert_eq!(described.encode_to_vec(), bytes);
    }
}

/// Catches: a reader that does not know a kind's number, as a reader of the release before does
/// not know 22 and 23, that refuses the alert, drops it or writes it back changed: the number is
/// kept as the number (90 stands for a kind no release has yet) and the alert's other fields read
/// as written.
#[test]
fn a_reader_that_does_not_know_a_kind_keeps_its_number_and_the_alert() {
    assert!(op::AlertKind::try_from(90).is_err(), "90 is no kind yet");
    let alert = op::Alert {
        kind: 90,
        severity: op::AlertSeverity::Critical.into(),
        host_id: "h1".to_owned(),
        message: "a later kind".to_owned(),
        host_group: "web".to_owned(),
        ..op::Alert::default()
    };
    let bytes = alert.encode_to_vec();
    let read = op::Alert::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read, alert);
    assert_eq!(read.kind, 90);
    assert_eq!(read.host_id, "h1");
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an alert of a kind of the release before (`UPDATES_DUE`, 19) that this build reads
/// differently from the way it was written, now that two kinds follow it.
#[test]
fn an_alert_of_an_older_kind_reads_as_it_was_written() {
    let alert = op::Alert {
        kind: op::AlertKind::UpdatesDue.into(),
        severity: op::AlertSeverity::Warning.into(),
        message: "Updates due on 2 hosts of web".to_owned(),
        host_group: "web".to_owned(),
        ..op::Alert::default()
    };
    let bytes = alert.encode_to_vec();
    assert_eq!(&bytes[..2], [0x08, 19]);
    let read = op::Alert::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read.kind(), op::AlertKind::UpdatesDue);
    assert_eq!(read, alert);
}
