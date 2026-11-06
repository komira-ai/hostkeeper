//! The messages added since the committed baseline (`proto/compat/baseline.binpb`), with the
//! generated types: each reads back as it was written, and a reader built from the baseline, the
//! release before, keeps what it does not know and writes it back unchanged.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, EnrollRequest, EnrollResponse, EnrollState, HeartbeatResponse, PollRequest,
    PollResponse, PollState, RenewRequest, RenewResponse, VerdictAck,
};
use hostkeeper_proto::hostkeeper::local::v1::{
    Enrollment, EnrollmentState, Lifecycle, LinkHealth, ServiceRecord, StatusResponse, StepRecord,
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
        at_unix: Some(1_800_000_000),
        by_name: Some("alice".to_owned()),
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
                by_name: Some("alice".to_owned()),
                newest: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.ListAuditResponse",
            round_trip(&op::ListAuditResponse {
                entries: vec![
                    audit_entry("exclude_abandon"),
                    audit_entry("enroll_approve"),
                    audit_entry("enroll_issued"),
                ],
                next_seq: 41,
            }),
        ),
        (
            "hostkeeper.operator.v1.ListPendingRequest",
            round_trip(&op::ListPendingRequest {
                limit: 10,
                page_token: "p2".to_owned(),
                unenrolled: true,
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
                    state: op::EnrollmentRequestState::Unenrolled.into(),
                    unenrolled_reason: "request_expired".to_owned(),
                    unenrolled_unix: 1_800_259_201,
                    request_ttl_secs: 259_200,
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
        ("hostkeeper.agent.v1.VerdictAck", round_trip(&verdict_ack())),
        ("hostkeeper.local.v1.Enrollment", round_trip(&enrolled())),
        ("hostkeeper.local.v1.LinkHealth", round_trip(&link())),
    ]
}

/// An enrolled host, every field set.
fn enrolled() -> Enrollment {
    Enrollment {
        state: EnrollmentState::Enrolled.into(),
        key_sha256: "ab".repeat(32),
        host_id: "web-17".to_owned(),
        not_after_unix: Some(1_900_604_800),
    }
}

fn link() -> LinkHealth {
    LinkHealth {
        last_success_unix: Some(1_900_000_015),
        consecutive_failures: 3,
    }
}

fn verdict_ack() -> VerdictAck {
    VerdictAck {
        service: "db".to_owned(),
        order_id: "force-1".to_owned(),
        order_try: 2,
    }
}

/// Catches: a message added since the baseline that does not read back as written, and a new
/// field or message that a reader built from the baseline drops: a new field of a released
/// message (`ExcludeRequest.abandon`, `ForceResponse.skipped`, `AuditEntry.at_unix` and
/// `by_name`) is kept by the older reader of that message, which still reads what it knows (`HeartbeatResponse.acks` too, and an answer of
/// the release before reads with no acks); a whole new message is kept, byte for byte, by
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

    // `AuditEntry.at_unix` and `by_name` are new fields of a released message: the older reader
    // of the entries above kept them, and an entry the release before wrote reads with neither.
    let entry = older
        .get_message_by_name("hostkeeper.operator.v1.AuditEntry")
        .expect("released");
    assert!(entry.get_field_by_name("at_unix").is_none());
    assert!(entry.get_field_by_name("by_name").is_none());
    let mut old_entry = DynamicMessage::new(entry);
    old_entry.set_field_by_name("seq", Value::U64(7));
    old_entry.set_field_by_name("action", Value::String("exclude".to_owned()));
    let read = op::AuditEntry::decode(old_entry.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!((read.seq, read.at_unix, read.by_name), (7, None, None));

    let acked = HeartbeatResponse {
        features: vec!["verdict-acks-v1".to_owned()],
        acks: vec![verdict_ack()],
        ..HeartbeatResponse::default()
    };
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatResponse")
        .expect("released");
    assert!(message.get_field_by_name("acks").is_none());
    let read = DynamicMessage::decode(message, round_trip(&acked).as_slice()).expect("decodes");
    let features = read.get_field_by_name("features").expect("known");
    assert_eq!(features.as_list().map(<[Value]>::len), Some(1));
    let back = HeartbeatResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, acked);
    let before = HeartbeatResponse::decode(
        HeartbeatResponse {
            features: vec!["halt-v1".to_owned()],
            ..HeartbeatResponse::default()
        }
        .encode_to_vec()
        .as_slice(),
    )
    .expect("decodes");
    assert!(before.acks.is_empty());

    // A released message with no field: every field of a new message is unknown to it.
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 26);
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

/// Catches the host's enrollment or link that an admin client of the release before cannot read
/// past, or drops: a `StatusResponse` with a service, an enrollment and a link is read by the
/// baseline's `StatusResponse`, which has neither, with its service, and written back with both
/// kept; a status of the release before reads with neither, and an `Enrollment` of an enrollment
/// state the reader does not know reads as UNSPECIFIED.
#[test]
fn an_older_admin_reader_keeps_the_enrollment_and_link_of_a_status() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.local.v1.StatusResponse")
        .expect("released");
    assert!(message.get_field_by_name("enrollment").is_none());
    assert!(message.get_field_by_name("link").is_none());
    let status = StatusResponse {
        services: vec![ServiceRecord {
            service: "web".to_owned(),
            ..ServiceRecord::default()
        }],
        enrollment: Some(enrolled()),
        link: Some(link()),
        ..StatusResponse::default()
    };
    let read = DynamicMessage::decode(message, round_trip(&status).as_slice()).expect("decodes");
    let services = read.get_field_by_name("services").expect("known");
    assert_eq!(services.as_list().map(<[Value]>::len), Some(1));
    let back = StatusResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, status);

    let before = StatusResponse {
        services: status.services.clone(),
        ..StatusResponse::default()
    };
    let read = StatusResponse::decode(round_trip(&before).as_slice()).expect("decodes");
    assert_eq!((read.enrollment, read.link), (None, None));

    let later = Enrollment {
        state: 99,
        ..Enrollment::default()
    };
    let read = Enrollment::decode(round_trip(&later).as_slice()).expect("decodes");
    assert_eq!(read.state(), EnrollmentState::Unspecified);
}

/// Catches the trusted root's age and reason in `Status` that an admin client of the release
/// before cannot read past, or drops, and a status of the release before read as a root of age
/// 0: a `StatusResponse` with both is read by the baseline's, which has neither, with its
/// services, and written back with both kept; one without them reads with no age and no reason.
#[test]
fn an_older_admin_reader_keeps_the_trusted_roots_age_and_reason() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.local.v1.StatusResponse")
        .expect("released");
    assert!(message.get_field_by_name("trusted_root_age_secs").is_none());
    assert!(message.get_field_by_name("trusted_root_reason").is_none());
    let stale = StatusResponse {
        services: vec![ServiceRecord {
            service: "web".to_owned(),
            ..ServiceRecord::default()
        }],
        trusted_root_age_secs: Some(91 * 86_400),
        trusted_root_reason: "trusted_root_stale".to_owned(),
        ..StatusResponse::default()
    };
    let read = DynamicMessage::decode(message, round_trip(&stale).as_slice()).expect("decodes");
    let services = read.get_field_by_name("services").expect("the services");
    assert!(matches!(services.as_ref(), Value::List(list) if list.len() == 1));
    let back = StatusResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, stale);

    let before = StatusResponse {
        services: stale.services.clone(),
        ..StatusResponse::default()
    };
    let read = StatusResponse::decode(round_trip(&before).as_slice()).expect("decodes");
    assert_eq!(
        (
            read.trusted_root_age_secs,
            read.trusted_root_reason.as_str()
        ),
        (None, "")
    );
}

/// Catches: a field added to a released message of `Services` that the release before drops or
/// cannot read past: an `Assign` or an `Unassign` of a role, a refusal token it does not know,
/// and a `List` entry with assignments, assigned hosts and the hosts ordered to uninstall are
/// read by the baseline's messages with every field they know, and written back with the new
/// ones kept.
#[test]
fn an_older_reader_keeps_the_assignments_of_services() {
    let older = baseline_pool();
    let assign = op::AssignRequest {
        node_id: String::new(),
        service: "db".to_owned(),
        role: "storage".to_owned(),
    };
    let unassign = op::UnassignRequest {
        node_id: String::new(),
        service: "db".to_owned(),
        uninstall: true,
        role: "storage".to_owned(),
    };
    let entry = op::ServiceEntry {
        service: "db".to_owned(),
        hosts: vec!["db-1".to_owned()],
        min_versions: vec![op::MinVersion {
            scope: Some(op::min_version::Scope::Role("storage".to_owned())),
            version: "1.5".to_owned(),
        }],
        holds: Vec::new(),
        desired_version: Some("2.0".to_owned()),
        roles: vec!["storage".to_owned()],
        assignments: vec![op::Assignment {
            scope: Some(op::assignment::Scope::Host("db-2".to_owned())),
            removed: true,
        }],
        assigned: vec![op::AssignedHost {
            node_id: "db-1".to_owned(),
            role: "storage".to_owned(),
            installed_version: Some("1.9".to_owned()),
            min_version: Some("1.5".to_owned()),
            holds: Vec::new(),
        }],
        uninstalling: vec!["db-2".to_owned()],
    };
    let cases: [(&str, &str, Vec<u8>); 3] = [
        (
            "hostkeeper.operator.v1.AssignRequest",
            "role",
            round_trip(&assign),
        ),
        (
            "hostkeeper.operator.v1.UnassignRequest",
            "role",
            round_trip(&unassign),
        ),
        (
            "hostkeeper.operator.v1.ServiceEntry",
            "assigned",
            round_trip(&entry),
        ),
    ];
    for (name, added, bytes) in cases {
        let message = older.get_message_by_name(name).expect("released");
        assert!(message.get_field_by_name(added).is_none(), "{name}");
        let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
        assert_eq!(
            read.get_field_by_name("service").as_deref(),
            Some(&Value::String("db".to_owned())),
            "{name}"
        );
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.ServicesRefusal")
        .expect("released");
    for added in [
        op::ServicesRefusal::UnknownRole,
        op::ServicesRefusal::BelowMinVersion,
    ] {
        assert!(refusal.get_value(added as i32).is_none(), "{added:?}");
    }
    for name in [
        "hostkeeper.operator.v1.Assignment",
        "hostkeeper.operator.v1.AssignedHost",
    ] {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
    }
}

/// Catches: a page of `Hosts.List` that the release before drops or cannot read past: a request
/// with a `limit` and a `page_token` is read by the baseline's `ListHostsRequest`, which has no
/// field, and written back with both kept; a reply with a `next_page_token` is read by the
/// baseline's `ListHostsResponse` with its hosts, and written back with the token kept; and the
/// request of the release before, empty, reads as the first page at the cap (`limit` 0, no
/// token), so an older client is answered as it was.
#[test]
fn an_older_reader_keeps_the_page_of_a_host_list() {
    let older = baseline_pool();
    let request = op::ListHostsRequest {
        limit: 2,
        page_token: "db-1".to_owned(),
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    let read = DynamicMessage::decode(message, round_trip(&request).as_slice()).expect("decodes");
    let back = op::ListHostsRequest::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, request);

    let reply = op::ListHostsResponse {
        hosts: vec![op::Host {
            node_id: "db-1".to_owned(),
            role: "storage".to_owned(),
            live: true,
            ..op::Host::default()
        }],
        next_page_token: "db-1".to_owned(),
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsResponse")
        .expect("released");
    assert!(message.get_field_by_name("next_page_token").is_none());
    let read = DynamicMessage::decode(message, round_trip(&reply).as_slice()).expect("decodes");
    let Some(Value::List(hosts)) = read.get_field_by_name("hosts").as_deref().cloned() else {
        panic!("the hosts are read");
    };
    assert_eq!(hosts.len(), 1);
    let back = op::ListHostsResponse::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, reply);

    let first = op::ListHostsRequest::decode(&[][..]).expect("decodes");
    assert_eq!(first, op::ListHostsRequest::default());
    assert_eq!((first.limit, first.page_token.as_str()), (0, ""));
}

/// Catches: a refusal of `Releases.SetDesired` for the release channel taking the number of a
/// value the release before already gave a meaning, or renumbering one: the baseline knows no
/// value 19 or 20, and every value it knows keeps its name and number. A caller of the release
/// before reads the new refusals by their token, the status message, which needs no value.
#[test]
fn the_release_channel_refusals_are_new_values_of_releases() {
    let older = baseline_pool();
    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.ReleasesRefusal")
        .expect("released");
    for (added, number) in [
        (op::ReleasesRefusal::NotInChannel, 19),
        (op::ReleasesRefusal::ChannelDigestMismatch, 20),
    ] {
        assert_eq!(added as i32, number, "{added:?}");
        assert!(refusal.get_value(number).is_none(), "{added:?}");
    }
    for value in refusal.values() {
        assert_eq!(
            op::ReleasesRefusal::try_from(value.number())
                .expect("a value of the release before")
                .as_str_name(),
            value.name()
        );
    }
}

/// Catches a release named by its index that a reader of the release before drops or cannot read
/// past: `SetDesiredRequest.release_index` and `release_index_digest`,
/// `DesiredState.release_index_digest`, and the `index_digest` of an update and a force are kept
/// by the baseline's readers, which still read every field they know; an update of the release
/// before reads with an empty `index_digest`, which the daemon does not check.
#[test]
fn a_release_index_and_its_digest_are_kept_by_an_older_reader() {
    use hostkeeper_proto::hostkeeper::agent::v1::{Force, Update};

    let older = baseline_pool();
    let index = r#"{"packages":[],"schema":1,"service":"db","version":"1.4"}"#;
    let set = op::SetDesiredRequest {
        service: "db".to_owned(),
        desired: Some(op::DesiredState {
            version: "1.4".to_owned(),
            release_index_digest: Some("cd".repeat(32)),
            ..op::DesiredState::default()
        }),
        release_index: Some(index.to_owned()),
        release_index_digest: Some("cd".repeat(32)),
        ..op::SetDesiredRequest::default()
    };
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.SetDesiredRequest")
        .expect("released");
    assert!(message.get_field_by_name("release_index").is_none());
    assert!(message.get_field_by_name("release_index_digest").is_none());
    let read = DynamicMessage::decode(message, round_trip(&set).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("db".to_owned()))
    );
    let back = op::SetDesiredRequest::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, set);

    let update = Update {
        service: "db".to_owned(),
        version: "1.4".to_owned(),
        package_digest: "ab".repeat(32),
        index_digest: "cd".repeat(32),
    };
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.Update")
        .expect("released");
    assert!(message.get_field_by_name("index_digest").is_none());
    let read = DynamicMessage::decode(message, round_trip(&update).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("package_digest").as_deref(),
        Some(&Value::String("ab".repeat(32)))
    );
    let back = Update::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, update);
    let before = Update::decode(
        Update {
            index_digest: String::new(),
            ..update.clone()
        }
        .encode_to_vec()
        .as_slice(),
    )
    .expect("decodes");
    assert_eq!(before.index_digest, "");

    let force = Force {
        service: "db".to_owned(),
        version: "1.4".to_owned(),
        package_digest: "ab".repeat(32),
        halt_generation: 3,
        index_digest: "cd".repeat(32),
    };
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.Force")
        .expect("released");
    assert!(message.get_field_by_name("index_digest").is_none());
    let read = DynamicMessage::decode(message, round_trip(&force).as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("halt_generation").as_deref(),
        Some(&Value::U64(3))
    );
    let back = Force::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, force);
}
