//! No service is built in: the services a coordinator manages are the ones its services file
//! defines, and nothing else. kbf and tierstore are names like any other.

mod support;

use hostkeeper_coordinator::services::Services;
use serde_json::json;
use support::rig::{Rig, Spec};
use support::{BEAT, recorded};

fn beat_listing(names: &[&str]) -> String {
    let services: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"name":"{n}","version":"1.0","health":"healthy"}}"#))
        .collect();
    format!(
        r#"{{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[{}]}}"#,
        services.join(",")
    )
}

/// Proves: with an empty services file (and so no legacy service) the coordinator manages
/// nothing and knows neither kbf nor tierstore: the status lists no service, the convergence of
/// any name is 404, a heartbeat that lists them is answered nothing and noted nowhere, a desired
/// state or an operator action that names them is refused, and nothing is stored. With a file
/// that defines tierstore alone, kbf is still unknown and tierstore is read; the default of the
/// services type is empty.
#[tokio::test]
async fn no_service_name_is_built_in() {
    assert_eq!(Services::default().names().count(), 0);
    assert_eq!(
        Services::parse("").expect("an empty file").names().count(),
        0
    );

    let spec = Spec::recorded()
        .with_services("")
        .without_legacy()
        .with_oidc_services(&["kbf", "tierstore"]);
    let rig = Rig::new("no-builtin", spec);
    rig.at(1_100);
    let status = rig.status().await;
    assert_eq!(status["per_service"], json!({}));
    assert_eq!(status["services"], json!({}));
    assert_eq!(status["hosts"], json!([]));
    assert_eq!(status["desired"], serde_json::Value::Null);
    let not_found = r#"{"error":"not_found"}"#;
    for name in ["kbf", "tierstore"] {
        let (code, body) = rig.convergence(Some(&format!("service={name}"))).await;
        assert_eq!((code, body.as_str()), (404, not_found), "{name}");
    }
    let (code, _) = rig.convergence(None).await;
    assert_eq!(
        code, 400,
        "no query names the legacy service, and there is none"
    );

    let (code, body) = rig
        .heartbeat("d1", "daemon", &beat_listing(&["kbf", "tierstore"]))
        .await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));
    let (code, _) = rig.heartbeat("d1", "daemon", BEAT).await;
    assert_eq!(code, 400, "a heartbeat with no services lists no service");
    let status = rig.status().await;
    assert_eq!(status["per_service"], json!({}));
    assert_eq!(status["hosts"], json!([]));

    let bad = r#"{"error":"bad_request"}"#;
    for name in ["kbf", "tierstore"] {
        let body = format!(
            r#"{{"service":"{name}","version":"2.0","min_up":{{"storage":1,"frontend":1}}}}"#
        );
        let (code, answer) = rig.desired(&body).await;
        assert_eq!(
            (code, answer.as_str()),
            (400, bad),
            "a desired state for {name}"
        );
        let action = format!(
            r#"{{"action":"exclude","service":"{name}","node_id":"s1","reason":"bad disk"}}"#
        );
        let (code, answer) = rig.operator(&action).await;
        assert_eq!(
            (code, answer.as_str()),
            (400, bad),
            "an operator action on {name}"
        );
    }
    assert_eq!(rig.status().await["services"], json!({}));
    assert!(
        !rig.dir.path().join("desired.json").exists(),
        "nothing was stored"
    );
    assert!(
        !rig.dir.path().join("desired.json.ops").exists(),
        "nothing was changed"
    );

    let tier_only = recorded("tierstore.services.toml");
    let spec = Spec::recorded()
        .with_services(&tier_only)
        .without_legacy()
        .with_oidc_services(&["tierstore"]);
    let rig = Rig::new("no-builtin-tier", spec);
    rig.at(1_100);
    let (code, body) = rig.convergence(Some("service=kbf")).await;
    assert_eq!(
        (code, body.as_str()),
        (404, not_found),
        "kbf is no more built in than tierstore"
    );
    let (code, body) = rig.convergence(Some("service=tierstore")).await;
    assert_eq!(code, 200, "{body}");
    let status = rig.status().await;
    let names: Vec<&String> = status["per_service"]
        .as_object()
        .expect("services")
        .keys()
        .collect();
    assert_eq!(names, ["tierstore"]);
}
