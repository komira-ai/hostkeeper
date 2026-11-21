//! What the alerts of a failed update and of a host's own halt add to the operator API:
//! `Alert.reason` (11), `Alert.detail` (12), `Alert.tries` (13) and `Alert.since_unix` (14), the
//! kinds `ALERT_KIND_FAILED_UPDATE` (20) and `ALERT_KIND_HOST_HALT` (21), and the plan's wait
//! `PLAN_WAIT_NOTHING_TO_ORDER` (12).
//!
//! The committed baseline (`proto/compat/baseline.binpb`) predates `Alert` and `RolloutPlan`, so
//! the release before's reader is this build's descriptor set with those fields and values taken
//! out. Rule B8 in both directions:
//!
//! - that reader reads an alert of a new kind with the new fields, and a plan that waits
//!   `nothing_to_order`, keeps what it does not know (the fields as unknown fields, the values as
//!   numbers) and writes each back byte for byte;
//! - this build reads an alert that release wrote with each new field empty.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// The descriptor set of the release before: this build's, with fields 11 to 14 of `Alert`, the
/// values 20 and 21 of `AlertKind` and the value 12 of `PlanWait` taken out, every other message
/// and value as it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let (mut fields, mut values) = (0, 0);
    for file in &mut set.file {
        let operator = file.package() == "hostkeeper.operator.v1";
        for message in &mut file.message_type {
            if operator && message.name() == "Alert" {
                let before = message.field.len();
                message.field.retain(|f| !(11..=14).contains(&f.number()));
                fields += before - message.field.len();
            }
        }
        for kind in &mut file.enum_type {
            let new: &[i32] = match kind.name() {
                "AlertKind" => &[20, 21],
                "PlanWait" => &[12],
                _ => &[],
            };
            let before = kind.value.len();
            kind.value.retain(|v| !new.contains(&v.number()));
            values += before - kind.value.len();
        }
    }
    assert_eq!(
        (fields, values),
        (4, 3),
        "this build has exactly four fields and three values past the release before"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
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

/// A failed update's alert with every field it carries set.
fn failed_update() -> op::Alert {
    op::Alert {
        kind: op::AlertKind::FailedUpdate.into(),
        severity: op::AlertSeverity::Critical.into(),
        host_id: "build-2".to_owned(),
        service: "tierstore".to_owned(),
        version: "0.3.150".to_owned(),
        message: "build-2 failed tierstore 0.3.150: config_missing".to_owned(),
        first_observed_unix: 1_800_000_000,
        reason: "config_missing".to_owned(),
        detail: "there is no configuration file".to_owned(),
        tries: 3,
        since_unix: 1_799_990_000,
        ..op::Alert::default()
    }
}

/// Catches: an alert of a new kind, or one of its new fields, that a client of the release before
/// refuses, drops, reads its other fields of differently, or writes back changed.
#[test]
fn an_older_reader_keeps_a_failed_update_alert_and_writes_it_back() {
    let older = before_pool();
    for alert in [
        failed_update(),
        op::Alert {
            kind: op::AlertKind::HostHalt.into(),
            ..failed_update()
        },
    ] {
        let bytes = alert.encode_to_vec();
        let read = DynamicMessage::decode(message(&older, "hostkeeper.operator.v1.Alert"), &*bytes)
            .expect("the release before reads it");
        assert_eq!(kept(&read), vec![11, 12, 13, 14]);
        assert_eq!(
            read.get_field_by_name("kind").as_deref(),
            Some(&Value::EnumNumber(alert.kind))
        );
        assert_eq!(
            read.get_field_by_name("host_id").as_deref(),
            Some(&Value::String("build-2".to_owned()))
        );
        assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
    }
}

/// Catches: a plan that waits `nothing_to_order` that a client of the release before cannot read,
/// or writes back as another wait.
#[test]
fn an_older_reader_keeps_a_plan_with_nothing_to_order() {
    let plan = op::RolloutPlan {
        version: "2.0".to_owned(),
        eligible: 4,
        current: 1,
        waiting: op::PlanWait::NothingToOrder.into(),
        host_group: "mac".to_owned(),
        ..op::RolloutPlan::default()
    };
    let bytes = plan.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&before_pool(), "hostkeeper.operator.v1.RolloutPlan"),
        &*bytes,
    )
    .expect("the release before reads it");
    assert_eq!(
        read.get_field_by_name("waiting").as_deref(),
        Some(&Value::EnumNumber(12))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: an alert of the release before read with a reason, a detail, a try or a time nobody
/// wrote: it reads with none, and the fields it had as they were written.
#[test]
fn this_build_reads_an_alert_of_the_release_before_with_no_new_field() {
    let older = before_pool();
    let mut written = DynamicMessage::new(message(&older, "hostkeeper.operator.v1.Alert"));
    written.set_field_by_name("kind", Value::EnumNumber(op::AlertKind::Halt.into()));
    written.set_field_by_name("service", Value::String("web".to_owned()));
    written.set_field_by_name("message", Value::String("web is halted".to_owned()));
    let read = op::Alert::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read,
        op::Alert {
            kind: op::AlertKind::Halt.into(),
            service: "web".to_owned(),
            message: "web is halted".to_owned(),
            ..op::Alert::default()
        }
    );
}

/// Catches: a new field or value given another number, label or type than the schema says, which
/// a peer built apart would read as something else.
#[test]
fn the_new_fields_and_values_have_their_numbers_and_types() {
    let now = current_pool();
    let alert = message(&now, "hostkeeper.operator.v1.Alert");
    for (name, number, kind) in [
        ("reason", 11, prost_reflect::Kind::String),
        ("detail", 12, prost_reflect::Kind::String),
        ("tries", 13, prost_reflect::Kind::Uint64),
        ("since_unix", 14, prost_reflect::Kind::Uint64),
    ] {
        let field = alert.get_field_by_name(name).expect(name);
        assert_eq!(
            (
                field.number(),
                field.kind(),
                field.is_list(),
                field.supports_presence()
            ),
            (number, kind, false, false),
            "{name}"
        );
    }
    let kinds = now
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("AlertKind");
    for (name, number) in [
        ("ALERT_KIND_FAILED_UPDATE", 20),
        ("ALERT_KIND_HOST_HALT", 21),
    ] {
        assert_eq!(kinds.get_value_by_name(name).expect(name).number(), number);
    }
    assert_eq!(
        (
            op::AlertKind::FailedUpdate as i32,
            op::AlertKind::HostHalt as i32
        ),
        (20, 21)
    );
    let waits = now
        .get_enum_by_name("hostkeeper.operator.v1.PlanWait")
        .expect("PlanWait");
    assert_eq!(
        waits
            .get_value_by_name("PLAN_WAIT_NOTHING_TO_ORDER")
            .expect("value")
            .number(),
        12
    );
    assert_eq!(op::PlanWait::NothingToOrder as i32, 12);
}
