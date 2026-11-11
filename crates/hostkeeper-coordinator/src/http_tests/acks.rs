//! The acks of a heartbeat answer on both transports (design section 5.4): for each heartbeat
//! of `tests/fixtures/acks/answers.json`, in order, the JSON route answers exactly the bytes the
//! fixture gives, and the typed answer, as `hostkeeper.agent.v1.HeartbeatResponse` on the wire,
//! acknowledges exactly the same verdicts, with the same features.

use prost::Message as _;
use serde::Deserialize;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::wire::{Answer, read_heartbeat};

use super::*;
use crate::rollback_tests::{TWO, rig, who};
use crate::testutil::LEGACY;
use crate::verdict_file::to_proto;

/// One case of the fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    case: String,
    host: String,
    beat: Value,
    /// The JSON answer, byte for byte.
    json: String,
    /// The verdicts acknowledged, on either transport.
    acks: Vec<Acked>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Acked {
    service: String,
    order_id: String,
    order_try: u64,
}

fn cases() -> Vec<Case> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acks/answers.json");
    serde_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("cases")
}

/// The acks of a `HeartbeatResponse` as an agent decodes them from the wire.
fn grpc_acks(answer: &Answer) -> (Vec<String>, Vec<Acked>) {
    let response = agent::HeartbeatResponse {
        features: answer.features.clone().unwrap_or_default(),
        acks: to_proto(&answer.acks),
        ..agent::HeartbeatResponse::default()
    };
    let read =
        agent::HeartbeatResponse::decode(response.encode_to_vec().as_slice()).expect("decodes");
    let acks = read
        .acks
        .into_iter()
        .map(|ack| Acked {
            service: ack.service,
            order_id: ack.order_id,
            order_try: ack.order_try,
        })
        .collect();
    (read.features, acks)
}

/// Catches: the two transports drifting apart, or an answer whose bytes change for an agent
/// that did not ask: every case of the fixture (an agent that announced nothing, one that
/// announced other features, a verdict acknowledged, resent and then no longer sent, a verdict
/// with no order id or no try, a success beside a service no one manages) gets the fixture's
/// JSON bytes on the JSON route, and the same features and acks in `HeartbeatResponse` from the
/// typed answer of a second coordinator fed the same heartbeats.
#[tokio::test]
async fn the_json_and_grpc_answers_acknowledge_the_same_verdicts() {
    let json_rig = rig("http-acks-json", "", TWO).await;
    let typed_rig = rig("http-acks-typed", "", TWO).await;
    let cases = cases();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let body = serde_json::to_string(&case.beat).expect("a body");
        let (code, bytes) = raw(
            &json_rig,
            Surface::Heartbeat,
            &updater(&case.host, "daemon"),
            request("POST", HEARTBEAT_PATH, None, &body),
        )
        .await;
        assert_eq!(code, 200, "{}", case.case);
        let text = String::from_utf8(bytes).expect("text");
        assert_eq!(text, case.json, "{}", case.case);
        let sent: Value = serde_json::from_str(&text).expect("JSON");
        let json_acks: Vec<Acked> = sent
            .get("acks")
            .map(|acks| serde_json::from_value(acks.clone()).expect("acks"))
            .unwrap_or_default();
        assert_eq!(json_acks, case.acks, "{}", case.case);

        let beat = read_heartbeat(body.as_bytes(), Some(LEGACY))
            .expect("a heartbeat")
            .beat;
        let (answer, order) = typed_rig
            .coordinator
            .heartbeat_with_orders(
                who(&case.host),
                beat,
                crate::trusted_root::TrustedRootSaid::default(),
            )
            .expect("an answer");
        assert_eq!(order, None, "{}", case.case);
        assert_eq!(
            serde_json::to_string(&answer).expect("JSON"),
            case.json,
            "{}",
            case.case
        );
        let (features, grpc) = grpc_acks(&answer);
        assert_eq!(grpc, case.acks, "{}", case.case);
        let json_features: Vec<String> = sent
            .get("features")
            .map(|names| serde_json::from_value(names.clone()).expect("names"))
            .unwrap_or_default();
        assert_eq!(features, json_features, "{}", case.case);
    }
}
