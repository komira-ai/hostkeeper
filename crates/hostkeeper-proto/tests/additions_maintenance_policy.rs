//! What the maintenance policy adds to the wire (`docs/operations/hostgroups.md`, Maintenance):
//! `RolloutPolicy.maintenance` (10), a new `MaintenancePolicy` message of eight keys (the optional
//! ones, `xcode_default`, `batch` and `min_serving`, told apart from a key set to zero or empty),
//! and the new `LinuxScope` enum.
//!
//! The committed baseline (`proto/compat/baseline.binpb`) predates `hostgroups.proto` entirely, so
//! the release before's reader is this build's descriptor set with field 10 of `RolloutPolicy`, the
//! `MaintenancePolicy` message and the `LinuxScope` enum taken out. Rule B8 in both directions:
//!
//! - that reader reads a policy, a group and an answer carrying a maintenance table, keeps the
//!   table as a field it does not know and writes it back byte for byte; its JSON reader refuses
//!   the key by name, so a coordinator before this change refuses such a body loudly;
//! - this build reads what that release wrote with no table, and a table with no key set with
//!   every optional key absent.

use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, Kind, MessageDescriptor, Value,
};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// The descriptor set of the release before: this build's, with field 10 of `RolloutPolicy`, the
/// message `MaintenancePolicy` and the enum `LinuxScope` of `hostgroups.proto` taken out, every
/// other message as it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let mut fields = 0;
    let mut messages = 0;
    let mut enums = 0;
    for file in &mut set.file {
        if file.name() != "hostkeeper/operator/v1/hostgroups.proto" {
            continue;
        }
        for message in &mut file.message_type {
            if message.name() == "RolloutPolicy" {
                let before = message.field.len();
                message.field.retain(|f| f.number() != 10);
                fields += before - message.field.len();
            }
        }
        let before = file.message_type.len();
        file.message_type
            .retain(|m| m.name() != "MaintenancePolicy");
        messages += before - file.message_type.len();
        let before = file.enum_type.len();
        file.enum_type.retain(|e| e.name() != "LinuxScope");
        enums += before - file.enum_type.len();
    }
    assert_eq!(
        (fields, messages, enums),
        (1, 1, 1),
        "this build has exactly one field, one message and one enum past the release before"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// A maintenance table with every key set to a value other than its default.
fn full_table() -> op::MaintenancePolicy {
    op::MaintenancePolicy {
        xcode_keep: 2,
        xcode_default: Some("26.1.1".to_owned()),
        macos_major: true,
        linux_scope: op::LinuxScope::All.into(),
        batch: Some(7),
        min_serving: Some(0),
        drain_deadline_secs: 900,
        return_secs: 120,
    }
}

/// A policy with keys of the release before set and the full maintenance table.
fn policy() -> op::RolloutPolicy {
    op::RolloutPolicy {
        bake_beats: 3,
        min_up: 2,
        live_rollouts: Some(3),
        maintenance: Some(full_table()),
        ..op::RolloutPolicy::default()
    }
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

/// What a field's kind is called here: the scalar's proto name, or the full name of its enum.
fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Uint32 => "uint32".to_owned(),
        Kind::Uint64 => "uint64".to_owned(),
        Kind::String => "string".to_owned(),
        Kind::Bool => "bool".to_owned(),
        Kind::Enum(e) => e.full_name().to_owned(),
        other => panic!("{other:?}"),
    }
}

/// Proves: `RolloutPolicy.maintenance` is field 10, a `MaintenancePolicy` message, absent from the
/// release before; the message has exactly its eight keys with their numbers, names, JSON names and
/// kinds, and `xcode_default`, `batch` and `min_serving` are `optional`; `LinuxScope` has exactly
/// its three values; the baseline holds none of it.
/// Catches: a key missing, renumbered, retyped, not `optional` (an absent batch could not be told
/// from 0), named otherwise on the wire or in JSON, or present in the release before; a value
/// added to or renumbered in the enum.
#[test]
fn maintenance_is_field_10_of_the_rollout_policy_and_its_keys_and_scopes_are_pinned() {
    let current = current_pool();
    let policy = message(&current, "hostkeeper.operator.v1.RolloutPolicy");
    let field = policy.get_field(10).expect("field 10");
    assert_eq!(field.name(), "maintenance");
    assert_eq!(field.json_name(), "maintenance");
    assert!(!field.is_list() && !field.is_map());
    assert_eq!(
        kind_name_of_message(&field.kind()),
        "hostkeeper.operator.v1.MaintenancePolicy"
    );

    let table = message(&current, "hostkeeper.operator.v1.MaintenancePolicy");
    let keys: Vec<(u32, String, String, String, bool)> = table
        .fields()
        .map(|f| {
            (
                f.number(),
                f.name().to_owned(),
                f.json_name().to_owned(),
                kind_name(&f.kind()),
                f.field_descriptor_proto().proto3_optional(),
            )
        })
        .collect();
    let key = |n: u32, name: &str, json: &str, kind: &str, optional: bool| {
        (
            n,
            name.to_owned(),
            json.to_owned(),
            kind.to_owned(),
            optional,
        )
    };
    assert_eq!(
        keys,
        [
            key(1, "xcode_keep", "xcodeKeep", "uint32", false),
            key(2, "xcode_default", "xcodeDefault", "string", true),
            key(3, "macos_major", "macosMajor", "bool", false),
            key(
                4,
                "linux_scope",
                "linuxScope",
                "hostkeeper.operator.v1.LinuxScope",
                false
            ),
            key(5, "batch", "batch", "uint32", true),
            key(6, "min_serving", "minServing", "uint32", true),
            key(
                7,
                "drain_deadline_secs",
                "drainDeadlineSecs",
                "uint64",
                false
            ),
            key(8, "return_secs", "returnSecs", "uint64", false),
        ]
    );

    let scope = current
        .get_enum_by_name("hostkeeper.operator.v1.LinuxScope")
        .expect("served");
    let values: Vec<(i32, String)> = scope
        .values()
        .map(|v| (v.number(), v.name().to_owned()))
        .collect();
    assert_eq!(
        values,
        [
            (0, "LINUX_SCOPE_UNSPECIFIED".to_owned()),
            (1, "LINUX_SCOPE_SECURITY".to_owned()),
            (2, "LINUX_SCOPE_ALL".to_owned()),
        ]
    );

    let before = before_pool();
    let older = message(&before, "hostkeeper.operator.v1.RolloutPolicy");
    assert!(older.get_field(10).is_none());
    assert!(older.get_field_by_name("maintenance").is_none());
    assert!(
        before
            .get_message_by_name("hostkeeper.operator.v1.MaintenancePolicy")
            .is_none()
    );
    assert!(
        before
            .get_enum_by_name("hostkeeper.operator.v1.LinuxScope")
            .is_none()
    );
    assert!(
        baseline_pool()
            .get_message_by_name("hostkeeper.operator.v1.RolloutPolicy")
            .is_none(),
        "the baseline predates hostgroups.proto, so it holds no RolloutPolicy"
    );
}

/// The full name of the message a field's kind names.
fn kind_name_of_message(kind: &Kind) -> String {
    match kind {
        Kind::Message(m) => m.full_name().to_owned(),
        other => panic!("{other:?}"),
    }
}

/// Proves: the table is written as field 10 (tag `0x52`), a table with no key set is still written
/// (present, empty), and `xcode_default` named empty is written apart from one not named (tag
/// `0x12`, length 0).
/// Catches: the number of the table or of `xcode_default` drifting, read off the wire rather than
/// the descriptor; a present empty table or key lost on the way.
#[test]
fn the_tags_of_the_maintenance_table_are_pinned_on_the_wire() {
    let empty_table = op::RolloutPolicy {
        maintenance: Some(op::MaintenancePolicy::default()),
        ..op::RolloutPolicy::default()
    };
    assert_eq!(empty_table.encode_to_vec(), [0x52, 0]);
    assert_eq!(
        op::RolloutPolicy::decode(empty_table.encode_to_vec().as_slice()).expect("decodes"),
        empty_table
    );
    let named_empty = op::MaintenancePolicy {
        xcode_default: Some(String::new()),
        ..op::MaintenancePolicy::default()
    };
    assert_eq!(named_empty.encode_to_vec(), [0x12, 0]);
    assert_eq!(
        op::MaintenancePolicy::decode(named_empty.encode_to_vec().as_slice())
            .expect("decodes")
            .xcode_default,
        Some(String::new())
    );
}

/// Proves: the release before's reader reads a policy, a group and an answer carrying a full
/// maintenance table: every key it knows is as written, exactly field 10 of the policy is kept as a
/// field it does not know, and the bytes it writes back are the bytes it read; the generated type
/// decodes them equal to the original.
/// Catches: a table that the release before refuses, drops or changes (an older coordinator
/// storing a group put by a newer `hk`, an older client of `Get` and `Put`), or that makes it
/// lose a key it knows.
#[test]
fn the_reader_of_the_release_before_keeps_the_maintenance_table_and_writes_it_back() {
    let before = before_pool();
    let original = policy();
    let bytes = original.encode_to_vec();
    let descriptor = message(&before, "hostkeeper.operator.v1.RolloutPolicy");
    let read = DynamicMessage::decode(descriptor, bytes.as_slice()).expect("decodes");
    assert_eq!(unknown_at(&read, &[]), [10]);
    assert_eq!(
        read.get_field_by_name("bake_beats").as_deref(),
        Some(&Value::U32(3))
    );
    assert_eq!(
        read.get_field_by_name("min_up").as_deref(),
        Some(&Value::U32(2))
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
        policy: Some(original.clone()),
    };
    let bytes = group.encode_to_vec();
    let descriptor = message(&before, "hostkeeper.operator.v1.HostGroup");
    let read = DynamicMessage::decode(descriptor, bytes.as_slice()).expect("decodes");
    assert_eq!(unknown_at(&read, &[]), Vec::<u32>::new());
    assert_eq!(unknown_at(&read, &["policy"]), [10]);
    assert_eq!(
        read.get_field_by_name("name").as_deref(),
        Some(&Value::String("web".to_owned()))
    );
    assert_eq!(read.encode_to_vec(), bytes);
    assert_eq!(
        op::HostGroup::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        group
    );

    let answer = op::GetHostGroupResponse {
        host_group: Some(op::HostGroupState {
            group: Some(group),
            generation: 5,
            hosts: 2,
            ..op::HostGroupState::default()
        }),
        node_ids: vec!["n1".to_owned()],
        next_page_token: String::new(),
    };
    let bytes = answer.encode_to_vec();
    let descriptor = message(&before, "hostkeeper.operator.v1.GetHostGroupResponse");
    let read = DynamicMessage::decode(descriptor, bytes.as_slice()).expect("decodes");
    assert_eq!(unknown_at(&read, &["host_group", "group", "policy"]), [10]);
    assert_eq!(read.encode_to_vec(), bytes);
    assert_eq!(
        op::GetHostGroupResponse::decode(read.encode_to_vec().as_slice()).expect("decodes"),
        answer
    );
}

/// Proves: this build reads a policy the release before wrote as one with no table (`None`, not an
/// empty table), keeps every key that release wrote, and reads a table with no key set with
/// `xcode_default`, `batch` and `min_serving` absent (not empty or 0) and the scope 0.
/// Catches: a policy of the release before read as naming a table, which would make every group
/// put before this change a group with a maintenance policy; an absent optional key read as a
/// value.
#[test]
fn this_build_reads_a_policy_of_the_release_before_with_no_table_and_a_bare_table_with_no_key() {
    let before = message(&before_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let mut written = DynamicMessage::new(before);
    written.set_field_by_name("min_up", Value::U32(2));
    written.set_field_by_name("bake_beats", Value::U32(3));
    let read = op::RolloutPolicy::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.maintenance, None);
    assert_eq!(
        read,
        op::RolloutPolicy {
            bake_beats: 3,
            min_up: 2,
            ..op::RolloutPolicy::default()
        }
    );

    let bare = op::MaintenancePolicy::decode(&[][..]).expect("decodes");
    assert_eq!(bare.xcode_default, None);
    assert_eq!(bare.batch, None);
    assert_eq!(bare.min_serving, None);
    assert_eq!(bare.linux_scope, 0);
    assert_eq!(bare, op::MaintenancePolicy::default());
}

/// Proves: this build's JSON names the table `maintenance` with camelCase keys, `uint64` keys as
/// strings, the scope by its enum name and an optional key set to 0 still written; it reads the
/// same JSON back to the same message; and the release before's JSON reader refuses a body naming
/// `maintenance`, by name.
/// Catches: the release before's gateway taking a body with a table and dropping it, which would
/// store a group with none (a coordinator before this change must answer `invalid_request`); a
/// JSON key named otherwise than the wire's.
#[test]
fn the_json_of_the_table_is_named_by_this_build_and_refused_by_the_release_before() {
    let current = message(&current_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let mut written = DynamicMessage::new(current.clone());
    written
        .transcode_from(&policy_with_table_only())
        .expect("this build writes it");
    let json = serde_json::to_value(&written).expect("JSON");
    assert_eq!(
        json,
        serde_json::json!({
            "maintenance": {
                "xcodeKeep": 2,
                "xcodeDefault": "26.1.1",
                "macosMajor": true,
                "linuxScope": "LINUX_SCOPE_ALL",
                "batch": 7,
                "minServing": 0,
                "drainDeadlineSecs": "900",
                "returnSecs": "120",
            }
        })
    );
    let read = DynamicMessage::deserialize(current, json.clone())
        .expect("this build reads it")
        .transcode_to::<op::RolloutPolicy>()
        .expect("a policy");
    assert_eq!(read, policy_with_table_only());

    let before = message(&before_pool(), "hostkeeper.operator.v1.RolloutPolicy");
    let refused =
        DynamicMessage::deserialize_with_options(before, json, &DeserializeOptions::new())
            .expect_err("the release before refuses an unknown key");
    assert!(refused.to_string().contains("maintenance"), "{refused}");
}

/// A policy with the full table and no other key set.
fn policy_with_table_only() -> op::RolloutPolicy {
    op::RolloutPolicy {
        maintenance: Some(full_table()),
        ..op::RolloutPolicy::default()
    }
}
