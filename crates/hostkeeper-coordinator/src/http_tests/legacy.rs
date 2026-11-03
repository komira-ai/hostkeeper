//! Over HTTP: what the absence of a legacy service changes (a heartbeat with no services, a
//! convergence read with no query and a desired state with no service are bad requests), what
//! an entry without services stands for, and the agent in the status.

use super::*;

use crate::services::Services;
use crate::testutil::with_kbf;

const OTHER_SERVICE: &str = "[[service]]\nname = \"other\"\n[service.roles.daemon]\nmin_up = 0\n";

const LISTS_OTHER: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[{"name":"other","version":"1.0","health":"healthy"}]}"#;

/// A coordinator with no legacy service over `other` alone, whose entry names `other`.
fn without_legacy(name: &str) -> Rig {
    let mut rig = Rig::new(name);
    let list = format!(
        "{}services = [\"other\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(Services::parse(OTHER_SERVICE).expect("services"))
        .with_legacy_service(None);
    rig
}

/// Catches: a heartbeat that belongs to no service being answered: with no legacy service a
/// heartbeat with no `services` is a 400 `bad_request` and its host is not noted, and so is
/// one whose list is bad and dropped; a heartbeat that lists a service is a 200.
#[tokio::test]
async fn without_a_legacy_service_a_heartbeat_listing_no_services_is_a_400() {
    let rig = without_legacy("http-no-legacy-beat");
    rig.at(2_000);
    let who = updater("d1", "daemon");
    let twice = LISTS_OTHER.replace(
        r#"[{"name":"other","version":"1.0","health":"healthy"}]"#,
        r#"[{"name":"other","version":"1.0","health":"healthy"},{"name":"other","version":"1.0","health":"healthy"}]"#,
    );
    for body in [BEAT, twice.as_str()] {
        let (code, text) = call(
            &rig,
            Surface::Heartbeat,
            &who,
            request("POST", HEARTBEAT_PATH, None, body),
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(text, json!({"error": "bad_request"}));
    }
    assert!(
        status(&rig).await["per_service"]["other"]["hosts"]
            .as_array()
            .expect("hosts")
            .is_empty()
    );
    let (code, text) = call(
        &rig,
        Surface::Heartbeat,
        &who,
        request("POST", HEARTBEAT_PATH, None, LISTS_OTHER),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(text, json!({"reply": "nothing"}));
    let seen = status(&rig).await;
    assert!(hosts(&seen).is_empty(), "the top level is nobody's");
    assert_eq!(seen["per_service"]["other"]["hosts"][0]["node_id"], "d1");
}

/// Catches: a convergence read with no query guessing a service: with no legacy service it is
/// a 400 `bad_request`, while `?service=other` is answered and a service nobody manages is a
/// 404.
#[tokio::test]
async fn without_a_legacy_service_a_convergence_read_needs_a_service() {
    let rig = without_legacy("http-no-legacy-converge");
    let read = |path: &'static str| {
        call(
            &rig,
            Surface::Control,
            &None,
            request("GET", path, Some(&ci_token()), ""),
        )
    };
    let (code, body) = read(CONVERGENCE_PATH).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "bad_request"}));
    let (code, body) = read("/v1/convergence?service=other").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["state"], "in_progress");
    let (code, body) = read("/v1/convergence?service=kbf").await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "not_found"}));
}

/// Catches: a desired state with no `service` being taken for a service's when there is no
/// legacy service: it is a 400 `bad_request` that stores nothing, and a body naming a service
/// the entry names is a 200.
#[tokio::test]
async fn without_a_legacy_service_a_desired_state_needs_a_service() {
    let rig = without_legacy("http-no-legacy-set");
    let (code, body) = call(
        &rig,
        Surface::Control,
        &None,
        request("POST", DESIRED_PATH, Some(&ci_token()), STATE),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "bad_request"}));
    assert_eq!(status(&rig).await["services"], json!({}));
    let named = r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#;
    let (code, _) = call(
        &rig,
        Surface::Control,
        &None,
        request("POST", DESIRED_PATH, Some(&ci_token()), named),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let seen = status(&rig).await;
    assert_eq!(seen["desired"], Value::Null);
    assert_eq!(seen["services"]["other"]["version"], "3.0");
}

/// Catches: an entry without `services` admitting a service besides the legacy one over
/// HTTP: it sets a body with no `service` (the legacy service's) and is a 403 `forbidden` for
/// `other`.
#[tokio::test]
async fn an_entry_without_services_sets_the_legacy_service_and_no_other() {
    let mut rig = Rig::new("http-entry-legacy");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(with_kbf(OTHER_SERVICE));
    let (code, _) = call(
        &rig,
        Surface::Control,
        &None,
        request("POST", DESIRED_PATH, Some(&ci_token()), STATE),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let other = r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#;
    let (code, body) = call(
        &rig,
        Surface::Control,
        &None,
        request("POST", DESIRED_PATH, Some(&ci_token()), other),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": "forbidden"}));
    let seen = status(&rig).await;
    assert_eq!(seen["desired"]["version"], "2.0");
    assert_eq!(seen["services"].as_object().expect("services").len(), 1);
}

/// Catches: the agent of a host missing from the status: a heartbeat without `agent` shows
/// `{"name":"kbf-updater"}` and one with `agent` shows its name and version, in the host
/// list and in the hosts of the service.
#[tokio::test]
async fn the_status_shows_the_agent_a_heartbeat_names_and_the_legacy_one_otherwise() {
    let rig = Rig::new("http-agent");
    rig.at(2_000);
    let with_agent = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent":{"name":"hostkeeper-updater","version":"0.3.1"}}"#;
    for (name, body) in [("d1", BEAT), ("d2", with_agent)] {
        let (code, _) = call(
            &rig,
            Surface::Heartbeat,
            &updater(name, "daemon"),
            request("POST", HEARTBEAT_PATH, None, body),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
    }
    let seen = status(&rig).await;
    for list in [&seen["hosts"], &seen["per_service"]["kbf"]["hosts"]] {
        assert_eq!(list[0]["agent"], json!({"name": "kbf-updater"}));
        assert_eq!(
            list[1]["agent"],
            json!({"name": "hostkeeper-updater", "version": "0.3.1"})
        );
    }
}

/// Catches: a malformed `agent` costing the host its heartbeat: the heartbeat is a 200, the
/// host counts as the legacy agent, and the dropped fact is counted under `agent`.
#[tokio::test]
async fn a_malformed_agent_is_dropped_and_counted_and_the_host_is_still_served() {
    let rig = Rig::new("http-agent-bad");
    rig.at(2_000);
    let bad = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent":{"name":"Not An Agent","version":"0.3.1"}}"#;
    let (code, _) = call(
        &rig,
        Surface::Heartbeat,
        &updater("d1", "daemon"),
        request("POST", HEARTBEAT_PATH, None, bad),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let seen = status(&rig).await;
    assert_eq!(seen["hosts"][0]["agent"], json!({"name": "kbf-updater"}));
    assert_eq!(seen["extras_dropped"]["agent"], 1);
}
