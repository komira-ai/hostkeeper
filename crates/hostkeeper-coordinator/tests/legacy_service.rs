//! The legacy service: the one service whose heartbeat is the top level of a heartbeat, named by
//! `--legacy-service`. Nothing in the coordinator names one: kbf is only what the recorded flags
//! say.

mod support;

use serde_json::{Value, json};
use support::rig::{Rig, Spec};

/// Two services that run on daemons, and nothing else: neither is special.
const TWO: &str = "[[service]]\nname = \"alpha\"\nroles = [\"daemon\"]\n\n\
                   [[service]]\nname = \"beta\"\nroles = [\"daemon\"]\n";

const BEAT: &str = support::BEAT;

fn beat_listing(version: &str, service: &str) -> String {
    format!(
        r#"{{"platform":"linux-x86_64","version":"{version}","health":"healthy","services":[{{"name":"{service}","version":"1.0","health":"healthy"}}]}}"#
    )
}

fn hosts_of(status: &Value, service: &str) -> Vec<String> {
    status["per_service"][service]["hosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .map(|h| h["node_id"].as_str().expect("an id").to_owned())
        .collect()
}

/// Proves: the top-level fields of a heartbeat are the heartbeat of the service the flag names,
/// whichever it is. With `--legacy-service` set to either of two services, a body of a desired
/// state with no `service` sets that service's, a convergence read with no query reads its,
/// a host that sends no `services` is a host of it and is told its updates in the old shape (no
/// service named), and the other service has no host and no state; a service the host lists
/// besides is told by name.
#[tokio::test]
async fn legacy_single_service_heartbeat_belongs_to_the_named_service() {
    for (legacy, other) in [("alpha", "beta"), ("beta", "alpha")] {
        let spec = Spec::recorded()
            .with_services(TWO)
            .with_legacy(legacy)
            .with_oidc_services(&["alpha", "beta"]);
        let rig = Rig::new("legacy-named", spec);
        rig.at(1_100);
        let (code, body) = rig.desired(r#"{"version":"2.0"}"#).await;
        assert_eq!(code, 200, "{legacy}: {body}");
        let status = rig.status().await;
        assert_eq!(status["desired"]["version"], "2.0", "{legacy}");
        assert_eq!(
            status["services"].as_object().map(|s| s.len()),
            Some(1),
            "{legacy}"
        );
        assert_eq!(status["services"][legacy]["version"], "2.0", "{legacy}");
        assert_eq!(
            status["per_service"][other]["desired"],
            Value::Null,
            "{legacy}"
        );

        let (code, body) = rig.heartbeat("d1", "daemon", BEAT).await;
        assert_eq!(code, 200);
        assert_eq!(
            body, r#"{"reply":"update","version":"2.0"}"#,
            "no service named: {legacy}"
        );
        let status = rig.status().await;
        assert_eq!(hosts_of(&status, legacy), ["d1"], "{legacy}");
        assert!(hosts_of(&status, other).is_empty(), "{legacy}");
        assert_eq!(status["hosts"][0]["node_id"], "d1");

        let (code, body) = rig.convergence(None).await;
        assert_eq!(code, 200);
        assert!(body.contains(r#""desired":"2.0""#), "{legacy}: {body}");
        let (code, body) = rig.convergence(Some(&format!("service={other}"))).await;
        assert_eq!(code, 200);
        assert!(body.contains(r#""desired":null"#), "{legacy}: {body}");

        let (code, _) = rig
            .desired(&format!(r#"{{"service":"{other}","version":"3.0"}}"#))
            .await;
        assert_eq!(code, 200);
        let (_, done) = rig
            .heartbeat("d1", "daemon", &BEAT.replace("1.0", "2.0"))
            .await;
        assert_eq!(done, r#"{"reply":"nothing"}"#, "{legacy}: d1 is done");
        let (code, named) = rig
            .heartbeat("d2", "daemon", &beat_listing("2.0", other))
            .await;
        assert_eq!(code, 200);
        assert_eq!(
            named,
            format!(r#"{{"reply":"update","service":"{other}","version":"3.0"}}"#),
            "{legacy}: the other service is named"
        );
    }
}

/// Proves: with no legacy service a heartbeat that lists no `services` (none sent, or a list so
/// bad that it is dropped) is refused `400 bad_request` and noted nowhere, because its top level
/// belongs to no service; one that lists services is answered, and only those count. A desired
/// state with no `service` is refused too, and so is a convergence read with no query.
#[tokio::test]
async fn without_legacy_service_such_a_heartbeat_is_refused() {
    let spec = Spec::recorded()
        .with_services(TWO)
        .without_legacy()
        .with_oidc_services(&["alpha", "beta"]);
    let rig = Rig::new("legacy-none", spec);
    rig.at(1_100);
    let bad = r#"{"error":"bad_request"}"#;

    let (code, body) = rig.heartbeat("d1", "daemon", BEAT).await;
    assert_eq!((code, body.as_str()), (400, bad), "no services list");
    let twice = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[
        {"name":"alpha","version":"1.0","health":"healthy"},
        {"name":"alpha","version":"1.0","health":"healthy"}]}"#;
    let (code, body) = rig.heartbeat("d1", "daemon", twice).await;
    assert_eq!((code, body.as_str()), (400, bad), "a list that is dropped");
    let status = rig.status().await;
    assert_eq!(status["hosts"], json!([]));
    assert!(hosts_of(&status, "alpha").is_empty() && hosts_of(&status, "beta").is_empty());
    assert_eq!(
        status["services_dropped"], 1,
        "the dropped list is still counted"
    );

    let (code, body) = rig.desired(r#"{"version":"2.0"}"#).await;
    assert_eq!(
        (code, body.as_str()),
        (400, bad),
        "a desired state names its service"
    );
    let (code, body) = rig.convergence(None).await;
    assert_eq!(
        (code, body.as_str()),
        (400, bad),
        "a read names its service"
    );
    let (code, _) = rig.desired(r#"{"service":"alpha","version":"2.0"}"#).await;
    assert_eq!(code, 200);
    let (code, body) = rig.convergence(Some("service=alpha")).await;
    assert_eq!(code, 200);
    assert!(body.contains(r#""desired":"2.0""#), "{body}");

    let empty = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[]}"#;
    let (code, body) = rig.heartbeat("d1", "daemon", empty).await;
    assert_eq!(
        (code, body.as_str()),
        (200, r#"{"reply":"nothing"}"#),
        "an empty list is a list"
    );

    let (code, body) = rig
        .heartbeat("d1", "daemon", &beat_listing("1.0", "alpha"))
        .await;
    assert_eq!(code, 200);
    assert_eq!(
        body,
        r#"{"reply":"update","service":"alpha","version":"2.0"}"#
    );
    let status = rig.status().await;
    assert_eq!(hosts_of(&status, "alpha"), ["d1"]);
    assert!(hosts_of(&status, "beta").is_empty());
    assert_eq!(
        status["hosts"],
        json!([]),
        "the top level of a heartbeat belongs to no service"
    );
    assert_eq!(status["desired"], Value::Null);
}
