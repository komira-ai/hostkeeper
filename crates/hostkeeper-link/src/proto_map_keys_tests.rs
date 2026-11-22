//! The operator keys of `operator-keys-v1` through the conversions of `proto_map`: the report out,
//! the set in.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{heartbeat, reply};
use crate::beat::{ANNOUNCED, Feature, Heartbeat, Platform};
use crate::operator_keys::{KeysRefusal, KeysReport, PushedKeys};

fn beat(report: Option<KeysReport>) -> Heartbeat {
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
        full_disk_access: None,
        operator_keys: report,
        ssh: None,
        prerequisites: None,
        bmc: None,
        maintenance: None,
    }
}

fn sent(report: Option<KeysReport>) -> pb::HeartbeatRequest {
    heartbeat(&beat(report)).expect("a request")
}

/// Proves the report a heartbeat holds goes out as `operator_keys` field for field, with its
/// refusal when it has one and with none when it has none.
#[test]
fn the_report_goes_out_as_its_field() {
    let with = sent(Some(KeysReport {
        digest: "a".repeat(64),
        refused: Some(KeysRefusal {
            digest: "b".repeat(64),
            reason: "key 2 is not an ssh public key".to_owned(),
        }),
        failed: "the file could not be written".to_owned(),
    }));
    assert_eq!(
        with.operator_keys,
        Some(pb::OperatorKeysReport {
            digest: "a".repeat(64),
            refused: Some(pb::OperatorKeysRefusal {
                digest: "b".repeat(64),
                reason: "key 2 is not an ssh public key".to_owned(),
            }),
            failed: "the file could not be written".to_owned(),
        })
    );

    let without = sent(Some(KeysReport {
        digest: "c".repeat(64),
        refused: None,
        failed: String::new(),
    }));
    assert_eq!(
        without.operator_keys,
        Some(pb::OperatorKeysReport {
            digest: "c".repeat(64),
            refused: None,
            failed: String::new(),
        })
    );
}

/// Proves a heartbeat with no report leaves `operator_keys` out, distinct from an empty report.
#[test]
fn no_report_goes_out_as_no_field() {
    assert_eq!(sent(None).operator_keys, None);
    assert_eq!(
        sent(Some(KeysReport::default())).operator_keys,
        Some(pb::OperatorKeysReport::default())
    );
}

/// Proves the set of a response comes in as sent, its keys in order, and a response with none has
/// none.
#[test]
fn a_response_keeps_its_set_as_decoded() {
    let keys = vec![
        "ssh-ed25519 AAAA ops laptop".to_owned(),
        "ssh-ed25519 BBBB build box".to_owned(),
    ];
    let mapped = reply(pb::HeartbeatResponse {
        operator_keys: Some(pb::OperatorKeys {
            digest: "d".repeat(64),
            keys: keys.clone(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.operator_keys,
        Some(PushedKeys {
            digest: "d".repeat(64),
            keys,
        })
    );
    assert_eq!(mapped.dropped, None);
    assert_eq!(
        reply(pb::HeartbeatResponse::default()).reply.operator_keys,
        None
    );
}

/// Proves the empty set, which takes the managed block away, stays a set with no keys and is not
/// read as an absent one.
/// Catches: a mapping that turns the empty set into none, so a coordinator could never take the
/// keys away.
#[test]
fn the_empty_set_is_kept_distinct_from_none() {
    let mapped = reply(pb::HeartbeatResponse {
        operator_keys: Some(pb::OperatorKeys {
            digest: "e".repeat(64),
            keys: Vec::new(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.operator_keys,
        Some(PushedKeys {
            digest: "e".repeat(64),
            keys: Vec::new(),
        })
    );
}

/// Proves `operator-keys-v1` reads back as the feature of that name, in a response's list as by
/// `Feature::parse`, that the feature's name reads back as itself, and that it is not one of the
/// features every daemon announces.
#[test]
fn the_feature_reads_back_from_its_name() {
    assert_eq!(
        Feature::parse("operator-keys-v1"),
        Some(Feature::OperatorKeysV1)
    );
    assert_eq!(Feature::OperatorKeysV1.as_str(), "operator-keys-v1");
    assert!(!ANNOUNCED.contains(&Feature::OperatorKeysV1));
    let mapped = reply(pb::HeartbeatResponse {
        features: vec!["operator-keys-v1".to_owned()],
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(mapped.reply.features, vec![Feature::OperatorKeysV1]);
}
