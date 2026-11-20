//! The host prerequisites of `prerequisites-v1` through the conversions of `proto_map`: the report
//! out, the set in.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{heartbeat, reply};
use crate::beat::{ANNOUNCED, Feature, Heartbeat, Platform};
use crate::prerequisites::{
    PrerequisiteState, PrerequisiteStatus, PrerequisitesRefusal, PrerequisitesReport,
    PushedPrerequisites,
};

fn beat(report: Option<PrerequisitesReport>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        operator_keys: None,
        prerequisites: report,
        ssh: None,
        bmc: None,
        maintenance: None,
    }
}

fn sent(report: Option<PrerequisitesReport>) -> pb::HeartbeatRequest {
    heartbeat(&beat(report)).expect("a request")
}

fn state(name: &str, status: PrerequisiteStatus) -> PrerequisiteState {
    PrerequisiteState {
        name: name.to_owned(),
        status,
        ..PrerequisiteState::default()
    }
}

/// Proves the report a heartbeat holds goes out as `prerequisites` field for field: the digest,
/// the refusal, and each state's name, status, version, reason, failures and times.
#[test]
fn the_report_goes_out_as_its_field() {
    let with = sent(Some(PrerequisitesReport {
        digest: "a".repeat(64),
        refused: Some(PrerequisitesRefusal {
            digest: "b".repeat(64),
            reason: "a name is twice in the set".to_owned(),
        }),
        states: vec![PrerequisiteState {
            name: "xcode-command-line-tools".to_owned(),
            status: PrerequisiteStatus::Failed,
            version: "16.2".to_owned(),
            reason: "softwareupdate exited 1".to_owned(),
            failures: 3,
            next_try_unix: 2_000_005_400,
            seen_unix: 2_000_000_000,
        }],
    }));
    assert_eq!(
        with.prerequisites,
        Some(pb::PrerequisitesReport {
            digest: "a".repeat(64),
            refused: Some(pb::PrerequisitesRefusal {
                digest: "b".repeat(64),
                reason: "a name is twice in the set".to_owned(),
            }),
            states: vec![pb::PrerequisiteState {
                name: "xcode-command-line-tools".to_owned(),
                status: pb::PrerequisiteStatus::Failed.into(),
                version: "16.2".to_owned(),
                reason: "softwareupdate exited 1".to_owned(),
                failures: 3,
                next_try_unix: 2_000_005_400,
                seen_unix: 2_000_000_000,
            }],
        })
    );

    let without = sent(Some(PrerequisitesReport {
        digest: "c".repeat(64),
        refused: None,
        states: Vec::new(),
    }));
    assert_eq!(
        without.prerequisites,
        Some(pb::PrerequisitesReport {
            digest: "c".repeat(64),
            refused: None,
            states: Vec::new(),
        })
    );
}

/// Proves each status of a state goes out as the protobuf status of the same name, and as no
/// other.
#[test]
fn every_status_goes_out_as_its_protobuf_value() {
    let all = [
        (PrerequisiteStatus::Pending, pb::PrerequisiteStatus::Pending),
        (PrerequisiteStatus::Present, pb::PrerequisiteStatus::Present),
        (
            PrerequisiteStatus::Installing,
            pb::PrerequisiteStatus::Installing,
        ),
        (PrerequisiteStatus::Failed, pb::PrerequisiteStatus::Failed),
        (
            PrerequisiteStatus::NotApplicable,
            pb::PrerequisiteStatus::NotApplicable,
        ),
        (PrerequisiteStatus::Unknown, pb::PrerequisiteStatus::Unknown),
    ];
    let request = sent(Some(PrerequisitesReport {
        states: all
            .iter()
            .map(|(status, _)| state("xcode-command-line-tools", *status))
            .collect(),
        ..PrerequisitesReport::default()
    }));
    let out: Vec<i32> = request
        .prerequisites
        .expect("a report")
        .states
        .iter()
        .map(|state| state.status)
        .collect();
    let expected: Vec<i32> = all.iter().map(|(_, wire)| (*wire).into()).collect();
    assert_eq!(out, expected);
    let mut distinct = out.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), all.len());
    assert!(!out.contains(&i32::from(pb::PrerequisiteStatus::Unspecified)));
}

/// Proves the wire name of each status is `PREREQUISITE_STATUS_` and its name in capitals, as the
/// protobuf enum names it.
#[test]
fn a_status_has_its_protobuf_name() {
    for status in [
        PrerequisiteStatus::Pending,
        PrerequisiteStatus::Present,
        PrerequisiteStatus::Installing,
        PrerequisiteStatus::Failed,
        PrerequisiteStatus::NotApplicable,
        PrerequisiteStatus::Unknown,
    ] {
        let request = sent(Some(PrerequisitesReport {
            states: vec![state("x", status)],
            ..PrerequisitesReport::default()
        }));
        let wire = request.prerequisites.expect("a report").states[0].status;
        let named = pb::PrerequisiteStatus::try_from(wire).expect("a known status");
        assert_eq!(named.as_str_name(), status.as_str());
    }
}

/// Proves a heartbeat with no report leaves `prerequisites` out, distinct from an empty report.
#[test]
fn no_report_goes_out_as_no_field() {
    assert_eq!(sent(None).prerequisites, None);
    assert_eq!(
        sent(Some(PrerequisitesReport::default())).prerequisites,
        Some(pb::PrerequisitesReport::default())
    );
}

/// Proves the set of a response comes in as sent, its names in order, and a response with none
/// has none.
#[test]
fn a_response_keeps_its_set_as_decoded() {
    let names = vec![
        "xcode-command-line-tools".to_owned(),
        "another-one".to_owned(),
    ];
    let mapped = reply(pb::HeartbeatResponse {
        prerequisites: Some(pb::HostPrerequisites {
            digest: "d".repeat(64),
            names: names.clone(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.prerequisites,
        Some(PushedPrerequisites {
            digest: "d".repeat(64),
            names,
        })
    );
    assert_eq!(mapped.dropped, None);
    assert_eq!(
        reply(pb::HeartbeatResponse::default()).reply.prerequisites,
        None
    );
}

/// Proves the empty set, which needs nothing, stays a set with no names and is not read as an
/// absent one.
/// Catches: a mapping that turns the empty set into none.
#[test]
fn the_empty_set_is_kept_distinct_from_none() {
    let mapped = reply(pb::HeartbeatResponse {
        prerequisites: Some(pb::HostPrerequisites {
            digest: "e".repeat(64),
            names: Vec::new(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.prerequisites,
        Some(PushedPrerequisites {
            digest: "e".repeat(64),
            names: Vec::new(),
        })
    );
}

/// Proves `prerequisites-v1` reads back as the feature of that name, in a response's list as by
/// `Feature::parse`, and that it is not one of the features every daemon announces.
#[test]
fn the_feature_reads_back_from_its_name() {
    assert_eq!(
        Feature::parse("prerequisites-v1"),
        Some(Feature::PrerequisitesV1)
    );
    assert_eq!(Feature::PrerequisitesV1.as_str(), "prerequisites-v1");
    assert!(!ANNOUNCED.contains(&Feature::PrerequisitesV1));
    let mapped = reply(pb::HeartbeatResponse {
        features: vec!["prerequisites-v1".to_owned()],
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(mapped.reply.features, vec![Feature::PrerequisitesV1]);
}
