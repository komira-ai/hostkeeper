//! The facts a heartbeat over gRPC is answered with ([`crate::facts`]), through
//! `crate::grpc::route`. No socket.

use std::collections::HashMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use super::heartbeat::{beat, farm, json_beat, updater};
use crate::testutil::OPERATOR;

/// A heartbeat over gRPC of a daemon that announces `facts-v1` and reports `generation`.
fn heartbeat(generation: Option<u64>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        platform: agent::Platform::LinuxArm64 as i32,
        agent: Some(agent::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: ["heartbeat-v2", "facts-v1"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        services: vec![agent::ServiceBeat {
            name: "kbf".to_owned(),
            version: "2.0".to_owned(),
            health: agent::Health::Healthy as i32,
            ..agent::ServiceBeat::default()
        }],
        facts_generation: generation,
        ..agent::HeartbeatRequest::default()
    }
}

/// Proves: a heartbeat over gRPC that announces `facts-v1` is answered the host's facts (its
/// generation, id, group and no labels) until a heartbeat reports that generation; after an
/// operator sets labels on the host, the next heartbeat is answered the labels with a higher
/// generation, and one that reports it is answered none; a heartbeat that does not announce the
/// feature is answered none; the same daemon over the JSON route is answered no `facts` key.
///
/// Catches: facts that are not in the answer, in every answer, left out after a label change, sent
/// to a daemon that did not ask or to the JSON route (which is frozen).
#[tokio::test]
async fn a_grpc_heartbeat_is_answered_the_facts_until_the_daemon_reports_them() {
    let farm = farm("facts-heartbeat").await;
    let who = updater("d1", "daemon");

    let answer: agent::HeartbeatResponse = beat(&farm.coordinator, &who, &heartbeat(None))
        .await
        .decode();
    let facts = answer.facts.expect("the facts");
    assert_eq!(
        (facts.host_id.as_str(), facts.host_group.as_str()),
        ("d1", "daemon")
    );
    assert_eq!(facts.labels, HashMap::new());

    let answer: agent::HeartbeatResponse =
        beat(&farm.coordinator, &who, &heartbeat(Some(facts.generation)))
            .await
            .decode();
    assert_eq!(answer.facts, None);

    farm.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"set_labels","node_id":"d1","labels":{"zone":"a"}}"#,
        )
        .expect("labels set");
    let answer: agent::HeartbeatResponse =
        beat(&farm.coordinator, &who, &heartbeat(Some(facts.generation)))
            .await
            .decode();
    let newer = answer.facts.expect("the labels");
    assert!(newer.generation > facts.generation);
    assert_eq!(
        newer.labels,
        HashMap::from([("zone".to_owned(), "a".to_owned())])
    );
    let answer: agent::HeartbeatResponse =
        beat(&farm.coordinator, &who, &heartbeat(Some(newer.generation)))
            .await
            .decode();
    assert_eq!(answer.facts, None);

    let mut plain = heartbeat(None);
    plain.features = vec!["heartbeat-v2".to_owned()];
    let answer: agent::HeartbeatResponse =
        beat(&farm.coordinator, &updater("d2", "daemon"), &plain)
            .await
            .decode();
    assert_eq!(answer.facts, None);

    let json = r#"{"platform":"linux-x86_64","version":"2.0","health":"healthy","agent_features":["heartbeat-v2","facts-v1"]}"#;
    let (code, answered) = json_beat(&farm.coordinator, &updater("d3", "daemon"), json).await;
    assert_eq!(code, 200);
    assert!(answered.get("facts").is_none(), "{answered}");
}
