//! `HostGroups` (docs/design/hostgroups.md sections 7.1, 7.2, 9.1, 12 to 14), new since the
//! committed baseline (`proto/compat/baseline.binpb`), the release before: a service of five
//! methods with its messages and its refusal, `HostGroupsRefusal`; refusal values added to
//! `ReleasesRefusal`, `ServicesRefusal`, `EnrollmentRefusal` and `RolloutsRefusal`, and
//! `WaitOutcome::NoDesired`; and a `host_group` (or `host_groups`, `facts_generation`, `facts`)
//! field added to a shipped message. A reader of the release before keeps every method, message,
//! value and field it knows, reads what this build writes with the new fields set, and writes
//! them back unchanged; this build reads what that release wrote with every new field at its
//! default. The explicit-presence fields tell zero from absent, and the JSON a `hkctl --request`
//! file carries for `HostGroups.Put` reads into the request.

use std::fmt::Debug;

use prost::Message as _;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
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

/// Each method of `service` in `pool`: its name, request and response.
fn methods(pool: &DescriptorPool, service: &str) -> Vec<(String, String, String)> {
    pool.get_service_by_name(service)
        .expect("the service")
        .methods()
        .map(|m| {
            (
                m.name().to_owned(),
                m.input().full_name().to_owned(),
                m.output().full_name().to_owned(),
            )
        })
        .collect()
}

/// Every value of the enum `name` in `pool`: its token name and number, in order.
fn values(pool: &DescriptorPool, name: &str) -> Vec<(String, i32)> {
    pool.get_enum_by_name(name)
        .expect("the enum")
        .values()
        .map(|v| (v.name().to_owned(), v.number()))
        .collect()
}

/// Catches: `HostGroups` being absent from this build, or one of its methods renamed, retyped
/// or moved, or the release before already having the service or any of its messages (the
/// addition would then re-mean them): the five methods in the design's order with their request
/// and response, and none of the service's messages or enums in the baseline.
#[test]
fn host_groups_is_a_service_the_release_before_does_not_have() {
    let service = "hostkeeper.operator.v1.HostGroups";
    assert!(baseline_pool().get_service_by_name(service).is_none());
    let want: Vec<(String, String, String)> = [
        ("List", "ListHostGroupsRequest", "ListHostGroupsResponse"),
        ("Get", "GetHostGroupRequest", "GetHostGroupResponse"),
        ("Put", "PutHostGroupRequest", "PutHostGroupResponse"),
        (
            "Remove",
            "RemoveHostGroupRequest",
            "RemoveHostGroupResponse",
        ),
        ("Move", "MoveHostRequest", "MoveHostResponse"),
    ]
    .iter()
    .map(|(m, i, o)| {
        (
            (*m).to_owned(),
            format!("hostkeeper.operator.v1.{i}"),
            format!("hostkeeper.operator.v1.{o}"),
        )
    })
    .collect();
    assert_eq!(methods(&current_pool(), service), want);

    let older = baseline_pool();
    for message in [
        "ListHostGroupsRequest",
        "ListHostGroupsResponse",
        "GetHostGroupRequest",
        "GetHostGroupResponse",
        "PutHostGroupRequest",
        "PutHostGroupResponse",
        "RemoveHostGroupRequest",
        "RemoveHostGroupResponse",
        "MoveHostRequest",
        "MoveHostResponse",
        "HostGroup",
        "RolloutPolicy",
        "PolicyStage",
        "Canary",
        "FailThreshold",
        "DeferBackoff",
        "HostGroupState",
        "GroupPackage",
    ] {
        let name = format!("hostkeeper.operator.v1.{message}");
        assert!(older.get_message_by_name(&name).is_none(), "{name} is new");
        assert!(
            current_pool().get_message_by_name(&name).is_some(),
            "{name}"
        );
    }
    for name in ["HostGroupsRefusal", "RollbackMode"] {
        let name = format!("hostkeeper.operator.v1.{name}");
        assert!(older.get_enum_by_name(&name).is_none(), "{name} is new");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.StageAdvance")
            .is_none(),
        "the enum a policy stage reuses is itself newer than the release before"
    );
}

/// Catches: a token or number of `HostGroupsRefusal` renamed, renumbered or reordered, which
/// the gateway writes into a status message and callers branch on: sixteen values numbered 1 to
/// 16 in the design's order after `UNSPECIFIED`, the generated type agreeing with each.
#[test]
fn the_host_groups_refusal_tokens_and_numbers_are_pinned() {
    let tokens = [
        "unauthorized",
        "forbidden",
        "invalid_request",
        "store_failed",
        "audit_unavailable",
        "unknown_host_group",
        "invalid_host_group",
        "invalid_package",
        "invalid_policy",
        "hostkeeper_not_listed",
        "too_many_packages",
        "generation_mismatch",
        "not_empty",
        "unknown_host",
        "same_host_group",
        "reserved_host_group",
    ];
    let mut want = vec![("HOST_GROUPS_REFUSAL_UNSPECIFIED".to_owned(), 0)];
    for (i, token) in tokens.iter().enumerate() {
        let number = i32::try_from(i + 1).expect("small");
        want.push((
            format!("HOST_GROUPS_REFUSAL_{}", token.to_uppercase()),
            number,
        ));
    }
    assert_eq!(
        values(&current_pool(), "hostkeeper.operator.v1.HostGroupsRefusal"),
        want
    );
    for (name, number) in want {
        let value = op::HostGroupsRefusal::try_from(number).expect("a generated value");
        assert_eq!(value.as_str_name(), name);
        assert_eq!(value as i32, number);
    }
    assert!(op::HostGroupsRefusal::try_from(17).is_err());
}

/// Catches: a refusal or outcome value added to a shipped enum at a number the release before
/// knows, a value of the release before renamed or renumbered, or a new token spelled other than
/// the design gives it: at each number added the baseline knows no value, every baseline value
/// keeps its name and number in this build, and this build has the new value under its token.
#[test]
fn the_new_values_of_shipped_enums_are_unknown_to_the_release_before() {
    let cases: [(&str, i32, &str, i32, &str); 15] = [
        (
            "ReleasesRefusal",
            29,
            "RELEASES_REFUSAL_NOT_IN_HOST_GROUP",
            op::ReleasesRefusal::NotInHostGroup as i32,
            op::ReleasesRefusal::NotInHostGroup.as_str_name(),
        ),
        (
            "ReleasesRefusal",
            30,
            "RELEASES_REFUSAL_AMBIGUOUS_HOST_GROUP",
            op::ReleasesRefusal::AmbiguousHostGroup as i32,
            op::ReleasesRefusal::AmbiguousHostGroup.as_str_name(),
        ),
        (
            "ReleasesRefusal",
            31,
            "RELEASES_REFUSAL_LIMITS_IN_HOST_GROUP",
            op::ReleasesRefusal::LimitsInHostGroup as i32,
            op::ReleasesRefusal::LimitsInHostGroup.as_str_name(),
        ),
        (
            "ServicesRefusal",
            12,
            "SERVICES_REFUSAL_PLACED_BY_HOST_GROUP",
            op::ServicesRefusal::PlacedByHostGroup as i32,
            op::ServicesRefusal::PlacedByHostGroup.as_str_name(),
        ),
        (
            "ServicesRefusal",
            13,
            "SERVICES_REFUSAL_UNKNOWN_HOST_GROUP",
            op::ServicesRefusal::UnknownHostGroup as i32,
            op::ServicesRefusal::UnknownHostGroup.as_str_name(),
        ),
        (
            "ServicesRefusal",
            14,
            "SERVICES_REFUSAL_NOT_IN_HOST_GROUP",
            op::ServicesRefusal::NotInHostGroup as i32,
            op::ServicesRefusal::NotInHostGroup.as_str_name(),
        ),
        (
            "EnrollmentRefusal",
            24,
            "ENROLLMENT_REFUSAL_SERVICES_RETIRED",
            op::EnrollmentRefusal::ServicesRetired as i32,
            op::EnrollmentRefusal::ServicesRetired.as_str_name(),
        ),
        (
            "EnrollmentRefusal",
            25,
            "ENROLLMENT_REFUSAL_HOST_GROUP_DIFFERS",
            op::EnrollmentRefusal::HostGroupDiffers as i32,
            op::EnrollmentRefusal::HostGroupDiffers.as_str_name(),
        ),
        (
            "EnrollmentRefusal",
            26,
            "ENROLLMENT_REFUSAL_UNKNOWN_HOST_GROUP",
            op::EnrollmentRefusal::UnknownHostGroup as i32,
            op::EnrollmentRefusal::UnknownHostGroup.as_str_name(),
        ),
        (
            "RolloutsRefusal",
            11,
            "ROLLOUTS_REFUSAL_AMBIGUOUS_HOST_GROUP",
            op::RolloutsRefusal::AmbiguousHostGroup as i32,
            op::RolloutsRefusal::AmbiguousHostGroup.as_str_name(),
        ),
        (
            "RolloutsRefusal",
            12,
            "ROLLOUTS_REFUSAL_NOT_IN_HOST_GROUP",
            op::RolloutsRefusal::NotInHostGroup as i32,
            op::RolloutsRefusal::NotInHostGroup.as_str_name(),
        ),
        (
            "WaitOutcome",
            5,
            "WAIT_OUTCOME_NO_DESIRED",
            op::WaitOutcome::NoDesired as i32,
            op::WaitOutcome::NoDesired.as_str_name(),
        ),
        (
            "GateReason",
            34,
            "GATE_REASON_INSTALL_ORDER",
            op::GateReason::InstallOrder as i32,
            op::GateReason::InstallOrder.as_str_name(),
        ),
        (
            "GateReason",
            35,
            "GATE_REASON_MOVING",
            op::GateReason::Moving as i32,
            op::GateReason::Moving.as_str_name(),
        ),
        (
            "GateReason",
            36,
            "GATE_REASON_UNINSTALL_ORDER",
            op::GateReason::UninstallOrder as i32,
            op::GateReason::UninstallOrder.as_str_name(),
        ),
    ];
    let (older, now) = (baseline_pool(), current_pool());
    for (enum_name, number, token, generated_number, generated_token) in cases {
        let name = format!("hostkeeper.operator.v1.{enum_name}");
        assert_eq!(generated_number, number, "{token}");
        assert_eq!(generated_token, token);
        let current = now.get_enum_by_name(&name).expect("served");
        let value = current.get_value(number).expect("the new value");
        assert_eq!(value.name(), token);
        let Some(before) = older.get_enum_by_name(&name) else {
            // The whole enum is newer than the release before, which then knows no value.
            assert!(
                ["EnrollmentRefusal", "RolloutsRefusal", "WaitOutcome"].contains(&enum_name),
                "{name}"
            );
            continue;
        };
        assert!(before.get_value(number).is_none(), "{token}");
        assert!(before.get_value_by_name(token).is_none(), "{token}");
        for kept in before.values() {
            let same = current.get_value(kept.number()).expect("kept");
            assert_eq!(same.name(), kept.name(), "{name}");
        }
    }
}

/// `base` encoded, after checking that the generated type reads it back as it was.
fn encoded<M: prost::Message + Default + PartialEq + Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// The fields `message` has set, by number.
fn known(message: &DynamicMessage) -> Vec<(u32, Value)> {
    message
        .fields()
        .map(|(field, value)| (field.number(), value.clone()))
        .collect()
}

/// Proves, for the field `field` (number `number`) of the shipped message `name`, which `set`
/// sets on `base`: the release before has no field at that number or of that name; its message
/// reads what this build writes with the field set, every field it knows unchanged, and writes
/// it back byte for byte; and this build reads what that release writes, with the known fields
/// copied over, as `base` with the field at its default. A message the release before does not
/// have at all (it came after that release, with the field in it) is read by no older reader;
/// only the last of the three then holds, of this build's message without the field.
fn check<M>(name: &str, field: &str, number: u32, base: M, set: impl Fn(&mut M))
where
    M: prost::Message + Default + Clone + PartialEq + Debug,
{
    let (older, now) = (baseline_pool(), current_pool());
    let full = name.to_owned();
    let served = now.get_message_by_name(&full).expect("served");
    let added = served.get_field(number).expect("the new field");
    assert_eq!(added.name(), field, "{full}");
    let mut with = base.clone();
    set(&mut with);
    assert_ne!(with, base, "{full}: setting {field} changes the message");
    let bytes = encoded(&with);
    let plain = encoded(&base);
    let Some(before) = older.get_message_by_name(&full) else {
        // The whole message is newer than the release before: it reads none of it. This build
        // reads the message without the field with the field at its default.
        assert_eq!(
            M::decode(plain.as_slice()).expect("decodes"),
            base,
            "{full}"
        );
        return;
    };
    assert!(
        before.get_field(number).is_none(),
        "{full} has no field {number}"
    );
    assert!(
        before.get_field_by_name(field).is_none(),
        "{full} has no {field}"
    );

    let read = DynamicMessage::decode(before.clone(), bytes.as_slice()).expect("decodes");
    let read_plain = DynamicMessage::decode(before.clone(), plain.as_slice()).expect("decodes");
    assert_eq!(known(&read), known(&read_plain), "{full}: known fields");
    assert_eq!(read.encode_to_vec(), bytes, "{full}: the new field is kept");

    let mut written = DynamicMessage::new(before);
    for (known_field, value) in read_plain.fields() {
        written.set_field(&known_field, value.clone());
    }
    let from_before = M::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(from_before, base, "{full}: {field} reads at its default");
}

/// Catches: a field added to a released message that the release before drops, mis-reads or
/// cannot read past, or that this build reads as set when the release before never wrote it: for
/// every message that gained a host group (or the host facts), the baseline has no such field
/// (some of the messages came after that release and are not in it at all), reads the message with all it knows, writes the new field back unchanged, and this
/// build reads the baseline's message with the field empty.
#[test]
fn an_older_reader_keeps_every_new_host_group_field_and_this_build_reads_it_empty() {
    fn group(m: &mut String) {
        *m = "web".to_owned();
    }
    let ns = "hostkeeper.operator.v1";
    check(
        &format!("{ns}.Host"),
        "host_group",
        25,
        op::Host {
            node_id: "web-1".into(),
            role: "frontend".into(),
            live: true,
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.SetDesiredRequest"),
        "host_group",
        9,
        op::SetDesiredRequest {
            service: "web".into(),
            if_generation: Some(3),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.ConvergeRequest"),
        "host_group",
        2,
        op::ConvergeRequest {
            service: "web".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.GetReleaseRequest"),
        "host_group",
        3,
        op::GetReleaseRequest {
            service: "web".into(),
            version: "2.0".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.WaitConvergedRequest"),
        "host_group",
        4,
        op::WaitConvergedRequest {
            service: "web".into(),
            version: "2.0".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.ForceRequest"),
        "host_group",
        8,
        op::ForceRequest {
            service: "web".into(),
            version: "2.0".into(),
            reason: "now".into(),
            include_held: true,
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.ServiceStatus"),
        "host_group",
        9,
        op::ServiceStatus {
            service: "web".into(),
            up: 2,
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.ServiceEntry"),
        "host_groups",
        11,
        op::ServiceEntry {
            service: "web".into(),
            hosts: vec!["web-1".into()],
            ..Default::default()
        },
        |m| m.host_groups = vec!["web".into(), "edge".into()],
    );
    check(
        &format!("{ns}.ListAuditRequest"),
        "host_group",
        9,
        op::ListAuditRequest {
            limit: 5,
            newest: true,
            ..Default::default()
        },
        |m| m.host_group = Some("web".into()),
    );
    check(
        &format!("{ns}.AuditEntry"),
        "host_group",
        9,
        op::AuditEntry {
            seq: 7,
            action: "host_move".into(),
            node_id: Some("web-1".into()),
            ..Default::default()
        },
        |m| m.host_group = Some("web".into()),
    );
    for (message, number) in [
        ("AdvanceRequest", 3),
        ("PauseRequest", 3),
        ("ResumeRequest", 3),
    ] {
        let name = format!("{ns}.{message}");
        match message {
            "AdvanceRequest" => check(
                &name,
                "host_group",
                number,
                op::AdvanceRequest {
                    service: "web".into(),
                    reason: "go".into(),
                    ..Default::default()
                },
                |m| group(&mut m.host_group),
            ),
            "PauseRequest" => check(
                &name,
                "host_group",
                number,
                op::PauseRequest {
                    service: "web".into(),
                    reason: "wait".into(),
                    ..Default::default()
                },
                |m| group(&mut m.host_group),
            ),
            _ => check(
                &name,
                "host_group",
                number,
                op::ResumeRequest {
                    service: "web".into(),
                    reason: "go on".into(),
                    ..Default::default()
                },
                |m| group(&mut m.host_group),
            ),
        }
    }
    check(
        &format!("{ns}.RolloutPlan"),
        "host_group",
        11,
        op::RolloutPlan {
            version: "2.0".into(),
            current: 1,
            eligible: 4,
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.ApproveRequest"),
        "host_group",
        8,
        op::ApproveRequest {
            request_id: "ab".repeat(32),
            host_id: "web-1".into(),
            role: "frontend".into(),
            reason: "new".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.AddRecordRequest"),
        "host_group",
        10,
        op::AddRecordRequest {
            host_id: "web-1".into(),
            role: "frontend".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.InventoryRecord"),
        "host_group",
        16,
        op::InventoryRecord {
            host_id: "web-1".into(),
            role: "frontend".into(),
            ..Default::default()
        },
        |m| group(&mut m.host_group),
    );
    check(
        &format!("{ns}.PendingEnrollment"),
        "record_host_group",
        22,
        op::PendingEnrollment {
            request_id: "ab".repeat(32),
            record_host_id: "web-1".into(),
            ..Default::default()
        },
        |m| group(&mut m.record_host_group),
    );
    check(
        "hostkeeper.agent.v1.HeartbeatRequest",
        "facts_generation",
        12,
        agent::HeartbeatRequest {
            features: vec!["heartbeat-v2".into()],
            ..Default::default()
        },
        |m| m.facts_generation = Some(4),
    );
    check(
        "hostkeeper.agent.v1.HeartbeatResponse",
        "facts",
        6,
        agent::HeartbeatResponse {
            features: vec!["facts-v1".into()],
            ..Default::default()
        },
        |m| {
            m.facts = Some(agent::HostFacts {
                generation: 4,
                host_id: "web-1".into(),
                host_group: "web".into(),
                labels: [("rack".to_owned(), "r1".to_owned())].into(),
            });
        },
    );
}

/// Catches: an explicit-presence field of this change losing its presence, so that an explicit
/// zero or empty value (a policy that holds the group with `max_updating` 0, an audit filter on
/// the empty group) reads as absent, or the other way round: each survives an encode and decode
/// as set, differs on the wire from absent, and a message without it reads as `None`; the
/// descriptors say so too, and a heartbeat answer without facts reads `facts` as `None`.
#[test]
fn explicit_zero_and_empty_values_are_distinct_from_absent() {
    fn present<M: prost::Message + Default + PartialEq + Debug>(set: M, absent: M) {
        let bytes = encoded(&set);
        assert_ne!(bytes, encoded(&absent));
        assert_eq!(M::decode(bytes.as_slice()).expect("decodes"), set);
        assert_eq!(M::decode(&[][..]).expect("decodes"), M::default());
    }
    present(
        op::AuditEntry {
            host_group: Some(String::new()),
            ..Default::default()
        },
        op::AuditEntry::default(),
    );
    present(
        op::ListAuditRequest {
            host_group: Some(String::new()),
            ..Default::default()
        },
        op::ListAuditRequest::default(),
    );
    present(
        agent::HeartbeatRequest {
            facts_generation: Some(0),
            ..Default::default()
        },
        agent::HeartbeatRequest::default(),
    );
    present(
        op::RolloutPolicy {
            max_updating: Some(0),
            live_rollouts: Some(0),
            ..Default::default()
        },
        op::RolloutPolicy::default(),
    );
    present(
        op::FailThreshold {
            percent: Some(0),
            min_hosts: Some(0),
        },
        op::FailThreshold::default(),
    );
    present(
        op::DeferBackoff {
            initial_secs: Some(0),
            factor: Some(0),
            max_secs: Some(0),
        },
        op::DeferBackoff::default(),
    );
    present(
        op::GroupPackage {
            desired_version: Some(String::new()),
            min_version: Some(String::new()),
            ..Default::default()
        },
        op::GroupPackage::default(),
    );
    present(
        op::PutHostGroupRequest {
            if_generation: Some(0),
            ..Default::default()
        },
        op::PutHostGroupRequest::default(),
    );

    let now = current_pool();
    for (message, field) in [
        ("hostkeeper.operator.v1.AuditEntry", "host_group"),
        ("hostkeeper.operator.v1.ListAuditRequest", "host_group"),
        ("hostkeeper.agent.v1.HeartbeatRequest", "facts_generation"),
        ("hostkeeper.operator.v1.RolloutPolicy", "max_updating"),
        ("hostkeeper.operator.v1.RolloutPolicy", "live_rollouts"),
        ("hostkeeper.operator.v1.FailThreshold", "percent"),
        ("hostkeeper.operator.v1.FailThreshold", "min_hosts"),
        ("hostkeeper.operator.v1.DeferBackoff", "initial_secs"),
        ("hostkeeper.operator.v1.DeferBackoff", "factor"),
        ("hostkeeper.operator.v1.DeferBackoff", "max_secs"),
    ] {
        let found = now
            .get_message_by_name(message)
            .and_then(|m| m.get_field_by_name(field))
            .expect("the field");
        assert!(found.supports_presence(), "{message}.{field}");
    }

    let answer = agent::HeartbeatResponse::decode(&[][..]).expect("decodes");
    assert_eq!(answer.facts, None);
    let empty_facts = agent::HeartbeatResponse {
        facts: Some(agent::HostFacts::default()),
        ..Default::default()
    };
    let read = agent::HeartbeatResponse::decode(encoded(&empty_facts).as_slice()).expect("decodes");
    assert_eq!(read.facts, Some(agent::HostFacts::default()));
}

/// The body a `hkctl --request` file carries for `HostGroups.Put`, as the gateway reads it.
fn put_body() -> serde_json::Value {
    serde_json::json!({
        "hostGroup": {
            "name": "web",
            "packages": ["hostkeeper", "tierstore", "tierstore-config-web"],
            "policy": {
                "stages": [
                    { "percent": 10 },
                    { "percent": 100, "advance": "STAGE_ADVANCE_MANUAL" },
                ],
                "canary": { "count": 2 },
                "bakeBeats": 3,
                "rollback": "ROLLBACK_MODE_AUTOMATIC",
                "maxUpdating": 0,
            },
        },
        "ifGeneration": "4",
        "reason": "add config",
    })
}

/// Catches: the proto3 JSON of a `Put` read with a field dropped, renamed or mis-typed (a
/// 64-bit generation as a string, an enum by name), `maxUpdating` 0 read as absent (which would
/// turn a group on hold into the default of one at a time), an absent `liveRollouts` read as
/// set, or a body with a key the request does not have accepted, which the gateway answers
/// `invalid_request`: the body reads into the request field for field, and one with an unknown
/// key at the top, in the group or in the policy is refused by name.
#[test]
fn the_json_of_a_put_reads_into_the_request_and_an_unknown_key_is_refused() {
    let desc = current_pool()
        .get_message_by_name("hostkeeper.operator.v1.PutHostGroupRequest")
        .expect("served");
    let read = DynamicMessage::deserialize(desc.clone(), put_body())
        .expect("reads")
        .transcode_to::<op::PutHostGroupRequest>()
        .expect("a request");
    let want = op::PutHostGroupRequest {
        host_group: Some(op::HostGroup {
            name: "web".to_owned(),
            packages: vec![
                "hostkeeper".to_owned(),
                "tierstore".to_owned(),
                "tierstore-config-web".to_owned(),
            ],
            policy: Some(op::RolloutPolicy {
                stages: vec![
                    op::PolicyStage {
                        percent: 10,
                        advance: op::StageAdvance::Unspecified.into(),
                    },
                    op::PolicyStage {
                        percent: 100,
                        advance: op::StageAdvance::Manual.into(),
                    },
                ],
                canary: Some(op::Canary { count: 2 }),
                bake_beats: 3,
                rollback: op::RollbackMode::Automatic.into(),
                rollback_after_failed: None,
                defer_backoff: None,
                max_updating: Some(0),
                min_up: 0,
                live_rollouts: None,
            }),
        }),
        if_generation: Some(4),
        reason: "add config".to_owned(),
    };
    assert_eq!(read, want);

    for place in ["", "hostGroup", "policy"] {
        let mut body = put_body();
        let target = match place {
            "" => &mut body,
            "hostGroup" => &mut body["hostGroup"],
            _ => &mut body["hostGroup"]["policy"],
        };
        target
            .as_object_mut()
            .expect("an object")
            .insert("bogus".to_owned(), serde_json::json!(1));
        let refused = DynamicMessage::deserialize_with_options(
            desc.clone(),
            body,
            &DeserializeOptions::new(),
        )
        .expect_err("an unknown key is refused");
        assert!(refused.to_string().contains("bogus"), "{place}: {refused}");
    }
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
        repository: String::new(),
        host_groups: vec!["web".to_owned()],
    };
    let cases: [(&str, &str, Vec<u8>); 3] = [
        (
            "hostkeeper.operator.v1.AssignRequest",
            "role",
            encoded(&assign),
        ),
        (
            "hostkeeper.operator.v1.UnassignRequest",
            "role",
            encoded(&unassign),
        ),
        (
            "hostkeeper.operator.v1.ServiceEntry",
            "assigned",
            encoded(&entry),
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
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.ServiceEntry")
        .expect("released");
    assert!(message.get_field_by_name("host_groups").is_none());
    let bytes = encoded(&entry);
    let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
    let back = op::ServiceEntry::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back.host_groups, vec!["web".to_owned()]);
    assert_eq!(back, entry);
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
