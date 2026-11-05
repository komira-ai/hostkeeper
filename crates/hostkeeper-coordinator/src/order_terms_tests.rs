//! The order terms of the JSON route (`order_id`, `order_try`, `package_digest`): who is sent
//! them, what they say, and that an agent that did not announce `order-v1` is answered with the
//! bytes it has always read. Every time is the rig's clock; nothing sleeps.

use hostkeeper_wire::wire::{Heartbeat, ORDER_FEATURE, OrderId, Outcome, UpdateResult};

use crate::marks_support::{WEB, beat_of, digest, restart, rig, runs, set, start, who};
use crate::order_terms::{order_id_text, order_try, terms};
use crate::registry::Digest;
use crate::service::{FEATURES, HeartbeatRefused, sends};
use crate::testutil::{Log, Rig, ci_token, version};

/// The order id of `web` at 2.0: `update-` and the first 32 hex characters of the SHA-256 of
/// `web`, a newline and `2.0`.
const WEB_ORDER: &str = "update-33e0fe2c5f9ee4b0e0ab882467d70d38";

/// A heartbeat of an agent that announced `features` and runs `running` of `web`, with `last`
/// as the last update it reports of `web`.
fn agent(features: &[&str], running: &str, last: Option<UpdateResult>) -> Heartbeat {
    let mut one = runs(WEB, running);
    one.last_update = last;
    beat_of(features, vec![one])
}

/// The answer to `host`'s heartbeat as the bytes the JSON route sends.
fn bytes(rig: &Rig, host: &str, b: Heartbeat) -> String {
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

/// CI sets `service` to `to` with no package digest.
async fn set_without_digest(rig: &Rig, service: &str, to: &str) {
    let body = format!(r#"{{"service":"{service}","version":"{to}"}}"#);
    rig.coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
        .expect("set");
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
        r#"{{"reply":"update","service":"web","version":"2.0","order_id":"{WEB_ORDER}","order_try":{n},"package_digest":"{}","features":["order-v1"]}}"#,
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
        r#"{"reply":"nothing","features":["order-v1"]}"#
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
/// before this feature: no terms, with or without other features.
///
/// Catches: terms sent to an agent whose strict reader refuses an unknown field.
#[tokio::test]
async fn an_agent_without_order_v1_is_sent_the_bytes_it_always_was() {
    let rig = rig("terms-absent", "");
    for host in ["d1", "d2"] {
        bytes(&rig, host, agent(&[], "1.0", None));
    }
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d1", agent(&[], "1.0", None)),
        r#"{"reply":"update","service":"web","version":"2.0"}"#
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

/// A version with no package digest registered is not ordered to an `order-v1` agent, which
/// takes no slot: a plain agent on the same role is still ordered, and one warning names the
/// service, the version and the host.
///
/// Catches: an order with no digest sent to an agent that needs one; a slot taken by an order
/// that was not sent; a refusal that is silent.
#[tokio::test]
async fn no_registered_digest_means_no_order_for_an_order_v1_agent_and_no_slot() {
    let rig = rig("terms-no-digest", "");
    let log = Log::default();
    let _guard = log.install();
    bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None));
    bytes(&rig, "d2", agent(&[], "1.0", None));
    set_without_digest(&rig, WEB, "2.0").await;

    assert_eq!(
        bytes(&rig, "d1", agent(&[ORDER_FEATURE], "1.0", None)),
        r#"{"reply":"nothing","features":["order-v1"]}"#
    );
    assert_eq!(
        bytes(&rig, "d2", agent(&[], "1.0", None)),
        r#"{"reply":"update","service":"web","version":"2.0"}"#
    );
    let text = log.text();
    assert_eq!(text.matches("no package digest is registered").count(), 1);
    assert!(
        text.contains("web") && text.contains("2.0") && text.contains("d1"),
        "{text}"
    );
}

/// A host that reported a verdict of try 1 of this order id is sent try 2 when the order is
/// given again; a verdict of another order id, or of none, leaves the try at 1.
///
/// Catches: a try that never rises (the verdict would be counted as a replay), one that rises
/// for another order's verdict, and one that restarts at 1 after a coordinator restart.
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
        with_terms(1)
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
    bytes(&rig, "d2", beat_of(&[], vec![]));
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        bytes(&rig, "d2", beat_of(&[], vec![])),
        r#"{"reply":"update","version":"2.0"}"#
    );
}

/// The order id is a function of the service and the version alone, and the try of a verdict
/// is read only for the same order id.
///
/// Catches: an id that changes between runs or hosts, one that ignores the service or the
/// version, a try taken from another order's verdict, and a verdict without a try.
#[test]
fn the_order_id_names_the_update_and_the_try_follows_its_verdict() {
    assert_eq!(order_id_text(WEB, &version("2.0")), WEB_ORDER);
    assert_ne!(order_id_text("db", &version("2.0")), WEB_ORDER);
    assert_ne!(order_id_text(WEB, &version("2.1")), WEB_ORDER);
    assert!(OrderId::parse(&order_id_text(WEB, &version("2.0"))).is_some());

    let id = OrderId::parse(WEB_ORDER).expect("an id");
    assert_eq!(order_try(&id, None), 1);
    assert_eq!(order_try(&id, Some(&verdict(WEB_ORDER, Some(1)))), 2);
    assert_eq!(order_try(&id, Some(&verdict(WEB_ORDER, None))), 1);
    assert_eq!(order_try(&id, Some(&verdict("other", Some(9)))), 1);
    assert_eq!(
        order_try(&id, Some(&verdict(WEB_ORDER, Some(u64::MAX)))),
        u64::MAX
    );
    let plain = UpdateResult::new(version("2.0"), Outcome::Failed);
    assert_eq!(order_try(&id, Some(&plain)), 1);
}

/// The terms carry the registered digest, the id and the try together.
///
/// Catches: a digest or id lost in the assembly.
#[test]
fn terms_join_the_id_the_try_and_the_registered_digest() {
    let registered = Digest::parse(&digest(7)).expect("a digest");
    let made = terms(
        WEB,
        &version("2.0"),
        &registered,
        Some(&verdict(WEB_ORDER, Some(2))),
    )
    .expect("terms");
    assert_eq!(made.order_id.as_str(), WEB_ORDER);
    assert_eq!(made.order_try, 3);
    assert_eq!(made.package_digest.as_str(), digest(7));
}
