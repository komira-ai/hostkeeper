//! What the encoded messages do across versions, with the generated types: a reader of the
//! release before reads what it knows of a newer message and keeps the rest, a newer reader
//! reads an older message, an unknown enum number reads as UNSPECIFIED and is kept, and an
//! `optional` field tells absent from zero. The release before is this crate's descriptor set
//! with the newest fields taken out, read with `prost-reflect`.
//!
//! Also the names on the wire, pinned: the feature strings, the order's verbs and their numbers,
//! every refusal token, and the kbf JSON keys and the fields that carry them.

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

/// Catches: a name on the wire renamed or renumbered without this test changing with it: the
/// features daemons announce, the order's verbs, every refusal token, and the kbf JSON keys and
/// the fields that carry them. Callers branch on each.
#[test]
fn wire_names_are_pinned() {
    assert_eq!(
        feature_names(),
        ["deferred-v1", "forced-v1", "halt-v1", "heartbeat-v2"]
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

    let cases: [(&str, &str, &[&str]); 5] = [
        (
            "hostkeeper.agent.v1.CoordinatorRefusal",
            "COORDINATOR_REFUSAL_",
            &["forbidden", "invalid_heartbeat"],
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
    ];
    for (name, prefix, want) in cases {
        assert_eq!(tokens(&pool, name, prefix), want, "{name}");
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
