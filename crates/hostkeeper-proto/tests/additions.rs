//! The messages added since the committed baseline (`proto/compat/baseline.binpb`), with the
//! generated types: each reads back as it was written, a reader built from the baseline, the
//! release before, keeps what it does not know and writes it back unchanged, and what a writer of
//! the release before sends reads with every new field at its default.

use std::collections::BTreeSet;

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, EnrollRequest, EnrollResponse, EnrollState, PollRequest, PollResponse, PollState,
    RenewRequest, RenewResponse,
};
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
    }
}

/// A pending request with every field set; `first_seen_unix` is past what 32 bits hold.
fn pending() -> op::PendingEnrollment {
    op::PendingEnrollment {
        request_id: "ab".repeat(32),
        key_sha256: "cd".repeat(32),
        claims: Some(claims()),
        image: "boot-1".to_owned(),
        first_seen_unix: 5_000_000_000,
        renewal_of: "db-1".to_owned(),
        duplicate_claim: true,
        mac_nonce_presented: true,
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
                since_seq: 5_000_000_001,
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
                next_seq: 5_000_000_002,
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
                requests: vec![pending()],
                next_page_token: "p3".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PendingEnrollment",
            round_trip(&pending()),
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
    ]
}

/// Catches: a message added since the baseline that does not read back as written or is missing
/// from the inventory (which must equal the descriptor messages the baseline lacks), and a new
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

    // A held host is in `held` and, with reason `held`, in `skipped`.
    let force = op::ForceResponse {
        entries: vec![audit_entry("force")],
        held: vec!["db-2".to_owned()],
        targeted: 2,
        skipped: vec![
            op::SkippedHost {
                node_id: "db-2".to_owned(),
                reason: "held".to_owned(),
            },
            op::SkippedHost {
                node_id: "db-4".to_owned(),
                reason: "daemon_too_old".to_owned(),
            },
        ],
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
    // The inventory is every message the baseline does not have, each once.
    let current = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let added: BTreeSet<String> = current
        .all_messages()
        .map(|m| m.full_name().to_owned())
        .filter(|name| older.get_message_by_name(name).is_none())
        .collect();
    let messages = new_messages();
    let listed: BTreeSet<String> = messages.iter().map(|(n, _)| (*n).to_owned()).collect();
    assert_eq!((listed.len(), messages.len()), (17, 17));
    assert_eq!(listed, added);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(!bytes.is_empty(), "{name} sets its fields");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
}

/// Catches: a new field of a released message that an older writer's message does not read as its
/// default (`abandon` false, no `skipped`), or that costs the fields the older writer set.
#[test]
fn an_older_writers_exclude_and_force_read_with_no_abandon_and_no_skips() {
    let older = baseline_pool();
    let text = |s: &str| Value::String(s.to_owned());

    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ExcludeRequest")
        .expect("released");
    let mut exclude = DynamicMessage::new(message);
    exclude.set_field_by_name("service", text("db"));
    exclude.set_field_by_name("node_id", text("db-1"));
    exclude.set_field_by_name("reason", text("host is gone"));
    let read = op::ExcludeRequest::decode(exclude.encode_to_vec().as_slice()).expect("decodes");
    let want = op::ExcludeRequest {
        service: "db".to_owned(),
        node_id: "db-1".to_owned(),
        reason: "host is gone".to_owned(),
        abandon: false,
    };
    assert_eq!(read, want);

    let entry = older
        .get_message_by_name("hostkeeper.operator.v1.AuditEntry")
        .expect("released");
    let entry = DynamicMessage::decode(entry, audit_entry("force").encode_to_vec().as_slice())
        .expect("decodes");
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ForceResponse")
        .expect("released");
    let mut force = DynamicMessage::new(message);
    force.set_field_by_name("entries", Value::List(vec![Value::Message(entry)]));
    force.set_field_by_name("held", Value::List(vec![text("db-3")]));
    force.set_field_by_name("targeted", Value::U32(4));
    let read = op::ForceResponse::decode(force.encode_to_vec().as_slice()).expect("decodes");
    let want = op::ForceResponse {
        entries: vec![audit_entry("force")],
        held: vec!["db-3".to_owned()],
        targeted: 4,
        skipped: Vec::new(),
    };
    assert_eq!(read, want);
}

/// Catches: an audit filter that does not tell unset (no filter) from set to the empty string,
/// or loses either on the way: each of the four reads back as written in all three states, and
/// the three are three different messages on the wire.
#[test]
fn an_audit_filter_reads_back_unset_empty_or_set() {
    let mut seen = BTreeSet::new();
    for filter in [None, Some(String::new()), Some("db".to_owned())] {
        let request = op::ListAuditRequest {
            since_seq: 7,
            limit: 0,
            actor: filter.clone(),
            action: filter.clone(),
            service: filter.clone(),
            node_id: filter.clone(),
        };
        let bytes = round_trip(&request);
        let read = op::ListAuditRequest::decode(bytes.as_slice()).expect("decodes");
        assert_eq!((read.actor, read.action), (filter.clone(), filter.clone()));
        assert_eq!((read.service, read.node_id), (filter.clone(), filter));
        seen.insert(bytes);
    }
    assert_eq!(seen.len(), 3);
}
