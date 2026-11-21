//! What a host group's `failed_hosts` and the alert of a repeated deferral add to the operator API
//! (`docs/operations/rollout-plans.md`, A failed host): `RolloutPolicy.failed_hosts` (11), the new
//! `FailedHosts` enum, and the kind `ALERT_KIND_REPEATED_DEFERRAL` (24).
//!
//! The committed baseline (`proto/compat/baseline.binpb`) predates `hostgroups.proto` and `Alert`,
//! so the release before's reader is this build's descriptor set with field 11 of `RolloutPolicy`,
//! the enum `FailedHosts` and the value 24 of `AlertKind` taken out. Rule B8 in both directions:
//!
//! - that reader reads a policy, a group and an answer naming `failed_hosts`, and an alert of the
//!   new kind, keeps what it does not know (the field as an unknown field, the value as a number)
//!   and writes each back byte for byte; its JSON reader refuses the key by name, so a coordinator
//!   before this change refuses such a body loudly;
//! - this build reads a policy that release wrote with `failed_hosts` unspecified (0), which the
//!   coordinator reads as `continue`, the default.

use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, Kind, MessageDescriptor, Value,
};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// The descriptor set of the release before: this build's, with field 11 of `RolloutPolicy`, the
/// enum `FailedHosts` and the value 24 of `AlertKind` taken out, every other message and value as
/// it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let (mut fields, mut enums, mut values) = (0, 0, 0);
    for file in &mut set.file {
        if file.package() != "hostkeeper.operator.v1" {
            continue;
        }
        for message in &mut file.message_type {
            if message.name() == "RolloutPolicy" {
                let before = message.field.len();
                message.field.retain(|f| f.number() != 11);
                fields += before - message.field.len();
            }
        }
        let before = file.enum_type.len();
        file.enum_type.retain(|e| e.name() != "FailedHosts");
        enums += before - file.enum_type.len();
        for kind in &mut file.enum_type {
            if kind.name() == "AlertKind" {
                let before = kind.value.len();
                kind.value.retain(|v| v.number() != 24);
                values += before - kind.value.len();
            }
        }
    }
    assert_eq!(
        (fields, enums, values),
        (1, 1, 1),
        "this build has exactly one field, one enum and one value past the release before"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// The numbers of the fields `read` kept as fields it does not know, after following the message
/// fields `path` down from it.
fn unknown_at(read: &DynamicMessage, path: &[&str]) -> Vec<u32> {
    let mut at = read.clone();
    for name in path {
        let value = at.get_field_by_name(name).expect("a field");
        at = match value.as_ref() {
            Value::Message(inner) => inner.clone(),
            other => panic!("{name}: {other:?}"),
        };
    }
    let mut kept: Vec<u32> = at.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// A policy with keys of the release before set and failed hosts that stop their stage.
fn policy() -> op::RolloutPolicy {
    op::RolloutPolicy {
        bake_beats: 3,
        min_up: 2,
        live_rollouts: Some(3),
        failed_hosts: op::FailedHosts::Stop.into(),
        ..op::RolloutPolicy::default()
    }
}

/// Proves: a reader of the release before reads a policy naming `failed_hosts`, alone and inside a
/// group and a `Get` answer, keeps the key as field 11 it does not know, reads every key it knows
/// as written, and writes back the bytes it read; the generated type decodes them equal to the
/// original.
/// Catches: a group's `failed_hosts` that an older coordinator or client refuses, drops or
/// changes, or that makes it lose a key it knows.
#[test]
fn the_reader_of_the_release_before_keeps_failed_hosts_and_writes_it_back() {
    let before = before_pool();
    let original = policy();
    let bytes = original.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&before, "hostkeeper.operator.v1.RolloutPolicy"),
        bytes.as_slice(),
    )
    .expect("decodes");
    assert_eq!(unknown_at(&read, &[]), [11]);
    assert_eq!(
        read.get_field_by_name("bake_beats").as_deref(),
        Some(&Value::U32(3))
    );
    assert_eq!(
        read.get_field_by_name("live_rollouts").as_deref(),
        Some(&Value::U32(3))
    );
    assert_eq!(read.encode_to_vec(), bytes);
    assert_eq!(
        op::RolloutPolicy::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        original
    );

    let group = op::HostGroup {
        name: "web".to_owned(),
        packages: vec!["hostkeeper".to_owned()],
        policy: Some(original),
    };
    let answer = op::GetHostGroupResponse {
        host_group: Some(op::HostGroupState {
            group: Some(group.clone()),
            generation: 5,
            hosts: 2,
            ..op::HostGroupState::default()
        }),
        node_ids: vec!["n1".to_owned()],
        next_page_token: String::new(),
    };
    let bytes = group.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&before, "hostkeeper.operator.v1.HostGroup"),
        bytes.as_slice(),
    )
    .expect("decodes");
    assert_eq!(unknown_at(&read, &[]), Vec::<u32>::new());
    assert_eq!(unknown_at(&read, &["policy"]), [11]);
    assert_eq!(read.encode_to_vec(), bytes);

    let bytes = answer.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&before, "hostkeeper.operator.v1.GetHostGroupResponse"),
        bytes.as_slice(),
    )
    .expect("decodes");
    assert_eq!(unknown_at(&read, &["host_group", "group", "policy"]), [11]);
    assert_eq!(read.encode_to_vec(), bytes);
    assert_eq!(
        op::GetHostGroupResponse::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        answer
    );
}

/// Proves: a reader of the release before reads an alert of kind 24 as the number it does not
/// name, its other fields as written, and writes back the bytes it read.
/// Catches: an alert of a repeated deferral that a client of the release before refuses, reads
/// as another kind, or writes back changed.
#[test]
fn an_older_reader_keeps_a_repeated_deferral_alert_and_writes_it_back() {
    let alert = op::Alert {
        kind: op::AlertKind::RepeatedDeferral.into(),
        severity: op::AlertSeverity::Critical.into(),
        host_id: "m1".to_owned(),
        service: "js".to_owned(),
        version: "2.0".to_owned(),
        message: "m1 declined js 2.0 in host group mac 3 times in a row".to_owned(),
        first_observed_unix: 1_800_000_000,
        reason: "host_setup_missing".to_owned(),
        tries: 3,
        ..op::Alert::default()
    };
    let bytes = alert.encode_to_vec();
    let read = DynamicMessage::decode(
        message(&before_pool(), "hostkeeper.operator.v1.Alert"),
        bytes.as_slice(),
    )
    .expect("the release before reads it");
    assert_eq!(
        read.get_field_by_name("kind").as_deref(),
        Some(&Value::EnumNumber(24))
    );
    assert_eq!(
        read.get_field_by_name("reason").as_deref(),
        Some(&Value::String("host_setup_missing".to_owned()))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Proves: this build reads a policy the release before wrote with `failed_hosts` unspecified
/// (0, which the coordinator reads as `continue`) and every key that release wrote as written.
/// Catches: a policy of the release before read as naming `stop`, or with a key lost.
#[test]
fn this_build_reads_a_policy_of_the_release_before_with_failed_hosts_unspecified() {
    let before = message(&before_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let mut written = DynamicMessage::new(before);
    written.set_field_by_name("min_up", Value::U32(2));
    written.set_field_by_name("bake_beats", Value::U32(3));
    let read = op::RolloutPolicy::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.failed_hosts, i32::from(op::FailedHosts::Unspecified));
    assert_eq!(
        read,
        op::RolloutPolicy {
            bake_beats: 3,
            min_up: 2,
            ..op::RolloutPolicy::default()
        }
    );
}

/// Proves: `RolloutPolicy.failed_hosts` is field 11, a plain (not `optional`, not repeated) field
/// of the enum `hostkeeper.operator.v1.FailedHosts`, named `failedHosts` in JSON; the enum has
/// exactly UNSPECIFIED 0, STOP 1 and CONTINUE 2; `ALERT_KIND_REPEATED_DEFERRAL` is 24.
/// Catches: a field or value given another number, label or type than the schema says, which a
/// peer built apart would read as something else.
#[test]
fn the_new_field_and_values_have_their_numbers_and_types() {
    let now = current_pool();
    let policy = message(&now, "hostkeeper.operator.v1.RolloutPolicy");
    let field = policy
        .get_field_by_name("failed_hosts")
        .expect("failed_hosts");
    let kind = match field.kind() {
        Kind::Enum(e) => e.full_name().to_owned(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        (
            field.number(),
            kind.as_str(),
            field.json_name(),
            field.is_list(),
            field.supports_presence()
        ),
        (
            11,
            "hostkeeper.operator.v1.FailedHosts",
            "failedHosts",
            false,
            false
        )
    );
    let modes = now
        .get_enum_by_name("hostkeeper.operator.v1.FailedHosts")
        .expect("FailedHosts");
    assert_eq!(
        modes
            .values()
            .map(|v| (v.name().to_owned(), v.number()))
            .collect::<Vec<_>>(),
        vec![
            ("FAILED_HOSTS_UNSPECIFIED".to_owned(), 0),
            ("FAILED_HOSTS_STOP".to_owned(), 1),
            ("FAILED_HOSTS_CONTINUE".to_owned(), 2),
        ]
    );
    assert_eq!(
        (
            op::FailedHosts::Unspecified as i32,
            op::FailedHosts::Stop as i32,
            op::FailedHosts::Continue as i32
        ),
        (0, 1, 2)
    );
    let kinds = now
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("AlertKind");
    assert_eq!(
        kinds
            .get_value_by_name("ALERT_KIND_REPEATED_DEFERRAL")
            .expect("value")
            .number(),
        24
    );
    assert_eq!(op::AlertKind::RepeatedDeferral as i32, 24);
}

/// Proves: this build's JSON names the key `failedHosts` with the mode by its enum name, reads it
/// back to the same message, and the release before's JSON reader refuses a body naming it, by
/// name.
/// Catches: the release before's gateway taking a body naming `failed_hosts` and dropping it,
/// which would store a group whose failed hosts go on when it was put to stop them; a JSON key
/// named otherwise than the wire's.
#[test]
fn the_json_of_failed_hosts_is_named_by_this_build_and_refused_by_the_release_before() {
    let only = op::RolloutPolicy {
        failed_hosts: op::FailedHosts::Stop.into(),
        ..op::RolloutPolicy::default()
    };
    let current = message(&current_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let mut written = DynamicMessage::new(current.clone());
    written.transcode_from(&only).expect("this build writes it");
    let json = serde_json::to_value(&written).expect("JSON");
    assert_eq!(
        json,
        serde_json::json!({ "failedHosts": "FAILED_HOSTS_STOP" })
    );
    let read = DynamicMessage::deserialize(current, json.clone())
        .expect("this build reads it")
        .transcode_to::<op::RolloutPolicy>()
        .expect("a policy");
    assert_eq!(read, only);

    let before = message(&before_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let refused =
        DynamicMessage::deserialize_with_options(before, json, &DeserializeOptions::new())
            .expect_err("the release before refuses an unknown key");
    assert!(refused.to_string().contains("failedHosts"), "{refused}");
}
