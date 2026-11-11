//! The two coordinator APIs carry the same full update order, the JSON route carries none, and the
//! wire keeps reading the update answers older agents read: golden answers in
//! `tests/fixtures/api` (see its README).

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use hostkeeper_coordinator::identity::identity_of;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::wire::{Answer, Heartbeat};
use support::beats::{Daemon, grpc_beat, runs};
use support::ci_token;
use support::rig::{Rig, Spec};
use support::targets::{SET_DESIRED, grpc, list, put_komira, repository_entry, set};

/// The rollout file of these tests: `web` on the role `daemon`.
const ROLLOUT: &str = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";

/// The package digest of the golden answers' update of `web` to 2.0.
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

/// A coordinator of `web`, past its quiet start, with `web` desired at 2.0 as a release that lists
/// one `apt` entry for `linux-x86_64`.
async fn rig(name: &str) -> Rig {
    let spec = Spec::recorded()
        .with_services(ROLLOUT)
        .without_legacy()
        .with_oidc_services(&["web"]);
    let rig = Rig::new(name, spec);
    rig.at(1_100);
    put_komira(&rig).await;
    let targets = BTreeMap::from([(
        "linux-x86_64".to_owned(),
        list(vec![repository_entry("web", "1.6.0", 'c')]),
    )]);
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set("web", "2.0", targets),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(2_000);
    rig
}

/// Catches an older agent broken by the new fields: the golden plain update answer, the bytes an
/// agent that announces no feature (kbf's) has always read, is read by the wire back to the same
/// bytes and with no terms, and the JSON route, which carries no order, answers that agent
/// `nothing`.
#[tokio::test]
async fn an_agent_without_order_v1_reads_the_golden_plain_answer() {
    let plain = golden("answer-update-plain.json");
    let read: Answer = serde_json::from_str(&plain).expect("an answer");
    assert_eq!(read.order_id, None);
    assert_eq!(serde_json::to_string(&read).expect("JSON"), plain);

    let rig = rig("api-compat-plain").await;
    let (code, body) = rig.heartbeat("d1", "daemon", &beat(&[])).await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));
}

/// Catches a golden answer that the wire reads into something else: the `order-v1` update answer
/// is read back to the same bytes and the same terms, and the JSON route answers an `order-v1`
/// agent `nothing`, with its features.
#[tokio::test]
async fn an_order_v1_agent_reads_the_golden_answer_with_the_terms() {
    let golden = golden("answer-update-order-v1.json");
    let read: Answer = serde_json::from_str(&golden).expect("an answer");
    assert_eq!(
        read.order_id.as_ref().map(|id| id.as_str()),
        Some("update-33e0fe2c5f9ee4b0e0ab882467d70d38")
    );
    assert_eq!(read.order_try, Some(1));
    assert_eq!(
        read.package_digest.as_ref().map(|d| d.as_str().to_owned()),
        Some(digest())
    );
    assert_eq!(serde_json::to_string(&read).expect("JSON"), golden);

    let rig = rig("api-compat-order-v1").await;
    let (code, body) = rig.heartbeat("d1", "daemon", &beat(&["order-v1"])).await;
    assert_eq!(
        (code, body.as_str()),
        (200, r#"{"reply":"nothing","features":["order-v1"]}"#)
    );
}

/// Catches the two APIs carrying different orders for one update: for the same heartbeat of a
/// daemon that announces `target-v1`, the typed order over gRPC and the terms of the answer a
/// transport that carries typed orders gives (`heartbeat_with_orders`) have the same order id,
/// try, service, version and package digest, and the order names the entry picked.
#[tokio::test]
async fn the_answer_and_the_typed_order_carry_the_same_update() {
    let rig = rig("api-compat-parity").await;
    let daemon = Daemon::new("d1", agent::Platform::LinuxX8664, &["apt"]).running(runs(
        "web",
        "1.0",
        Some("apt"),
    ));
    let response = grpc_beat(&rig, &daemon).await;
    let typed = response.order.expect("a typed order");

    // The same host's next heartbeat: its order is resent, with the same terms.
    let peer = rig.updater("d1", "daemon").expect("a peer");
    let who = identity_of(&peer, rig.coordinator.prefixes()).expect("an updater");
    let next: Heartbeat = serde_json::from_str(&daemon.json()).expect("a heartbeat");
    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders(
            who,
            next,
            hostkeeper_coordinator::trusted_root::TrustedRootSaid::default(),
        )
        .expect("an answer");
    assert_eq!(order.expect("a typed order").to_proto(), typed);
    let entry = repository_entry("web", "1.6.0", 'c');
    assert_eq!(
        typed,
        agent::Order {
            order_id: answer.order_id.expect("an order id").as_str().to_owned(),
            r#try: answer.order_try.expect("a try"),
            verb: Some(agent::order::Verb::Update(agent::Update {
                target: Some(entry.clone()),
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: answer.package_digest.expect("a digest").as_str().to_owned(),
                index_digest: String::new(),
            })),
        }
    );
    assert_eq!(
        typed.order_id, "update-33e0fe2c5f9ee4b0e0ab882467d70d38",
        "the id names the update"
    );
    assert_eq!(
        match typed.verb {
            Some(agent::order::Verb::Update(update)) => update.package_digest,
            other => panic!("not an update: {other:?}"),
        },
        entry.digest
    );
}
