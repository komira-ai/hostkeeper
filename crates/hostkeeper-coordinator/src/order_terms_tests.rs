//! The order terms of an update (`order_id`, `order_try`, `package_digest`): who is sent them, what
//! they say, and that the JSON route, which carries no order, sends none. The terms are those of
//! an answer over a transport that carries typed orders, to a daemon that announces `target-v1`
//! and lists `apt`. Every time is the rig's clock; nothing sleeps.

use hostkeeper_wire::wire::{Heartbeat, ORDER_FEATURE, OrderId, Outcome, UpdateResult};

use crate::marks_support::{WEB, beat_of, digest, restart, rig, runs, set, start, who};
use crate::order_terms::{order_id_text, terms};
use crate::picks::TARGET_FEATURE;
use crate::registry::{Digest, Package};
use crate::service::{FEATURES, HeartbeatRefused, sends};
use crate::testutil::{Rig, version};

/// The order id of `web` at 2.0: `update-` and the first 32 hex characters of the SHA-256 of
/// `web`, a newline and `2.0`.
const WEB_ORDER: &str = "update-33e0fe2c5f9ee4b0e0ab882467d70d38";

/// A heartbeat of an agent that announced `features` and `target-v1` and runs `running` of `web`,
/// with `last` as the last update it reports of `web`.
fn agent(features: &[&str], running: &str, last: Option<UpdateResult>) -> Heartbeat {
    let mut one = runs(WEB, running);
    one.last_update = last;
    let mut features = features.to_vec();
    features.push(TARGET_FEATURE);
    beat_of(&features, vec![one])
}

/// The answer to `host`'s heartbeat as the bytes a transport that carries typed orders sends.
fn bytes(rig: &Rig, host: &str, b: Heartbeat) -> String {
    let (answer, _) = rig
        .coordinator
        .heartbeat_with_orders_native(
            who(host, "daemon"),
            b,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .expect("answered");
    serde_json::to_string(&answer).expect("json")
}

/// The answer to `host`'s heartbeat as the bytes the JSON route sends.
fn json_bytes(rig: &Rig, host: &str, b: Heartbeat) -> String {
    let answer = rig
        .coordinator
        .heartbeat(who(host, "daemon"), b)
        .expect("answered");
    serde_json::to_string(&answer).expect("json")
}

/// A rig whose legacy service is `web`, past its quiet start.
fn legacy_rig(test: &str) -> Rig {
    let mut rig = rig(test, "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_legacy_service(Some(WEB.to_owned()));
    rig.at(2_100);
    rig
}

/// CI sets `service` to `to` with no release.
async fn set_without_release(rig: &Rig, service: &str, to: &str) {
    let body = format!(r#"{{"service":"{service}","version":"{to}"}}"#);
    rig.set_plain(&body).await;
}

/// A verdict of the order `id` at try `n`, which ended `Succeeded`.
fn verdict(id: &str, n: Option<u64>) -> UpdateResult {
    let mut result = UpdateResult::new(version("2.0"), Outcome::Succeeded);
    result.order_id = OrderId::parse(id);
    result.order_try = n;
    result
}

/// The update answer an agent that announced `order-v1` is sent for `web` at 2.0, try `n`.
fn with_terms(n: u64) -> String {
    format!(
        r#"{{"reply":"update","service":"web","version":"2.0","order_id":"{WEB_ORDER}","order_try":{n},"package_digest":"{}","features":["order-v1","target-v1"]}}"#,
        digest(1)
    )
}

/// An `order-v1` agent is sent the order id, the try and the package digest of the version
/// right after the reply's keys and before `features`, as exact bytes; the coordinator
/// advertises the feature and gates the three fields on it.
///
/// Catches: terms missing from the answer, misplaced, or taken from the wrong version's
/// digest; a feature that is gated but not advertised.
#[tokio::test]
async fn an_order_v1_agent_is_sent_the_terms_of_the_order_as_exact_bytes() {
    let rig = rig("terms-bytes", "");
    assert_eq!(
        bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
        r#"{"reply":"nothing","features":["order-v1","target-v1"]}"#
    );
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
        with_terms(1)
    );
    assert!(FEATURES.contains(&ORDER_FEATURE));
    let agreed = [ORDER_FEATURE.to_owned()];
    for field in ["order_id", "order_try", "package_digest"] {
        assert!(sends(field, Some(&agreed)), "{field}");
        assert!(!sends(field, None), "{field}");
    }
}

/// An agent that did not announce `order-v1` is answered with the bytes of the same state as
/// before this feature: no terms, with or without other features; and the JSON route, which
/// carries no order, answers every agent `nothing`.
///
/// Catches: terms sent to an agent whose strict reader refuses an unknown field, and an order on
/// the JSON route.
#[tokio::test]
async fn an_agent_without_order_v1_is_sent_the_bytes_it_always_was() {
    let rig = rig("terms-absent", "");
    for host in ["d1", "d2"] {
        bytes(&rig, host, agent(&[], "1.0", None));
    }
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        json_bytes(&rig, "d1", beat_of(&[], vec![runs(WEB, "1.0")])),
        r#"{"reply":"nothing"}"#
    );
    // Another rig: the one slot of `web` is d1's in this one.
    let other = crate::marks_support::rig("terms-absent-featured", "");
    bytes(&other, "d2", agent(&["halt-v1"], "1.0", None));
    set(&other, WEB, "2.0", 1).await;
    let featured = bytes(&other, "d2", agent(&["halt-v1"], "1.0", None));
    assert!(
        featured.starts_with(r#"{"reply":"update","service":"web","version":"2.0","features":"#),
        "{featured}"
    );
    assert!(!featured.contains("order_"), "{featured}");
    assert!(!featured.contains("package_digest"), "{featured}");
}

/// A version with no release kept is ordered to no agent, whatever it announced, and takes no
/// slot: a plain agent on the same role is told nothing either.
///
/// Catches: an order with no entry sent to an agent that needs one; a slot taken by an order that
/// was not sent.
#[tokio::test]
async fn no_release_means_no_order_for_any_agent_and_no_slot() {
    let rig = rig("terms-no-release", "");
    bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None));
    bytes(&rig, "d2", agent(&[], "1.0", None));
    set_without_release(&rig, WEB, "2.0").await;

    assert_eq!(
        bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
        r#"{"reply":"nothing","features":["order-v1","target-v1"]}"#
    );
    assert_eq!(
        bytes(&rig, "d2", agent(&[], "1.0", None)),
        r#"{"reply":"nothing","features":["target-v1"]}"#
    );
    // A release for the version orders at once: no slot was taken by the orders not sent.
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d2", agent(&[], "1.0", None)),
        r#"{"reply":"update","service":"web","version":"2.0","features":["target-v1"]}"#
    );
}

/// A host that reported a verdict of try 1 of this order id is sent try 2 when the order is
/// given again; a verdict of another order of the service (an install, an earlier update) at try
/// 7 raises it to 8, since the daemon keys tries per service (design 8.3).
///
/// Catches: a try that never rises (the verdict would be counted as a replay), one that restarts
/// at 1 after another order's verdict (the daemon refuses it `try_conflict` or `stale_try`), and
/// one that restarts at 1 after a coordinator restart.
#[tokio::test]
async fn a_reordered_update_after_a_verdict_is_sent_the_next_try() {
    let mut rig = rig("terms-try", "");
    bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None));
    set(&rig, WEB, "2.0", 1).await;

    let after = |id: &str, n: u64| agent(&[ORDER_FEATURE], "1.0", Some(verdict(id, Some(n))));
    assert_eq!(bytes(&rig, "d1", after(WEB_ORDER, 1)), with_terms(2));
    assert_eq!(bytes(&rig, "d1", after(WEB_ORDER, 4)), with_terms(5));
    assert_eq!(
        bytes(&rig, "d1", after("update-someone-else", 7)),
        with_terms(8)
    );

    // A coordinator that restarted remembers no tries: the host's word is what counts.
    restart(&mut rig, "");
    rig.at(2_100);
    assert_eq!(bytes(&rig, "d1", after(WEB_ORDER, 1)), with_terms(2));
}

/// An order that is sent again before the host reported a verdict keeps its try.
///
/// Catches: a try that counts the answers instead of the verdicts.
#[tokio::test]
async fn a_resent_order_keeps_its_try_until_the_host_reports_a_verdict() {
    let rig = rig("terms-resend", "");
    bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None));
    set(&rig, WEB, "2.0", 1).await;
    for _ in 0..3 {
        assert_eq!(
            bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
            with_terms(1)
        );
    }
}

/// With `--legacy-service` set, an `order-v1` agent's top level is not the legacy service: a
/// heartbeat with no `services` is refused, and its `services` entry naming the legacy service
/// is ordered with the service named.
///
/// Catches: the top level of a daemon counted as the legacy service's state, and an update that
/// does not name its service (the daemon would update the wrong thing).
#[tokio::test]
async fn an_order_v1_agent_is_not_the_legacy_service_and_its_entry_naming_it_is_ordered() {
    let rig = legacy_rig("terms-legacy");

    let mut bare = agent(&[ORDER_FEATURE], "1.0", None);
    bare.services = None;
    assert_eq!(
        rig.coordinator.heartbeat(who("d1", "daemon"), bare),
        Err(HeartbeatRefused)
    );

    bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None));
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
        with_terms(1)
    );
}

/// With `--legacy-service` set, an agent that did not announce `order-v1` is still answered
/// about the legacy service, whose state is its top level, without a service name or terms.
///
/// Catches: a legacy agent's answer changed by the feature.
#[tokio::test]
async fn a_plain_agent_is_still_answered_about_the_legacy_service_by_its_top_level() {
    let rig = legacy_rig("terms-legacy-plain");
    bytes(&rig, "d2", beat_of(&[TARGET_FEATURE], vec![]));
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d2", beat_of(&[TARGET_FEATURE], vec![])),
        r#"{"reply":"update","version":"2.0","features":["target-v1"]}"#
    );
}

/// The order id is a function of the service and the version alone.
///
/// Catches: an id that changes between runs or hosts, and one that ignores the service or the
/// version.
#[test]
fn the_order_id_names_the_update() {
    assert_eq!(order_id_text(WEB, &version("2.0")), WEB_ORDER);
    assert_ne!(order_id_text("db", &version("2.0")), WEB_ORDER);
    assert_ne!(order_id_text(WEB, &version("2.1")), WEB_ORDER);
    assert!(OrderId::parse(&order_id_text(WEB, &version("2.0"))).is_some());
}

/// The terms carry the registered digest, the id and the try together.
///
/// Catches: a digest or id lost in the assembly.
#[test]
fn terms_join_the_id_the_try_and_the_registered_digest() {
    let registered = Package {
        digest: Digest::parse(&digest(7)).expect("a digest"),
        index_digest: None,
    };
    let made = terms(WEB, &version("2.0"), &registered, 3).expect("terms");
    assert_eq!(made.order_id.as_str(), WEB_ORDER);
    assert_eq!(made.order_try, 3);
    assert_eq!(made.package_digest.as_str(), digest(7));
}
