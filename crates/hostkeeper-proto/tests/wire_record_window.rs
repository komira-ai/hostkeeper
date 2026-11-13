//! The names on the wire of the enrollment service's inventory records, pinned: the RPCs of
//! `Enrollment`, its refusal tokens, the states of a record, and the fields of the messages that
//! carry a record or its approval window by name and number. The rest of the pinned names are in
//! `wire.rs`, `wire_mdm.rs` and `wire_plan.rs`.

use prost_reflect::DescriptorPool;

/// The tokens of an enum: each value's name without the enum's prefix, in lower case.
fn tokens(pool: &DescriptorPool, name: &str, prefix: &str) -> Vec<String> {
    pool.get_enum_by_name(name)
        .expect("the enum")
        .values()
        .filter(|v| v.number() != 0)
        .map(|v| {
            v.name()
                .strip_prefix(prefix)
                .expect("the prefix")
                .to_ascii_lowercase()
        })
        .collect()
}

/// Each RPC of a service, as `Name(Request) Response`, in the file's order.
fn rpcs(pool: &DescriptorPool, service: &str) -> Vec<String> {
    pool.get_service_by_name(service)
        .expect("the service")
        .methods()
        .map(|m| format!("{}({}) {}", m.name(), m.input().name(), m.output().name()))
        .collect()
}

/// Each field of a message, as its name and number, in the file's order.
fn fields_of(pool: &DescriptorPool, message: &str) -> Vec<(String, u32)> {
    pool.get_message_by_name(message)
        .expect("the message")
        .fields()
        .map(|f| (f.name().to_owned(), f.number()))
        .collect()
}

/// The RPCs of the `Enrollment` service, pinned.
const RPCS: [(&str, &[&str]); 1] = [(
    "hostkeeper.operator.v1.Enrollment",
    &[
        "List(ListPendingRequest) ListPendingResponse",
        "Approve(ApproveRequest) ApproveResponse",
        "Reject(RejectRequest) RejectResponse",
        "AddRecord(AddRecordRequest) AddRecordResponse",
        "ListRecords(ListRecordsRequest) ListRecordsResponse",
        "RemoveRecord(RemoveRecordRequest) RemoveRecordResponse",
        "UpdateRecord(UpdateRecordRequest) UpdateRecordResponse",
        "OpenRecord(OpenRecordRequest) OpenRecordResponse",
    ],
)];

/// The fields of the messages that carry a record or its window, pinned by name and number.
const FIELDS: [(&str, &[(&str, u32)]); 5] = [
    (
        "hostkeeper.operator.v1.PendingEnrollment",
        &[
            ("request_id", 1),
            ("key_sha256", 2),
            ("claims", 3),
            ("image", 4),
            ("first_seen_unix", 5),
            ("renewal_of", 6),
            ("duplicate_claim", 7),
            ("mac_nonce_presented", 8),
            ("inventory_match", 9),
            ("record_host_id", 10),
            ("record_role", 11),
            ("candidate_host_ids", 12),
            ("state", 13),
            ("unenrolled_reason", 14),
            ("unenrolled_unix", 15),
            ("request_ttl_secs", 16),
            ("contested_by", 17),
            ("record_serial", 18),
            ("record_bmc_mac", 19),
            ("record_state", 20),
            ("record_approve_until_unix", 21),
            ("record_host_group", 22),
        ],
    ),
    (
        "hostkeeper.operator.v1.InventoryRecord",
        &[
            ("host_id", 1),
            ("role", 2),
            ("data_macs", 3),
            ("bmc_mac", 4),
            ("serial", 5),
            ("notes", 6),
            ("added_by", 7),
            ("added_unix", 8),
            ("enrolled", 9),
            ("host_held", 10),
            ("approve_until_unix", 11),
            ("state", 12),
            ("used_by_request_id", 13),
            ("used_unix", 14),
            ("platform", 15),
            ("host_group", 16),
        ],
    ),
    (
        "hostkeeper.operator.v1.AddRecordRequest",
        &[
            ("host_id", 1),
            ("role", 2),
            ("data_macs", 3),
            ("bmc_mac", 4),
            ("serial", 5),
            ("notes", 6),
            ("approve_until_unix", 7),
            ("approve_for_secs", 8),
            ("platform", 9),
            ("host_group", 10),
        ],
    ),
    (
        "hostkeeper.operator.v1.OpenRecordRequest",
        &[
            ("host_id", 1),
            ("approve_until_unix", 2),
            ("approve_for_secs", 3),
            ("reason", 4),
        ],
    ),
    (
        "hostkeeper.operator.v1.OpenRecordResponse",
        &[("record", 1), ("entry", 2)],
    ),
];

/// The enrollment refusal tokens and the states of a record, pinned.
const CASES: [(&str, &str, &[&str]); 2] = [
    (
        "hostkeeper.operator.v1.EnrollmentRefusal",
        "ENROLLMENT_REFUSAL_",
        &[
            "unauthorized",
            "forbidden",
            "unknown_request",
            "key_mismatch",
            "host_id_taken",
            "pending_full",
            "duplicate_claim",
            "invalid_host_id",
            "invalid_role",
            "invalid_service",
            "invalid_request",
            "not_pending",
            "denied",
            "store_failed",
            "record_exists",
            "record_conflict",
            "no_record",
            "capacity",
            "audit_unavailable",
            "record_mismatch",
            "record_contested",
            "invalid_window",
            "record_used",
            "services_retired",
            "host_group_differs",
            "unknown_host_group",
        ],
    ),
    (
        "hostkeeper.operator.v1.RecordState",
        "RECORD_STATE_",
        &["open", "closed", "used"],
    ),
];

/// Catches: a name on the wire of the inventory records renamed or renumbered without this test
/// changing with it: the `Enrollment` RPCs including `OpenRecord`, every enrollment refusal
/// token including `invalid_window` and `record_used`, the record states, and the fields of the
/// pending request, the record, and the add and open requests.
#[test]
fn the_record_names_are_pinned() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    for (name, prefix, want) in CASES {
        assert_eq!(tokens(&pool, name, prefix), want, "{name}");
    }
    for (service, want) in RPCS {
        assert_eq!(rpcs(&pool, service), want, "{service}");
    }
    for (message, want) in FIELDS {
        let want: Vec<(String, u32)> = want.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect();
        assert_eq!(fields_of(&pool, message), want, "{message}");
    }
}
