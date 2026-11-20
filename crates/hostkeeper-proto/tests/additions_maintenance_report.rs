//! What the coordinator's keeping of maintenance reports adds to the operator API, new since the
//! committed baseline (`proto/compat/baseline.binpb`, the release before): `Host.maintenance_look_age_secs`
//! (27) and `Host.maintenance_stale` (28), `Alert.host_group` (10) and `AlertKind.UPDATES_DUE`
//! (19). A client of the release before keeps each new field of `Host` as one it does not know and
//! writes it back byte for byte, and this build reads a host that release wrote with none of them:
//! the tests of rule B8 in both directions. `Alert` is new since the baseline, so it has no reader
//! of the release before; its new field and kind are proved on the wire as written.

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

/// The numbers of the fields `read` kept as unknown, each once.
fn kept(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// Catches: a host with the look's age and the stale mark that a client of the release before
/// refuses, drops, reads its other fields of differently, or writes back changed. Proves the
/// tags on the wire: fields 27 (`0xd8 0x01`) and 28 (`0xe0 0x01`) of `Host`.
#[test]
fn an_older_client_keeps_a_hosts_look_age_and_stale_mark() {
    let older = baseline_pool();
    let host_d = older
        .get_message_by_name("hostkeeper.operator.v1.Host")
        .expect("Host");
    assert!(
        host_d
            .get_field_by_name("maintenance_look_age_secs")
            .is_none()
    );
    assert!(host_d.get_field_by_name("maintenance_stale").is_none());
    let host = op::Host {
        node_id: "mac-3".to_owned(),
        live: true,
        maintenance_look_age_secs: Some(90),
        maintenance_stale: true,
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host);
    assert!(bytes.windows(2).any(|w| w == [0xd8, 0x01]));
    assert!(bytes.windows(2).any(|w| w == [0xe0, 0x01]));
    let read = DynamicMessage::decode(host_d, bytes.as_slice()).expect("decodes");
    assert_eq!(kept(&read), [27, 28]);
    assert_eq!(
        read.get_field_by_name("node_id").as_deref(),
        Some(&Value::String("mac-3".to_owned()))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an alert of the new kind, or its host group, lost or read as something else. `Alert`
/// itself is new since the baseline (`additions.rs` round-trips it with every field set), so no
/// client of the release before reads one: this proves the tag on the wire, field 10 (`0x52`), and
/// the kind's number 19, read back as written.
#[test]
fn an_updates_due_alert_and_its_group_read_back_as_written() {
    assert!(
        baseline_pool()
            .get_message_by_name("hostkeeper.operator.v1.Alert")
            .is_none(),
        "Alert is new since the baseline"
    );
    let alert = op::Alert {
        kind: op::AlertKind::UpdatesDue.into(),
        severity: op::AlertSeverity::Warning.into(),
        message: "Updates due on 2 hosts of web".to_owned(),
        host_group: "web".to_owned(),
        ..op::Alert::default()
    };
    let bytes = alert.encode_to_vec();
    assert_eq!(&bytes[..2], [0x08, 19]);
    assert!(bytes.windows(4).any(|w| w == [0x52, 3, b'w', b'e']));
    assert_eq!(op::Alert::decode(bytes.as_slice()).expect("decodes"), alert);
}

/// Catches: a host of the release before read with a look age or a stale mark nobody wrote: it
/// reads with none, and the fields it had as they were written.
#[test]
fn the_release_befores_hosts_read_without_the_new_fields() {
    let older = baseline_pool();
    let mut host = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.operator.v1.Host")
            .expect("Host"),
    );
    host.set_field_by_name("node_id", Value::String("db-1".to_owned()));
    let read = op::Host::decode(host.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.maintenance_look_age_secs, None, "absent, not 0");
    assert!(!read.maintenance_stale);
    assert_eq!(read.node_id, "db-1");
}

/// Catches: the look's age read as absent when it is 0, or a zero age lost on the wire: a look
/// at the coordinator's own second is a 0, which `optional` keeps apart from no look.
#[test]
fn a_zero_look_age_is_distinct_from_absent() {
    let zero = op::Host {
        maintenance_look_age_secs: Some(0),
        ..op::Host::default()
    };
    assert_eq!(zero.encode_to_vec(), [0xd8, 0x01, 0]);
    assert_eq!(
        op::Host::decode([0xd8, 0x01, 0].as_slice()).expect("decodes"),
        zero
    );
    assert_eq!(
        op::Host::decode(&[][..])
            .expect("decodes")
            .maintenance_look_age_secs,
        None
    );
}

/// Catches: a new field or value given another number, label or type than the schema says, which
/// a peer built apart would read as something else.
#[test]
fn the_new_fields_and_the_kind_have_their_numbers_and_types() {
    let now = current_pool();
    let host = now
        .get_message_by_name("hostkeeper.operator.v1.Host")
        .expect("Host");
    let age = host
        .get_field_by_name("maintenance_look_age_secs")
        .expect("age");
    assert_eq!(
        (age.number(), age.kind(), age.supports_presence()),
        (27, prost_reflect::Kind::Uint64, true)
    );
    let stale = host.get_field_by_name("maintenance_stale").expect("stale");
    assert_eq!(
        (stale.number(), stale.kind(), stale.supports_presence()),
        (28, prost_reflect::Kind::Bool, false)
    );
    let alert = now
        .get_message_by_name("hostkeeper.operator.v1.Alert")
        .expect("Alert");
    let group = alert.get_field_by_name("host_group").expect("group");
    assert_eq!(
        (group.number(), group.kind(), group.is_list()),
        (10, prost_reflect::Kind::String, false)
    );
    let kind = now
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("AlertKind");
    let value = kind
        .get_value_by_name("ALERT_KIND_UPDATES_DUE")
        .expect("value");
    assert_eq!(value.number(), 19);
    assert_eq!(op::AlertKind::UpdatesDue as i32, 19);
}
