//! What the encoded messages do across versions, with the generated types: a reader of the
//! release before reads what it knows of a newer message and keeps the rest, a newer reader
//! reads an older message, an unknown enum number reads as UNSPECIFIED and is kept, and an
//! `optional` field tells absent from zero. The release before is this crate's descriptor set
//! with the newest fields taken out, read with `prost-reflect`.
//!
//! Also the names on the wire, pinned: the feature strings, the order's verbs and their numbers,
//! every refusal token, the RPCs of the coordinator's services, the fields of the enrollment,
//! abort, audit and force-report messages, and the kbf JSON keys and the fields that carry them.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1::{
    FailurePhase, Force, Health, HeartbeatRequest, HeartbeatResponse, Order, Outcome, PinReport,
    Platform, ServiceBeat, UpdateResult, order,
};
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortResponse, ClearRecordResponse, RecordProblem, ServiceRecord, StatusResponse, StepOutcome,
    StepRecord,
};

#[path = "support/kbf_tables.rs"]
mod kbf_tables;

/// The fields of `UpdateResult` that the release before did not have.
const NEWEST: [&str; 6] = [
    "started_unix",
    "order_try",
    "order_id",
    "forced",
    "detail",
    "log_tail",
];

/// This crate's descriptors, as a reader of the release before would have them: `UpdateResult`
/// without its newest fields.
fn older_pool() -> DescriptorPool {
    let mut set = prost_types::FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("the set decodes");
    let agent = set
        .file
        .iter_mut()
        .find(|f| f.package() == "hostkeeper.agent.v1")
        .expect("the agent file");
    let result = agent
        .message_type
        .iter_mut()
        .find(|m| m.name() == "UpdateResult")
        .expect("UpdateResult");
    result.field.retain(|f| !NEWEST.contains(&f.name()));
    // The synthetic oneofs of the optional ones go with them; `UpdateResult` has no other.
    result.oneof_decl.clear();
    DescriptorPool::from_file_descriptor_set(set).expect("the older set is valid")
}

fn failed(started: Option<u64>, order_try: Option<u64>) -> UpdateResult {
    UpdateResult {
        target: "1.5".to_owned(),
        outcome: Outcome::Failed.into(),
        reason: "verify".to_owned(),
        started_unix: started,
        order_try,
        ..UpdateResult::default()
    }
}

/// A verdict with every newest field set.
fn verdict_with_everything() -> UpdateResult {
    UpdateResult {
        order_id: Some("o-17".to_owned()),
        forced: true,
        reason: "start_failed".to_owned(),
        detail: Some("db.service did not become active".to_owned()),
        log_tail: vec!["starting".to_owned(), "exit 1".to_owned()],
        ..failed(Some(1_800_000_000), Some(2))
    }
}

/// Catches: a new field that an older reader cannot skip, or loses when it writes back what it
/// read: the older reader reads every field it knows, and the bytes it writes back still carry
/// the new ones.
#[test]
fn an_older_reader_reads_what_it_knows_and_keeps_the_rest() {
    let newer = verdict_with_everything();
    let older = older_pool()
        .get_message_by_name("hostkeeper.agent.v1.UpdateResult")
        .expect("the older message");
    assert!(older.get_field_by_name("order_try").is_none());

    let read = DynamicMessage::decode(older, newer.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("target").as_deref(),
        Some(&Value::String("1.5".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("outcome").as_deref(),
        Some(&Value::EnumNumber(Outcome::Failed.into()))
    );
    let written_back = UpdateResult::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(written_back, newer);
}

/// Catches: a newer reader that needs the new fields: a message from the release before reads,
/// with the new optional fields absent, not zero.
#[test]
fn a_newer_reader_reads_an_older_message() {
    let older = older_pool()
        .get_message_by_name("hostkeeper.agent.v1.UpdateResult")
        .expect("the older message");
    let mut message = DynamicMessage::new(older);
    message.set_field_by_name("target", Value::String("1.5".to_owned()));
    message.set_field_by_name("outcome", Value::EnumNumber(Outcome::Failed.into()));
    let read = UpdateResult::decode(message.encode_to_vec().as_slice()).expect("decodes");
    let mut want = failed(None, None);
    want.reason.clear();
    assert_eq!(read, want);
}

/// Catches: an enum value a reader does not know read as another value, or dropped: it reads as
/// UNSPECIFIED and is written back as the number it came with.
#[test]
fn an_unknown_enum_number_reads_as_unspecified_and_is_kept() {
    let beat = ServiceBeat {
        name: "db".to_owned(),
        version: "1.0".to_owned(),
        health: 99,
        ..ServiceBeat::default()
    };
    let read = ServiceBeat::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.health(), Health::Unspecified);
    assert_eq!(read.health, 99);
    assert_eq!(read.encode_to_vec(), beat.encode_to_vec());
}

/// Catches: an `optional` field whose zero reads as absent: a running update to an empty
/// version, a pin with an empty commit, and a start at time 0 each keep their presence.
#[test]
fn an_optional_field_tells_absent_from_zero() {
    for updating in [None, Some(String::new()), Some("1.6".to_owned())] {
        let beat = ServiceBeat {
            updating: updating.clone(),
            pinned: Some(PinReport { commit: None }),
            ..ServiceBeat::default()
        };
        let read = ServiceBeat::decode(beat.encode_to_vec().as_slice()).expect("decodes");
        assert_eq!(read.updating, updating);
        assert_eq!(read.pinned, Some(PinReport { commit: None }));
    }
    for started in [None, Some(0), Some(1_800_000_000)] {
        let read = UpdateResult::decode(failed(started, None).encode_to_vec().as_slice())
            .expect("decodes");
        assert_eq!(read.started_unix, started);
    }
    let empty = PinReport {
        commit: Some(String::new()),
    };
    assert_ne!(
        empty.encode_to_vec(),
        PinReport { commit: None }.encode_to_vec()
    );
}

/// Catches: two tries at one target that end the same way, in the same second, read as one
/// report: their try numbers tell them apart.
#[test]
fn two_tries_in_one_second_are_two_reports() {
    let first = failed(Some(1_800_000_000), Some(1));
    let second = failed(Some(1_800_000_000), Some(2));
    assert_ne!(first, second);
    assert_ne!(first.encode_to_vec(), second.encode_to_vec());
}

/// Catches: a shape the documented outcomes cannot say: a forced update that failed and stays on
/// its target, unhealthy, reported by the agent and recorded by the daemon.
#[test]
fn a_forced_update_that_failed_on_its_target_is_reported() {
    let beat = HeartbeatRequest {
        platform: Platform::LinuxX8664.into(),
        services: vec![ServiceBeat {
            name: "db".to_owned(),
            version: "1.5".to_owned(),
            health: Health::Unhealthy.into(),
            last_update: Some(UpdateResult {
                forced: true,
                reason: "start_failed".to_owned(),
                ..failed(Some(1_800_000_000), Some(3))
            }),
            ..ServiceBeat::default()
        }],
        ..HeartbeatRequest::default()
    };
    let read = HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    let service = &read.services[0];
    assert_eq!(
        service.version,
        service.last_update.as_ref().expect("a result").target
    );
    assert_eq!(service.health(), Health::Unhealthy);
    assert!(service.last_update.as_ref().is_some_and(|u| u.forced));

    let record = StepRecord {
        version: "1.5".to_owned(),
        forced: true,
        outcome: StepOutcome::Failed.into(),
        order_try: 3,
        ..StepRecord::default()
    };
    let read = StepRecord::decode(record.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.outcome(), StepOutcome::Failed);
    assert!(read.forced);
}

/// Catches: outcomes that can only say a forced update failed on its target: one that failed
/// before the switch leaves the service on the release before, reported as such.
#[test]
fn a_forced_update_that_failed_before_its_switch_is_reported() {
    let before_switch = UpdateResult {
        phase: FailurePhase::BeforeCommit.into(),
        forced: true,
        reason: "digest_mismatch".to_owned(),
        ..failed(Some(1_800_000_000), Some(4))
    };
    let beat = ServiceBeat {
        name: "db".to_owned(),
        version: "1.4".to_owned(),
        health: Health::Healthy.into(),
        last_update: Some(before_switch),
        ..ServiceBeat::default()
    };
    let read = ServiceBeat::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    let result = read.last_update.as_ref().expect("a result");
    assert_eq!(result.outcome(), Outcome::Failed);
    assert_eq!(result.phase(), FailurePhase::BeforeCommit);
    assert_ne!(read.version, result.target);

    let record = StepRecord {
        version: "1.5".to_owned(),
        forced: true,
        outcome: StepOutcome::Failed.into(),
        phase: FailurePhase::BeforeCommit.into(),
        order_try: 4,
        ..StepRecord::default()
    };
    let read = StepRecord::decode(record.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.phase(), FailurePhase::BeforeCommit);
    assert!(read.forced && !read.committed);
}

/// Catches: an Admin status that cannot say a step record is unreadable, an abort that cannot
/// return the record it closed, and a clear that cannot say what it cleared.
#[test]
fn the_admin_calls_carry_a_record_problem_and_the_closed_record() {
    let status = StatusResponse {
        services: vec![ServiceRecord {
            service: "db".to_owned(),
            record_problem: RecordProblem::Unreadable.into(),
            ..ServiceRecord::default()
        }],
        ..StatusResponse::default()
    };
    let read = StatusResponse::decode(status.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.services[0].record_problem(), RecordProblem::Unreadable);
    assert_eq!(read.services[0].current, None);

    let aborted = AbortResponse {
        record: Some(StepRecord {
            order_id: "o-17".to_owned(),
            order_try: 2,
            outcome: StepOutcome::Failed.into(),
            phase: FailurePhase::Halt.into(),
            ..StepRecord::default()
        }),
    };
    let read = AbortResponse::decode(aborted.encode_to_vec().as_slice()).expect("decodes");
    let record = read.record.expect("a record");
    assert_eq!(record.phase(), FailurePhase::Halt);
    assert_eq!((record.order_id.as_str(), record.order_try), ("o-17", 2));

    let cleared = ClearRecordResponse {
        problem: RecordProblem::NewerSchema.into(),
    };
    let read = ClearRecordResponse::decode(cleared.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.problem(), RecordProblem::NewerSchema);
}

/// Catches: an order that cannot say one verb with its typed arguments, or that reads back as
/// another: a force with its digest and halt generation, in a reply.
#[test]
fn an_order_is_one_verb_with_typed_arguments() {
    let reply = HeartbeatResponse {
        order: Some(Order {
            order_id: "o-18".to_owned(),
            r#try: 1,
            verb: Some(order::Verb::Force(Force {
                service: "db".to_owned(),
                version: "1.4".to_owned(),
                package_digest: "ab".repeat(32),
                halt_generation: 3,
            })),
        }),
        ..HeartbeatResponse::default()
    };
    let read = HeartbeatResponse::decode(reply.encode_to_vec().as_slice()).expect("decodes");
    let order = read.order.expect("an order");
    let Some(order::Verb::Force(force)) = order.verb else {
        panic!("not a force: {:?}", order.verb);
    };
    assert_eq!(force.package_digest.len(), 64);
    assert_eq!((force.halt_generation, order.r#try), (3, 1));
}

/// The tokens of a refusal enum: each value's name without the enum's prefix, in lower case.
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

/// The feature names `agent.proto` gives (`deferred-v1`), in the order it first names them.
fn feature_names() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("proto/hostkeeper/agent/v1/agent.proto");
    let text = std::fs::read_to_string(path).expect("read the file");
    let mut out: Vec<String> = Vec::new();
    for quoted in text.split('`').skip(1).step_by(2) {
        let feature = quoted.rsplit_once("-v").is_some_and(|(name, n)| {
            !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        });
        if feature && !out.iter().any(|f| f == quoted) {
            out.push(quoted.to_owned());
        }
    }
    out.sort();
    out
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

/// The RPCs of the coordinator's services, pinned.
const RPCS: [(&str, &[&str]); 6] = [
    (
        "hostkeeper.agent.v1.Coordinator",
        &[
            "Heartbeat(HeartbeatRequest) HeartbeatResponse",
            "Enroll(EnrollRequest) EnrollResponse",
            "Poll(PollRequest) PollResponse",
            "Renew(RenewRequest) RenewResponse",
        ],
    ),
    (
        "hostkeeper.operator.v1.Hosts",
        &[
            "List(ListHostsRequest) ListHostsResponse",
            "Get(GetHostRequest) GetHostResponse",
            "Exclude(ExcludeRequest) ExcludeResponse",
            "Include(IncludeRequest) IncludeResponse",
            "SetLabels(SetLabelsRequest) SetLabelsResponse",
            "Abort(AbortRequest) AbortResponse",
        ],
    ),
    (
        "hostkeeper.operator.v1.Releases",
        &[
            "SetDesired(SetDesiredRequest) SetDesiredResponse",
            "Converge(ConvergeRequest) ConvergeResponse",
            "Force(ForceRequest) ForceResponse",
            "UnmarkBad(UnmarkBadRequest) UnmarkBadResponse",
            "ClearHalt(ClearHaltRequest) ClearHaltResponse",
            "Status(StatusRequest) StatusResponse",
            "MarkBad(MarkBadRequest) MarkBadResponse",
            "Halt(HaltRequest) HaltResponse",
        ],
    ),
    (
        "hostkeeper.operator.v1.Services",
        &[
            "List(ListServicesRequest) ListServicesResponse",
            "Assign(AssignRequest) AssignResponse",
            "Unassign(UnassignRequest) UnassignResponse",
            "SetMinVersion(SetMinVersionRequest) SetMinVersionResponse",
            "Hold(HoldRequest) HoldResponse",
            "Unhold(UnholdRequest) UnholdResponse",
        ],
    ),
    (
        "hostkeeper.operator.v1.Enrollment",
        &[
            "List(ListPendingRequest) ListPendingResponse",
            "Approve(ApproveRequest) ApproveResponse",
            "AddRecord(AddRecordRequest) AddRecordResponse",
            "ListRecords(ListRecordsRequest) ListRecordsResponse",
            "RemoveRecord(RemoveRecordRequest) RemoveRecordResponse",
        ],
    ),
    (
        "hostkeeper.operator.v1.Audit",
        &["List(ListAuditRequest) ListAuditResponse"],
    ),
];

/// The fields of the enrollment, abort, audit and force-report messages, pinned by name and
/// number.
const FIELDS: [(&str, &[(&str, u32)]); 26] = [
    (
        "hostkeeper.agent.v1.HeartbeatResponse",
        &[
            ("order", 1),
            ("features", 2),
            ("halt_generations", 3),
            ("acks", 4),
        ],
    ),
    (
        "hostkeeper.agent.v1.VerdictAck",
        &[("service", 1), ("order_id", 2), ("order_try", 3)],
    ),
    (
        "hostkeeper.local.v1.StatusResponse",
        &[
            ("services", 1),
            ("trusted_root_age_secs", 2),
            ("trusted_root_reason", 3),
        ],
    ),
    (
        "hostkeeper.local.v1.ServiceRecord",
        &[
            ("service", 1),
            ("current", 2),
            ("last", 3),
            ("state", 4),
            ("record_problem", 5),
            ("lifecycle", 6),
        ],
    ),
    (
        "hostkeeper.agent.v1.EnrollRequest",
        &[
            ("csr_der", 1),
            ("claims", 2),
            ("image", 3),
            ("renewal_of", 4),
            ("mac_nonce", 5),
        ],
    ),
    (
        "hostkeeper.agent.v1.Claims",
        &[
            ("machine_id", 1),
            ("dmi_serial", 2),
            ("product_uuid", 3),
            ("addresses", 4),
            ("mac_serial", 5),
            ("platform_uuid", 6),
            ("bmc_mac", 7),
        ],
    ),
    (
        "hostkeeper.agent.v1.EnrollResponse",
        &[("request_id", 1), ("state", 2)],
    ),
    (
        "hostkeeper.agent.v1.PollRequest",
        &[("request_id", 1), ("nonce", 2), ("signature", 3)],
    ),
    (
        "hostkeeper.agent.v1.PollResponse",
        &[
            ("state", 1),
            ("certificate_der", 2),
            ("host_id", 3),
            ("ca_bundle_pem", 4),
            ("nonce", 5),
        ],
    ),
    ("hostkeeper.agent.v1.RenewRequest", &[("csr_der", 1)]),
    (
        "hostkeeper.agent.v1.RenewResponse",
        &[("certificate_der", 1), ("ca_bundle_pem", 2)],
    ),
    (
        "hostkeeper.operator.v1.ExcludeRequest",
        &[
            ("service", 1),
            ("node_id", 2),
            ("reason", 3),
            ("abandon", 4),
        ],
    ),
    (
        "hostkeeper.operator.v1.AbortRequest",
        &[("node_id", 1), ("service", 2), ("reason", 3)],
    ),
    (
        "hostkeeper.operator.v1.AbortResponse",
        &[("queued", 1), ("entry", 2)],
    ),
    (
        "hostkeeper.operator.v1.ListAuditRequest",
        &[
            ("since_seq", 1),
            ("limit", 2),
            ("actor", 3),
            ("action", 4),
            ("service", 5),
            ("node_id", 6),
        ],
    ),
    (
        "hostkeeper.operator.v1.ListAuditResponse",
        &[("entries", 1), ("next_seq", 2)],
    ),
    (
        "hostkeeper.operator.v1.ListPendingRequest",
        &[("limit", 1), ("page_token", 2)],
    ),
    (
        "hostkeeper.operator.v1.ListPendingResponse",
        &[("requests", 1), ("next_page_token", 2)],
    ),
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
        ],
    ),
    (
        "hostkeeper.operator.v1.RemoveRecordRequest",
        &[("host_id", 1), ("reason", 2)],
    ),
    (
        "hostkeeper.operator.v1.ApproveRequest",
        &[
            ("request_id", 1),
            ("key_sha256", 2),
            ("host_id", 3),
            ("role", 4),
            ("services", 5),
            ("replace", 6),
            ("reason", 7),
        ],
    ),
    (
        "hostkeeper.operator.v1.ApproveResponse",
        &[
            ("host_id", 1),
            ("certificate_issued_at_poll", 2),
            ("entry", 3),
        ],
    ),
    (
        "hostkeeper.operator.v1.ForceResponse",
        &[("entries", 1), ("held", 2), ("targeted", 3), ("skipped", 4)],
    ),
    (
        "hostkeeper.operator.v1.SkippedHost",
        &[("node_id", 1), ("reason", 2)],
    ),
];

/// Catches: a name on the wire renamed or renumbered without this test changing with it: the
/// features daemons announce, the order's verbs, every refusal token, the enrollment and poll
/// states, the coordinator's RPCs with their messages, the fields of the enrollment, abort,
/// audit and force-report messages, and the kbf JSON keys and the fields that carry them.
/// Callers branch on each, and the daemon and the coordinator are built against them apart.
#[test]
fn wire_names_are_pinned() {
    assert_eq!(
        feature_names(),
        [
            "abort-v1",
            "deferred-v1",
            "forced-v1",
            "halt-v1",
            "heartbeat-v2",
            "install-v1",
            "verdict-acks-v1"
        ]
    );

    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let order = pool
        .get_message_by_name("hostkeeper.agent.v1.Order")
        .expect("the order");
    let fields: Vec<(String, u32)> = order
        .fields()
        .map(|f| (f.name().to_owned(), f.number()))
        .collect();
    let want = [
        ("order_id", 1),
        ("try", 2),
        ("update", 3),
        ("install", 4),
        ("force_rollback", 5),
        ("force", 6),
        ("uninstall", 7),
        ("abort", 8),
        ("clear_halt", 9),
    ];
    assert_eq!(fields, want.map(|(name, number)| (name.to_owned(), number)));

    let cases: [(&str, &str, &[&str]); 11] = [
        (
            "hostkeeper.agent.v1.CoordinatorRefusal",
            "COORDINATOR_REFUSAL_",
            &[
                "forbidden",
                "invalid_heartbeat",
                "pending_full",
                "key_not_p256",
                "bad_csr",
                "denied",
                "unknown_request",
                "bad_signature",
                "renewal_not_allowed",
            ],
        ),
        (
            "hostkeeper.agent.v1.EnrollState",
            "ENROLL_STATE_",
            &["pending", "duplicate_claim"],
        ),
        (
            "hostkeeper.agent.v1.PollState",
            "POLL_STATE_",
            &["pending", "issued", "denied"],
        ),
        (
            "hostkeeper.operator.v1.HostsRefusal",
            "HOSTS_REFUSAL_",
            &[
                "unauthorized",
                "forbidden",
                "invalid_request",
                "unknown_service",
                "unknown_host",
                "capacity",
                "store_failed",
                "audit_unavailable",
            ],
        ),
        (
            "hostkeeper.operator.v1.ReleasesRefusal",
            "RELEASES_REFUSAL_",
            &[
                "unauthorized",
                "forbidden",
                "keys_unavailable",
                "invalid_request",
                "unknown_service",
                "generation_conflict",
                "held_bad",
                "below_min_version",
                "not_halted",
                "not_marked_bad",
                "store_failed",
                "no_digest",
                "self_service",
                "confirm_mismatch",
                "already_bad",
                "already_halted",
                "unknown_target",
                "audit_unavailable",
                "not_in_channel",
                "channel_digest_mismatch",
            ],
        ),
        (
            "hostkeeper.operator.v1.ServicesRefusal",
            "SERVICES_REFUSAL_",
            &[
                "unauthorized",
                "forbidden",
                "invalid_request",
                "unknown_service",
                "unknown_host",
                "not_assigned",
                "not_held",
                "store_failed",
                "unknown_role",
                "below_min_version",
                "audit_unavailable",
            ],
        ),
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
            ],
        ),
        (
            "hostkeeper.operator.v1.InventoryMatch",
            "INVENTORY_MATCH_",
            &["matched", "unmatched", "ambiguous"],
        ),
        (
            "hostkeeper.operator.v1.AuditRefusal",
            "AUDIT_REFUSAL_",
            &[
                "unauthorized",
                "forbidden",
                "invalid_request",
                "store_failed",
            ],
        ),
        (
            "hostkeeper.local.v1.AdminRefusal",
            "ADMIN_REFUSAL_",
            &[
                "invalid_request",
                "not_installed",
                "nothing_running",
                "record_reads",
                "store_failed",
            ],
        ),
        (
            "hostkeeper.local.v1.Lifecycle",
            "LIFECYCLE_",
            &["absent", "installing", "running", "removing"],
        ),
    ];
    for (name, prefix, want) in cases {
        assert_eq!(tokens(&pool, name, prefix), want, "{name}");
    }

    for (service, want) in RPCS {
        assert_eq!(rpcs(&pool, service), want, "{service}");
    }
    for (message, want) in FIELDS {
        let want: Vec<(String, u32)> = want.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect();
        assert_eq!(fields_of(&pool, message), want, "{message}");
    }

    assert_eq!(
        kbf_tables::SERVICE_FACTS,
        [
            ("version", "version"),
            ("health", "health"),
            ("last_update", "last_update"),
            ("last_update.target", "last_update.target"),
            ("last_update.outcome", "last_update.outcome"),
            ("last_update.phase", "last_update.phase"),
            ("last_update.class", "last_update.class"),
            ("last_update.reason", "last_update.reason"),
            ("last_update.started_unix", "last_update.started_unix"),
            ("updating", "updating"),
            ("step", "step"),
            ("step.name", "step.name"),
            ("step.attempt", "step.attempt"),
            ("commit", "commit"),
            ("step_secs", "step_secs"),
            ("promotion_pending", "promotion_pending"),
            ("deferred", "deferred"),
            ("deferred.target", "deferred.target"),
            ("deferred.reason", "deferred.reason"),
            ("deferred.retry_after_secs", "deferred.retry_after_secs"),
            ("pinned", "pinned"),
            ("pinned.commit", "pinned.commit"),
        ]
    );
    assert_eq!(
        kbf_tables::HEARTBEAT,
        [
            ("platform", "platform"),
            ("agent", "agent"),
            ("agent.name", "agent.name"),
            ("agent.version", "agent.version"),
            ("agent_features", "features"),
            ("services", "services"),
            ("services[].name", "services.name"),
        ]
    );
    assert_eq!(
        kbf_tables::REPLY,
        [
            ("reply", "order"),
            ("service", "order.update.service"),
            ("version", "order.update.version"),
            ("features", "features"),
            ("halts", "halt_generations"),
            ("forced", "order.force"),
        ]
    );
}
