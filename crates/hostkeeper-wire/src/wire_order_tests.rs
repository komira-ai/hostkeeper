//! The order terms of an update answer (`order_id`, `order_try`, `package_digest`) and the
//! `order-v1` feature that sends them: what the answer writes and reads, and that a heartbeat
//! announcing the feature is not told its top level is the legacy service.

use serde_json::{Value, json};

use crate::version::Version;
use crate::wire::{Answer, ORDER_FEATURE, OrderId, PackageDigest, ServiceReply, read_heartbeat};

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn update_terms() -> Answer {
    Answer {
        order_id: OrderId::parse("update-33e0fe2c5f9ee4b0e0ab882467d70d38"),
        order_try: Some(2),
        package_digest: PackageDigest::parse(DIGEST),
        features: Some(vec![ORDER_FEATURE.to_owned()]),
        ..Answer::of(ServiceReply::Update {
            service: Some("web".to_owned()),
            version: Version::parse("2.0").expect("a version"),
        })
    }
}

fn read(value: &Value) -> Result<Answer, serde_json::Error> {
    serde_json::from_value(value.clone())
}

/// Catches: terms written in another place or spelling, lost on the way back, or an answer
/// without them gaining a key: they follow the reply's keys and come before `features`, and the
/// reader returns what was written.
#[test]
fn an_answer_with_terms_writes_them_after_the_reply_and_reads_back() {
    let sent = update_terms();
    let text = serde_json::to_string(&sent).expect("json");
    assert_eq!(
        text,
        format!(
            r#"{{"reply":"update","service":"web","version":"2.0","order_id":"update-33e0fe2c5f9ee4b0e0ab882467d70d38","order_try":2,"package_digest":"{DIGEST}","features":["order-v1"]}}"#
        )
    );
    let back: Answer = serde_json::from_str(&text).expect("read");
    assert_eq!(back, sent);
    assert_eq!(back.order_try, Some(2));
    assert_eq!(back.order_id, sent.order_id);
    assert_eq!(back.package_digest, sent.package_digest);

    let plain = serde_json::to_string(&Answer::of(ServiceReply::Nothing)).expect("json");
    assert_eq!(plain, r#"{"reply":"nothing"}"#);
}

/// Catches: terms that are half there accepted (a host would act on an order it cannot
/// identify), terms on an answer that orders nothing, and a digest or id that is no such thing.
#[test]
fn the_reader_refuses_partial_terms_terms_on_nothing_and_bad_values() {
    let full = json!({
        "reply": "update", "service": "web", "version": "2.0",
        "order_id": "update-abc", "order_try": 1, "package_digest": DIGEST,
    });
    assert!(read(&full).is_ok());

    for missing in ["order_id", "order_try", "package_digest"] {
        let mut partial = full.clone();
        partial.as_object_mut().expect("object").remove(missing);
        assert!(read(&partial).is_err(), "without {missing}");
        let mut alone = json!({"reply": "update", "version": "2.0"});
        alone[missing] = full[missing].clone();
        assert!(read(&alone).is_err(), "only {missing}");
    }

    let mut nothing = full.clone();
    nothing["reply"] = json!("nothing");
    nothing.as_object_mut().expect("object").remove("service");
    nothing.as_object_mut().expect("object").remove("version");
    assert!(read(&nothing).is_err());

    for (key, bad) in [
        ("package_digest", json!(DIGEST.to_uppercase())),
        ("package_digest", json!("abc")),
        ("package_digest", json!(format!("{}g", &DIGEST[1..]))),
        ("order_id", json!("has space")),
        ("order_id", json!("")),
        ("order_try", json!("one")),
    ] {
        let mut wrong = full.clone();
        wrong[key] = bad;
        assert!(read(&wrong).is_err(), "{wrong}");
    }

    let mut unknown = full;
    unknown["order_extra"] = json!(1);
    assert!(read(&unknown).is_err());
}

/// Catches: a digest grammar that lets uppercase, a wrong length or a non-hex character
/// through, or refuses a good one.
#[test]
fn a_package_digest_is_64_lowercase_hex_characters() {
    assert_eq!(PackageDigest::parse(DIGEST).expect("good").as_str(), DIGEST);
    assert_eq!(PackageDigest::parse(&DIGEST[1..]), None);
    assert_eq!(PackageDigest::parse(&format!("{DIGEST}0")), None);
    assert_eq!(PackageDigest::parse(&DIGEST.to_uppercase()), None);
    assert_eq!(PackageDigest::parse(&"g".repeat(64)), None);
    assert_eq!(
        String::from(PackageDigest::parse(DIGEST).expect("good")),
        DIGEST
    );
}

fn heartbeat(features: &[&str]) -> Vec<u8> {
    let entry = |name: &str| json!({"name": name, "version": "1.0", "health": "healthy"});
    serde_json::to_vec(&json!({
        "platform": "linux-x86_64",
        "version": "1.0",
        "health": "healthy",
        "agent_features": features,
        "services": [entry("db"), entry("kbf")],
    }))
    .expect("json")
}

/// Catches: the legacy name applied to a heartbeat whose top level is the agent itself (its
/// `kbf` entry would be dropped and the agent answered as a legacy-only host), and the name
/// ignored for every other agent (its top level would be counted twice).
#[test]
fn an_order_v1_heartbeat_keeps_a_list_naming_the_legacy_service() {
    let kept = read_heartbeat(&heartbeat(&[ORDER_FEATURE]), Some("kbf")).expect("read");
    let names: Vec<&str> = kept
        .beat
        .services
        .iter()
        .flatten()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["db", "kbf"]);
    assert_eq!(kept.services_dropped, None);

    for features in [&[][..], &["halt-v1"][..]] {
        let dropped = read_heartbeat(&heartbeat(features), Some("kbf")).expect("read");
        assert_eq!(dropped.beat.services, None);
        assert_eq!(
            dropped.services_dropped.as_deref(),
            Some("kbf is the top level of a heartbeat, not a service")
        );
    }
}
