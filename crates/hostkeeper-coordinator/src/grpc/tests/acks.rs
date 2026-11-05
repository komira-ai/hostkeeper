//! The acks of a heartbeat answer over gRPC: `HeartbeatResponse.acks` is what the JSON answer's
//! `acks` says for the same heartbeat, and only an agent that announced `verdict-acks-v1` is
//! sent any.

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_wire::wire::VERDICT_ACKS_FEATURE;
use serde_json::{Value, json};
use tonic::Code;

use super::heartbeat::{beat, farm, json_beat, legacy_beat, updater};

/// The verdict `o-3`, try 1, of the update to 2.0 that failed, reported by kbf's entry.
fn with_verdict(
    mut request: proto::HeartbeatRequest,
    features: &[&str],
) -> proto::HeartbeatRequest {
    request.features = features.iter().map(|name| (*name).to_owned()).collect();
    request.services[0].last_update = Some(proto::UpdateResult {
        target: "2.0".to_owned(),
        outcome: proto::Outcome::Failed as i32,
        order_id: Some("o-3".to_owned()),
        order_try: Some(1),
        ..proto::UpdateResult::default()
    });
    request
}

/// The JSON heartbeat that says what [`with_verdict`] says over gRPC.
fn json_verdict(features: &[&str]) -> String {
    let mut beat = json!({
        "platform": "linux-x86_64",
        "version": "1.0",
        "health": "healthy",
        "last_update": {
            "target": "2.0", "outcome": "failed", "order_id": "o-3", "order_try": 1
        },
    });
    if !features.is_empty() {
        beat["agent_features"] = json!(features);
    }
    beat.to_string()
}

/// Proves that an agent that announced `verdict-acks-v1` and reported a verdict that names its
/// order and try is acknowledged it in `HeartbeatResponse.acks`, the same service, order id and
/// try as the `acks` of the JSON answer to the same heartbeat on another farm.
#[tokio::test]
async fn a_stored_verdict_is_acknowledged_alike_on_grpc_and_json() {
    let over_json = farm("grpc-acks-json").await;
    let over_grpc = farm("grpc-acks-grpc").await;
    let who = updater("d1", "daemon");
    let features = [VERDICT_ACKS_FEATURE];

    let (code, answer) = json_beat(&over_json.coordinator, &who, &json_verdict(&features)).await;
    assert_eq!(code, 200);
    let json_acks = answer
        .get("acks")
        .cloned()
        .expect("the JSON answer acknowledges");
    assert_eq!(
        json_acks,
        json!([{"service": "kbf", "order_id": "o-3", "order_try": 1}])
    );

    let message = with_verdict(legacy_beat("1.0", None), &features);
    let reply = beat(&over_grpc.coordinator, &who, &message).await;
    assert_eq!(reply.token(), (Code::Ok, ""));
    let response: proto::HeartbeatResponse = reply.decode();
    let grpc_acks: Vec<Value> = response
        .acks
        .iter()
        .map(|ack| {
            json!({"service": ack.service, "order_id": ack.order_id, "order_try": ack.order_try})
        })
        .collect();
    assert_eq!(Value::Array(grpc_acks), json_acks);
    assert_eq!(response.features, vec![VERDICT_ACKS_FEATURE.to_owned()]);
}

/// Proves that an agent that did not announce `verdict-acks-v1` is acknowledged nothing over
/// gRPC, as over JSON, though it reports the same verdict.
#[tokio::test]
async fn an_agent_that_did_not_ask_is_acknowledged_nothing() {
    let over_json = farm("grpc-acks-none-json").await;
    let over_grpc = farm("grpc-acks-none-grpc").await;
    let who = updater("d1", "daemon");

    let (_, answer) = json_beat(&over_json.coordinator, &who, &json_verdict(&[])).await;
    assert!(answer.get("acks").is_none(), "{answer}");

    let message = with_verdict(legacy_beat("1.0", None), &[]);
    let response: proto::HeartbeatResponse =
        beat(&over_grpc.coordinator, &who, &message).await.decode();
    assert!(response.acks.is_empty(), "{:?}", response.acks);
}
