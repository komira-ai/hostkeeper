//! `Services.SetRepository` (docs/design/hostgroups.md section 9), new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the method, its two messages and the
//! `ServicesRefusal` value `REPOSITORY_UNKNOWN` (15). A reader of the release before keeps every
//! method and value it knows; this build reads and writes the new messages field for field.

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

/// Each method of `Services` in `pool`, in order, with its request and response.
fn services_methods(pool: &DescriptorPool) -> Vec<(String, String, String)> {
    pool.get_service_by_name("hostkeeper.operator.v1.Services")
        .expect("the service")
        .methods()
        .map(|m| {
            (
                m.name().to_owned(),
                m.input().name().to_owned(),
                m.output().name().to_owned(),
            )
        })
        .collect()
}

/// Proves: `SetRepository` is a method of `Services` that the release before lacks, every method
/// of that release is still there in the same order with the same messages, and neither of the
/// two messages exists in the release before (so no shipped message was re-meant).
/// Catches: the method renamed or retyped, a shipped method dropped, or a message reused.
#[test]
fn set_repository_is_a_method_the_release_before_does_not_have() {
    let older = services_methods(&baseline_pool());
    let now = services_methods(&current_pool());
    assert!(!older.iter().any(|(name, _, _)| name == "SetRepository"));
    let want = (
        "SetRepository".to_owned(),
        "SetRepositoryRequest".to_owned(),
        "SetRepositoryResponse".to_owned(),
    );
    assert!(now.contains(&want));
    let kept: Vec<_> = now.iter().filter(|m| **m != want).cloned().collect();
    assert_eq!(kept, older);
    for message in ["SetRepositoryRequest", "SetRepositoryResponse"] {
        let name = format!("hostkeeper.operator.v1.{message}");
        assert!(baseline_pool().get_message_by_name(&name).is_none());
        assert!(current_pool().get_message_by_name(&name).is_some());
    }
}

/// Proves: `SERVICES_REFUSAL_REPOSITORY_UNKNOWN` is number 15, unknown to the release before,
/// the generated type agrees, and every value of the release before keeps its name and number.
/// Catches: a value added at a number the release before uses, or a shipped value renumbered.
#[test]
fn the_new_services_refusal_is_unknown_to_the_release_before() {
    let name = "hostkeeper.operator.v1.ServicesRefusal";
    assert_eq!(op::ServicesRefusal::RepositoryUnknown as i32, 15);
    assert_eq!(
        op::ServicesRefusal::RepositoryUnknown.as_str_name(),
        "SERVICES_REFUSAL_REPOSITORY_UNKNOWN"
    );
    let older = baseline_pool();
    let before = older.get_enum_by_name(name).expect("shipped");
    assert!(before.get_value(15).is_none());
    assert!(
        before
            .get_value_by_name("SERVICES_REFUSAL_REPOSITORY_UNKNOWN")
            .is_none()
    );
    let current_pool = current_pool();
    let current = current_pool.get_enum_by_name(name).expect("served");
    assert_eq!(
        current.get_value(15).expect("new").name(),
        "SERVICES_REFUSAL_REPOSITORY_UNKNOWN"
    );
    for kept in before.values() {
        let same = current.get_value(kept.number()).expect("kept");
        assert_eq!(same.name(), kept.name());
    }
}

/// Proves: the request's three fields keep their numbers (service 1, repository 2, reason 3) and
/// the response carries the audit entry at field 1, which the release before reads as an
/// `AuditEntry` (the message every `Services` answer carries), keeping its action and reason.
/// Catches: a field renumbered, or the entry moved off field 1.
#[test]
fn the_messages_keep_their_field_numbers_and_the_entry_reads_as_an_audit_entry() {
    let request = op::SetRepositoryRequest {
        service: "hello".to_owned(),
        repository: "private".to_owned(),
        reason: "moved".to_owned(),
    };
    let pool = current_pool();
    let descriptor = pool
        .get_message_by_name("hostkeeper.operator.v1.SetRepositoryRequest")
        .expect("served");
    let read =
        DynamicMessage::decode(descriptor, request.encode_to_vec().as_slice()).expect("decodes");
    let strings: Vec<(u32, String)> = read
        .fields()
        .map(|(f, v)| (f.number(), v.as_str().expect("a string").to_owned()))
        .collect();
    assert_eq!(
        strings,
        vec![
            (1, "hello".to_owned()),
            (2, "private".to_owned()),
            (3, "moved".to_owned()),
        ]
    );

    let response = op::SetRepositoryResponse {
        entry: Some(op::AuditEntry {
            seq: 7,
            action: "set_repository".to_owned(),
            reason: Some("repository private: moved".to_owned()),
            ..Default::default()
        }),
    };
    let bytes = response.encode_to_vec();
    assert_eq!(
        op::SetRepositoryResponse::decode(bytes.as_slice()).expect("decodes"),
        response
    );
    let older = baseline_pool();
    let entry = older
        .get_message_by_name("hostkeeper.operator.v1.SetMinVersionResponse")
        .expect("shipped answer of the same shape");
    let read = DynamicMessage::decode(entry, bytes.as_slice()).expect("decodes");
    let Some(Value::Message(entry)) = read
        .get_field_by_number(1)
        .map(std::borrow::Cow::into_owned)
    else {
        panic!("the entry is at field 1");
    };
    let action = entry.get_field_by_name("action").expect("action");
    assert_eq!(action.as_str(), Some("set_repository"));
}
