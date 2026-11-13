//! `Services.Put` and `Services.Remove`, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the two methods, their four messages and
//! the `ServicesRefusal` value `IN_USE` (16). A reader of the release before keeps every method and
//! value it knows; this build reads and writes the new messages field for field.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

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

/// The names of the methods of `Services` in `pool`, in order.
fn methods(pool: &DescriptorPool) -> Vec<String> {
    pool.get_service_by_name("hostkeeper.operator.v1.Services")
        .expect("the service")
        .methods()
        .map(|m| m.name().to_owned())
        .collect()
}

/// Proves: `Put` and `Remove` are methods of `Services` that the release before lacks, and every
/// method of that release is still there in the same relative order.
/// Catches: a shipped method dropped, renamed or reordered, or the new ones already known to the
/// release before.
#[test]
fn put_and_remove_are_methods_the_release_before_does_not_have() {
    let older = methods(&baseline_pool());
    let now = methods(&current_pool());
    for name in ["Put", "Remove"] {
        assert!(!older.contains(&name.to_owned()), "{name} is new");
        assert!(now.contains(&name.to_owned()), "{name} is served");
    }
    let kept: Vec<_> = now.iter().filter(|m| older.contains(*m)).cloned().collect();
    assert_eq!(kept, older);
}

/// Proves: `SERVICES_REFUSAL_IN_USE` is number 16, unknown to the release before, and every value
/// of the release before keeps its name and number.
/// Catches: a value added at a number the release before uses, or a shipped value renumbered.
#[test]
fn the_new_refusal_is_unknown_to_the_release_before() {
    let name = "hostkeeper.operator.v1.ServicesRefusal";
    assert_eq!(op::ServicesRefusal::InUse as i32, 16);
    assert_eq!(
        op::ServicesRefusal::InUse.as_str_name(),
        "SERVICES_REFUSAL_IN_USE"
    );
    let older = baseline_pool();
    let before = older.get_enum_by_name(name).expect("shipped");
    assert!(before.get_value(16).is_none());
    let pool = current_pool();
    let current = pool.get_enum_by_name(name).expect("served");
    assert_eq!(
        current.get_value(16).expect("new").name(),
        "SERVICES_REFUSAL_IN_USE"
    );
    for kept in before.values() {
        assert_eq!(
            current.get_value(kept.number()).expect("kept").name(),
            kept.name()
        );
    }
}

/// Proves: the requests keep their field numbers (service 1, repository 2, reason 3 for `Put`;
/// service 1, reason 2 for `Remove`) and a response carries the audit entry at field 1, which the
/// release before reads as the `AuditEntry` of `SetMinVersionResponse`.
/// Catches: a field renumbered, or the entry moved off field 1.
#[test]
fn the_messages_keep_their_field_numbers_and_the_entry_reads_as_an_audit_entry() {
    let pool = current_pool();
    let put = op::PutServiceRequest {
        service: "tierstore".to_owned(),
        repository: "komira".to_owned(),
        reason: "register".to_owned(),
    };
    let read = DynamicMessage::decode(
        pool.get_message_by_name("hostkeeper.operator.v1.PutServiceRequest")
            .expect("served"),
        put.encode_to_vec().as_slice(),
    )
    .expect("decodes");
    let strings: Vec<(u32, String)> = read
        .fields()
        .map(|(f, v)| (f.number(), v.as_str().expect("a string").to_owned()))
        .collect();
    assert_eq!(
        strings,
        vec![
            (1, "tierstore".to_owned()),
            (2, "komira".to_owned()),
            (3, "register".to_owned())
        ]
    );
    let remove = op::RemoveServiceRequest {
        service: "tierstore".to_owned(),
        reason: "gone".to_owned(),
    };
    let read = DynamicMessage::decode(
        pool.get_message_by_name("hostkeeper.operator.v1.RemoveServiceRequest")
            .expect("served"),
        remove.encode_to_vec().as_slice(),
    )
    .expect("decodes");
    let strings: Vec<(u32, String)> = read
        .fields()
        .map(|(f, v)| (f.number(), v.as_str().expect("a string").to_owned()))
        .collect();
    assert_eq!(
        strings,
        vec![(1, "tierstore".to_owned()), (2, "gone".to_owned())]
    );

    let entry = Some(op::AuditEntry {
        seq: 3,
        action: "service_put".to_owned(),
        ..Default::default()
    });
    let bytes = op::PutServiceResponse {
        entry: entry.clone(),
    }
    .encode_to_vec();
    assert_eq!(bytes, op::RemoveServiceResponse { entry }.encode_to_vec());
    let older = baseline_pool();
    let shipped = older
        .get_message_by_name("hostkeeper.operator.v1.SetMinVersionResponse")
        .expect("shipped answer of the same shape");
    let read = DynamicMessage::decode(shipped, bytes.as_slice()).expect("decodes");
    assert!(read.get_field_by_number(1).is_some());
}
