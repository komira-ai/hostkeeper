//! `Enrollment.UpdateRecord`, added after the last release: a new method with its own two
//! messages, and no field of a shipped message changed. A reader of the release before reads
//! every record an update answers with, the release before has no such method (a new console's
//! edit against an old coordinator is refused as unimplemented, nothing else changes), and the
//! numbers of the new messages are pinned.

use prost::Message as _;
use prost_reflect::DescriptorPool;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// Proves: the release before has neither `UpdateRecord` nor its messages, so the method is an
/// addition and replaces nothing.
#[test]
fn the_release_before_has_no_update_record() {
    let older = baseline_pool();
    for name in ["UpdateRecordRequest", "UpdateRecordResponse"] {
        assert!(
            older
                .get_message_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name}"
        );
    }
    if let Some(service) = older.get_service_by_name("hostkeeper.operator.v1.Enrollment") {
        assert!(service.methods().all(|m| m.name() != "UpdateRecord"));
    }
}

/// Proves: the new messages keep the numbers the proto gives them, and a request and a response
/// read back what was written.
#[test]
fn the_update_messages_round_trip_with_their_numbers_pinned() {
    let request = op::UpdateRecordRequest {
        host_id: "db-1".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        notes: "rack 4\nslot 2".to_owned(),
    };
    let bytes = request.encode_to_vec();
    assert_eq!(bytes[0], 0x0a, "host_id is field 1, a string");
    assert_eq!(
        op::UpdateRecordRequest::decode(bytes.as_slice()).expect("decodes"),
        request
    );
    let response = op::UpdateRecordResponse {
        record: Some(op::InventoryRecord {
            host_id: "db-1".to_owned(),
            notes: "rack 4\nslot 2".to_owned(),
            ..op::InventoryRecord::default()
        }),
        entry: Some(op::AuditEntry {
            seq: 3,
            action: "record_update".to_owned(),
            ..op::AuditEntry::default()
        }),
    };
    let bytes = response.encode_to_vec();
    assert_eq!(bytes[0], 0x0a, "record is field 1, a message");
    assert_eq!(
        op::UpdateRecordResponse::decode(bytes.as_slice()).expect("decodes"),
        response
    );
}
