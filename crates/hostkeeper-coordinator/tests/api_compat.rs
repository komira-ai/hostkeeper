//! The two coordinator APIs carry the same full update order, and the JSON route's answer keeps
//! the bytes older agents read: golden answers in `tests/fixtures/api` (see its README).

mod support;

use std::path::Path;

use hostkeeper_coordinator::identity::identity_of;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::wire::{Answer, Heartbeat};
use support::rig::{Rig, Spec};

/// The rollout file of these tests: `web` on the role `daemon`.
const ROLLOUT: &str = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";

/// The package digest CI registers for `web` 2.0.
fn digest() -> String {
    "ab".repeat(32)
}

/// The golden answer `name`, without its trailing newline.
fn golden(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/api")
        .join(name);
    std::fs::read_to_string(path)
        .expect("the golden")
        .trim_end()
        .to_owned()
}

/// A heartbeat of a host running `web` 1.0 that announces `features`.
fn beat(features: &[&str]) -> String {
    let announced: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
    let features = if features.is_empty() {
        String::new()
    } else {
        format!(r#","agent_features":[{}]"#, announced.join(","))
    };
    format!(
        r#"{{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[{{"name":"web","version":"1.0","health":"healthy","last_update":null,"updating":null}}]{features}}}"#
    )
}

/// A coordinator of `web`, past its quiet start, with `web` desired at 2.0 and its digest
/// registered.
async fn rig(name: &str) -> Rig {
    let spec = Spec::recorded()
        .with_services(ROLLOUT)
        .without_legacy()
        .with_oidc_services(&["web"]);
    let rig = Rig::new(name, spec);
    rig.at(1_100);
    let body = format!(
        r#"{{"service":"web","version":"2.0","release_digest":"{}"}}"#,
        digest()
    );
    let (code, answer) = rig.desired(&body).await;
    assert_eq!(code, 200, "{answer}");
    rig
}

/// Catches an older agent broken by the new fields: an agent that announces no feature (kbf's)
/// is answered the golden plain bytes, which the wire reads back to the same bytes.
#[tokio::test]
async fn an_agent_without_order_v1_gets_the_golden_plain_answer() {
    let rig = rig("api-compat-plain").await;
    let (code, body) = rig.heartbeat("d1", "daemon", &beat(&[])).await;
    assert_eq!(
        (code, body.as_str()),
        (200, golden("answer-update-plain.json").as_str())
    );
    let read: Answer = serde_json::from_str(&body).expect("an answer");
    assert_eq!(read.order_id, None);
    assert_eq!(serde_json::to_string(&read).expect("JSON"), body);
}

/// Catches a JSON answer that drifts from its golden, or that the wire reads into something
/// else: an `order-v1` agent is answered the golden bytes with the order's terms, and the wire
/// reads them back to the same bytes and the same terms.
#[tokio::test]
async fn an_order_v1_agent_gets_the_golden_answer_with_the_terms() {
    let rig = rig("api-compat-order-v1").await;
    let (code, body) = rig.heartbeat("d1", "daemon", &beat(&["order-v1"])).await;
    assert_eq!(
        (code, body.as_str()),
        (200, golden("answer-update-order-v1.json").as_str())
    );
    let read: Answer = serde_json::from_str(&body).expect("an answer");
    assert_eq!(
        read.order_id.as_ref().map(|id| id.as_str()),
        Some("update-33e0fe2c5f9ee4b0e0ab882467d70d38")
    );
    assert_eq!(read.order_try, Some(1));
    assert_eq!(
        read.package_digest.as_ref().map(|d| d.as_str().to_owned()),
        Some(digest())
    );
    assert_eq!(serde_json::to_string(&read).expect("JSON"), body);
}

/// Catches the two APIs carrying different orders for one update: for the same heartbeat, the
/// JSON answer's terms and the typed order of the transport that carries them (`Order` of
/// `hostkeeper.agent.v1`) have the same order id, try, service, version and package digest.
#[tokio::test]
async fn the_json_answer_and_the_typed_order_carry_the_same_update() {
    let rig = rig("api-compat-parity").await;
    let (code, body) = rig.heartbeat("d1", "daemon", &beat(&["order-v1"])).await;
    assert_eq!(code, 200, "{body}");
    let json: Answer = serde_json::from_str(&body).expect("an answer");

    // The same host's next heartbeat on the typed transport: its order is resent.
    let peer = rig.updater("d1", "daemon").expect("a peer");
    let who = identity_of(&peer, rig.coordinator.prefixes()).expect("an updater");
    let next: Heartbeat = serde_json::from_str(&beat(&["order-v1"])).expect("a heartbeat");
    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders(who, next)
        .expect("an answer");
    assert_eq!(answer, json, "the answer is the same on both");
    let typed = order.expect("a typed order").to_proto();
    assert_eq!(
        typed,
        agent::Order {
            order_id: json.order_id.expect("an order id").as_str().to_owned(),
            r#try: json.order_try.expect("a try"),
            verb: Some(agent::order::Verb::Update(agent::Update {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: json.package_digest.expect("a digest").as_str().to_owned(),
            })),
        }
    );
}
