//! `Services.ListTrustedPublishers`, `Services.PutTrustedPublisher` and
//! `Services.RemoveTrustedPublisher`, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the three methods, their messages and the
//! `ServicesRefusal` value `PUBLISHER_UNKNOWN` (17). A reader of the release before keeps every
//! method and value it knows; this build reads and writes the new messages field for field.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

fn methods(pool: &DescriptorPool) -> Vec<String> {
    pool.get_service_by_name("hostkeeper.operator.v1.Services")
        .expect("the service")
        .methods()
        .map(|m| m.name().to_owned())
        .collect()
}

/// Proves: the three calls are methods of `Services` the release before lacks, and every method of
/// that release is still there in the same relative order.
/// Catches: a shipped method dropped, renamed or reordered, or a new one already known before.
#[test]
fn the_three_calls_are_methods_the_release_before_does_not_have() {
    let older = methods(&baseline_pool());
    let now = methods(&current_pool());
    for name in [
        "ListTrustedPublishers",
        "PutTrustedPublisher",
        "RemoveTrustedPublisher",
    ] {
        assert!(!older.contains(&name.to_owned()), "{name} is new");
        assert!(now.contains(&name.to_owned()), "{name} is served");
    }
    let kept: Vec<_> = now.iter().filter(|m| older.contains(*m)).cloned().collect();
    assert_eq!(kept, older);
}

/// Proves: `SERVICES_REFUSAL_PUBLISHER_UNKNOWN` is number 17, unknown to the release before, and
/// every value of the release before keeps its name and number.
/// Catches: a value added at a number the release before uses, or a shipped value renumbered.
#[test]
fn the_new_refusal_is_unknown_to_the_release_before() {
    let name = "hostkeeper.operator.v1.ServicesRefusal";
    assert_eq!(op::ServicesRefusal::PublisherUnknown as i32, 17);
    let older = baseline_pool();
    let before = older.get_enum_by_name(name).expect("shipped");
    assert!(before.get_value(17).is_none());
    let pool = current_pool();
    let current = pool.get_enum_by_name(name).expect("served");
    assert_eq!(
        current.get_value(17).expect("new").name(),
        "SERVICES_REFUSAL_PUBLISHER_UNKNOWN"
    );
    for kept in before.values() {
        assert_eq!(
            current.get_value(kept.number()).expect("kept").name(),
            kept.name()
        );
    }
}

/// Proves: an entry keeps its field numbers (repository 1, workflow 2, ref 3, environment 4,
/// repository id 5, called workflows 6, the host group limit 7 and its groups 8) and a response
/// carries the audit entry at field 1, which the release before reads as the `AuditEntry` of
/// `SetMinVersionResponse`.
/// Catches: a field renumbered, or the entry moved off field 1.
#[test]
fn the_messages_keep_their_field_numbers_and_the_entry_reads_as_an_audit_entry() {
    let pool = current_pool();
    let publisher = op::TrustedPublisher {
        repository: "o/r".to_owned(),
        workflow: ".github/workflows/a.yml".to_owned(),
        r#ref: "refs/heads/main".to_owned(),
        environment: "prod".to_owned(),
        repository_id: "7".to_owned(),
        called_workflows: vec!["w@1".to_owned()],
        limited_to_host_groups: true,
        host_groups: vec!["lab".to_owned()],
    };
    let read = DynamicMessage::decode(
        pool.get_message_by_name("hostkeeper.operator.v1.TrustedPublisher")
            .expect("served"),
        publisher.encode_to_vec().as_slice(),
    )
    .expect("decodes");
    let numbers: Vec<u32> = read.fields().map(|(f, _)| f.number()).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7, 8]);

    let put = op::PutTrustedPublisherRequest {
        service: "s".to_owned(),
        repository: "o/r".to_owned(),
        workflow: "w".to_owned(),
        r#ref: "main".to_owned(),
        environment: "e".to_owned(),
        reason: "why".to_owned(),
    };
    let read = DynamicMessage::decode(
        pool.get_message_by_name("hostkeeper.operator.v1.PutTrustedPublisherRequest")
            .expect("served"),
        put.encode_to_vec().as_slice(),
    )
    .expect("decodes");
    let strings: Vec<(u32, String)> = read
        .fields()
        .map(|(f, v)| (f.number(), v.as_str().expect("a string").to_owned()))
        .collect();
    assert_eq!(strings.len(), 6);
    assert_eq!(strings[0], (1, "s".to_owned()));
    assert_eq!(strings[5], (6, "why".to_owned()));

    let entry = Some(op::AuditEntry {
        seq: 3,
        action: "trusted_publisher_put".to_owned(),
        ..Default::default()
    });
    let bytes = op::PutTrustedPublisherResponse {
        entry,
        publishers: vec![publisher],
    }
    .encode_to_vec();
    let shipped = baseline_pool()
        .get_message_by_name("hostkeeper.operator.v1.SetMinVersionResponse")
        .expect("shipped answer of the same shape");
    let read = DynamicMessage::decode(shipped, bytes.as_slice()).expect("decodes");
    assert!(read.get_field_by_number(1).is_some());
}
