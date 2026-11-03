//! What the encoded messages do across versions, with the generated types: a reader of the
//! release before reads what it knows of a newer message and keeps the rest, a newer reader
//! reads an older message, an unknown enum number reads as UNSPECIFIED and is kept, and an
//! `optional` field tells absent from zero. The release before is this crate's descriptor set
//! with the newest fields taken out, read with `prost-reflect`.
//!
//! Also the closed refusal vocabularies, whose tokens are the wire of every refusal.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1::{
    Health, HeartbeatRequest, Outcome, PinReport, Platform, ServiceBeat, UpdateResult,
};
use hostkeeper_proto::hostkeeper::local::v1::{
    ActivateResponse, Pin, RecordProblem, ServiceRecord, StatusResponse, StepOutcome, StepRecord,
};

/// This crate's descriptors, as a reader of the release before would have them: `UpdateResult`
/// without `started_unix` and `order_try`.
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
    result
        .field
        .retain(|f| !matches!(f.name(), "started_unix" | "order_try"));
    // Their synthetic oneofs go with them; `UpdateResult` has no other.
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

/// Catches: a new field that an older reader cannot skip, or loses when it writes back what it
/// read: the older reader reads every field it knows, and the bytes it writes back still carry
/// the new ones.
#[test]
fn an_older_reader_reads_what_it_knows_and_keeps_the_rest() {
    let newer = failed(Some(1_800_000_000), Some(2));
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
/// its target, unhealthy, reported by the agent and recorded by the root part.
#[test]
fn a_forced_update_that_failed_on_its_target_is_reported() {
    let beat = HeartbeatRequest {
        platform: Platform::LinuxX8664.into(),
        services: vec![ServiceBeat {
            name: "db".to_owned(),
            version: "1.5".to_owned(),
            health: Health::Unhealthy.into(),
            last_update: Some(failed(Some(1_800_000_000), Some(3))),
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

/// Catches: a status that cannot say a step record is unreadable, and an activation that cannot
/// say where the pin it kept now holds the service.
#[test]
fn status_and_activate_carry_a_record_problem_and_the_moved_pin() {
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

    let activated = ActivateResponse {
        record: Some(StepRecord {
            version: "1.4".to_owned(),
            forced: true,
            outcome: StepOutcome::Succeeded.into(),
            ..StepRecord::default()
        }),
        pin: Some(Pin {
            version: Some("1.4".to_owned()),
            reason: "back to the release before".to_owned(),
            ..Pin::default()
        }),
    };
    let read = ActivateResponse::decode(activated.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read.pin.and_then(|p| p.version),
        read.record.map(|r| r.version)
    );
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

/// Catches: a refusal token added, renamed or dropped without this list changing with it; the
/// tokens are the wire of every refusal, and callers branch on them.
#[test]
fn the_refusal_vocabularies_are_closed() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
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
            ],
        ),
        (
            "hostkeeper.local.v1.RootRefusal",
            "ROOT_REFUSAL_",
            &[
                "unknown_order",
                "invalid_request",
                "not_declared",
                "already_installed",
                "release_unverified",
                "declaration_mismatch",
                "pinned",
                "busy",
                "stale_try",
                "journal_unavailable",
                "outcome_unrecorded",
            ],
        ),
        (
            "hostkeeper.local.v1.AdminRefusal",
            "ADMIN_REFUSAL_",
            &[
                "invalid_request",
                "not_declared",
                "not_kept",
                "unknown_declaration",
                "busy",
                "store_failed",
                "not_installed",
            ],
        ),
    ];
    for (name, prefix, want) in cases {
        assert_eq!(tokens(&pool, name, prefix), want, "{name}");
    }
}
