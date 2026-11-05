//! The messages added since the committed baseline (`proto/compat/baseline.binpb`), with the
//! generated types: each reads back as it was written, and a reader built from the baseline, the
//! release before, keeps what it does not know and writes it back unchanged.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, EnrollRequest, EnrollResponse, EnrollState, PollRequest, PollResponse, PollState,
    RenewRequest, RenewResponse,
};
use hostkeeper_proto::hostkeeper::local::v1::{Lifecycle, ServiceRecord, StepRecord};
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

fn audit_entry(action: &str) -> op::AuditEntry {
    op::AuditEntry {
        seq: 41,
        by: op::Actor::Operator.into(),
        action: action.to_owned(),
        service: Some("db".to_owned()),
        node_id: Some("db-1".to_owned()),
        reason: Some("disk swap".to_owned()),
    }
}

fn claims() -> Claims {
    Claims {
        machine_id: "0f".repeat(16),
        dmi_serial: "SN-1".to_owned(),
        product_uuid: "product-uuid-1".to_owned(),
        addresses: vec!["192.0.2.10".to_owned(), "198.51.100.7".to_owned()],
        mac_serial: "C02X".to_owned(),
        platform_uuid: "platform-uuid-2".to_owned(),
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
    }
}

fn record() -> op::InventoryRecord {
    op::InventoryRecord {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        serial: "SN-1".to_owned(),
        notes: "rack 4, slot 2".to_owned(),
        added_by: "alice".to_owned(),
        added_unix: 1_800_000_000,
    }
}

/// One message of each type added since the baseline, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    let request_id = "ab".repeat(32);
    vec![
        (
            "hostkeeper.agent.v1.EnrollRequest",
            round_trip(&EnrollRequest {
                csr_der: vec![0x30, 0x82, 1, 2],
                claims: Some(claims()),
                image: "boot-1".to_owned(),
                renewal_of: "db-1".to_owned(),
                mac_nonce: vec![7; 32],
            }),
        ),
        ("hostkeeper.agent.v1.Claims", round_trip(&claims())),
        (
            "hostkeeper.agent.v1.EnrollResponse",
            round_trip(&EnrollResponse {
                request_id: request_id.clone(),
                state: EnrollState::DuplicateClaim.into(),
            }),
        ),
        (
            "hostkeeper.agent.v1.PollRequest",
            round_trip(&PollRequest {
                request_id: request_id.clone(),
                nonce: vec![9; 16],
                signature: vec![0x30, 0x45, 3],
            }),
        ),
        (
            "hostkeeper.agent.v1.PollResponse",
            round_trip(&PollResponse {
                state: PollState::Issued.into(),
                certificate_der: vec![0x30, 0x82, 4],
                host_id: "db-1".to_owned(),
                ca_bundle_pem: "-----BEGIN CERTIFICATE-----\n".to_owned(),
                nonce: vec![5; 64],
            }),
        ),
        (
            "hostkeeper.agent.v1.RenewRequest",
            round_trip(&RenewRequest {
                csr_der: vec![0x30, 0x82, 5],
            }),
        ),
        (
            "hostkeeper.agent.v1.RenewResponse",
            round_trip(&RenewResponse {
                certificate_der: vec![0x30, 0x82, 6],
                ca_bundle_pem: "-----BEGIN CERTIFICATE-----\n".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.AbortRequest",
            round_trip(&op::AbortRequest {
                node_id: "db-1".to_owned(),
                service: "db".to_owned(),
                reason: "stuck in prepare".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.AbortResponse",
            round_trip(&op::AbortResponse {
                queued: true,
                entry: Some(audit_entry("abort")),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListAuditRequest",
            round_trip(&op::ListAuditRequest {
                since_seq: 40,
                limit: 256,
                actor: Some("operator".to_owned()),
                action: Some("abort".to_owned()),
                service: Some("db".to_owned()),
                node_id: Some("db-1".to_owned()),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListAuditResponse",
            round_trip(&op::ListAuditResponse {
                entries: vec![
                    audit_entry("exclude_abandon"),
                    audit_entry("enroll_approve"),
                ],
                next_seq: 41,
            }),
        ),
        (
            "hostkeeper.operator.v1.ListPendingRequest",
            round_trip(&op::ListPendingRequest {
                limit: 10,
                page_token: "p2".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListPendingResponse",
            round_trip(&op::ListPendingResponse {
                requests: vec![op::PendingEnrollment {
                    request_id: request_id.clone(),
                    key_sha256: request_id.clone(),
                    claims: Some(claims()),
                    image: "boot-1".to_owned(),
                    first_seen_unix: 1_800_000_000,
                    renewal_of: "db-1".to_owned(),
                    duplicate_claim: true,
                    mac_nonce_presented: true,
                    inventory_match: op::InventoryMatch::Ambiguous.into(),
                    record_host_id: "db-1".to_owned(),
                    record_role: "storage".to_owned(),
                    candidate_host_ids: vec!["db-1".to_owned(), "db-2".to_owned()],
                }],
                next_page_token: "p3".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ApproveRequest",
            round_trip(&op::ApproveRequest {
                request_id: request_id.clone(),
                key_sha256: request_id,
                host_id: "db-1".to_owned(),
                role: "storage".to_owned(),
                services: vec!["db".to_owned()],
                replace: true,
                reason: "new disk".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ApproveResponse",
            round_trip(&op::ApproveResponse {
                host_id: "db-1".to_owned(),
                certificate_issued_at_poll: true,
                entry: Some(audit_entry("enroll_approve")),
            }),
        ),
        (
            "hostkeeper.operator.v1.SkippedHost",
            round_trip(&op::SkippedHost {
                node_id: "db-2".to_owned(),
                reason: "daemon_too_old".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.InventoryRecord",
            round_trip(&record()),
        ),
        (
            "hostkeeper.operator.v1.AddRecordRequest",
            round_trip(&op::AddRecordRequest {
                host_id: "db-1".to_owned(),
                role: "storage".to_owned(),
                data_macs: vec!["0C-C4-7A-00-00-02".to_owned()],
                bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
                serial: "SN-1".to_owned(),
                notes: "rack 4".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.AddRecordResponse",
            round_trip(&op::AddRecordResponse {
                record: Some(record()),
                entry: Some(audit_entry("record_add")),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListRecordsRequest",
            round_trip(&op::ListRecordsRequest {
                limit: 10,
                page_token: "db-1".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListRecordsResponse",
            round_trip(&op::ListRecordsResponse {
                records: vec![record()],
                next_page_token: "db-1".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveRecordRequest",
            round_trip(&op::RemoveRecordRequest {
                host_id: "db-1".to_owned(),
                reason: "returned to vendor".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveRecordResponse",
            round_trip(&op::RemoveRecordResponse {
                entry: Some(audit_entry("record_remove")),
            }),
        ),
    ]
}

/// Catches: a message added since the baseline that does not read back as written, and a new
/// field or message that a reader built from the baseline drops: a new field of a released
/// message (`ExcludeRequest.abandon`, `ForceResponse.skipped`) is kept by the older reader of
/// that message, which still reads what it knows; a whole new message is kept, byte for byte, by
/// an older reader that knows none of its fields (a gateway or proxy of the release before).
#[test]
fn the_new_messages_round_trip_and_an_older_reader_keeps_unknown_fields() {
    let older = baseline_pool();

    let exclude = op::ExcludeRequest {
        service: "db".to_owned(),
        node_id: "db-1".to_owned(),
        reason: "host is gone".to_owned(),
        abandon: true,
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ExcludeRequest")
        .expect("released");
    assert!(message.get_field_by_name("abandon").is_none());
    let read = DynamicMessage::decode(message, round_trip(&exclude).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("reason").as_deref(),
        Some(&Value::String("host is gone".to_owned()))
    );
    let back = op::ExcludeRequest::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, exclude);

    let force = op::ForceResponse {
        entries: vec![audit_entry("force")],
        skipped: vec![op::SkippedHost {
            node_id: "db-2".to_owned(),
            reason: "held".to_owned(),
        }],
        ..op::ForceResponse::default()
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ForceResponse")
        .expect("released");
    assert!(message.get_field_by_name("skipped").is_none());
    let read = DynamicMessage::decode(message, round_trip(&force).as_slice()).expect("decodes");
    let entries = read.get_field_by_name("entries").expect("known");
    assert_eq!(entries.as_list().map(<[Value]>::len), Some(1));
    let back = op::ForceResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, force);

    // A released message with no field: every field of a new message is unknown to it.
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 23);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(!bytes.is_empty(), "{name} sets its fields");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
}

/// Catches a service's lifecycle that an admin client of the release before cannot read past, or
/// drops: a `ServiceRecord` that says `REMOVING` is read by the baseline's `ServiceRecord`, which
/// has no `lifecycle`, with every field it knows, and written back with the lifecycle kept; a
/// record of the release before reads with the lifecycle UNSPECIFIED.
#[test]
fn an_older_admin_reader_keeps_the_lifecycle_of_a_service() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.local.v1.ServiceRecord")
        .expect("released");
    assert!(message.get_field_by_name("lifecycle").is_none());
    let removing = ServiceRecord {
        service: "web".to_owned(),
        last: Some(StepRecord {
            step: "uninstall".to_owned(),
            reason: "unit_stuck".to_owned(),
            ..StepRecord::default()
        }),
        lifecycle: Lifecycle::Removing.into(),
        ..ServiceRecord::default()
    };
    let read = DynamicMessage::decode(message, round_trip(&removing).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("web".to_owned()))
    );
    let back = ServiceRecord::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, removing);
    assert_eq!(back.lifecycle(), Lifecycle::Removing);

    let before = ServiceRecord {
        service: "web".to_owned(),
        ..ServiceRecord::default()
    };
    let read = ServiceRecord::decode(round_trip(&before).as_slice()).expect("decodes");
    assert_eq!(read.lifecycle(), Lifecycle::Unspecified);
}
