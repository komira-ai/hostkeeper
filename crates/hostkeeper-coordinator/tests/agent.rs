//! The agent a heartbeat names: shown in the status, and counted as the updater that predates the
//! field when it names none.

mod support;

use serde_json::{Value, json};
use support::BEAT;
use support::rig::Rig;

/// The host `id` of `hosts`, a list of the status.
fn host<'a>(hosts: &'a Value, id: &str) -> &'a Value {
    hosts
        .as_array()
        .expect("hosts")
        .iter()
        .find(|h| h["node_id"] == id)
        .unwrap_or_else(|| panic!("no host {id}"))
}

fn with_agent(agent: &str) -> String {
    BEAT.replace('}', &format!(r#","agent":{agent}}}"#))
}

/// Proves: a heartbeat with no `agent` is from the updater that predates the field, shown in the
/// status as the agent `kbf-updater` with no version; one that names its agent is shown with the
/// name and the version it gave, both in the status of the host and in the host of its service;
/// the beat the status flattens into the host does not repeat the key; and an `agent` that is no
/// agent is dropped (the heartbeat is still answered, the host shown as the older updater, and
/// the status counts the fact dropped).
#[tokio::test]
async fn heartbeat_without_agent_counts_as_kbf_updater_and_with_agent_is_shown_in_status() {
    let rig = Rig::recorded("agent-status");
    rig.at(1_100);
    let (code, _) = rig.heartbeat("d1", "daemon", BEAT).await;
    assert_eq!(code, 200);
    let named = with_agent(r#"{"name":"hostkeeper-updater","version":"1.2"}"#);
    let (code, _) = rig.heartbeat("d2", "daemon", &named).await;
    assert_eq!(code, 200);
    let bad = with_agent(r#"{"name":"Bad Name","version":"1.0"}"#);
    let (code, body) = rig.heartbeat("d3", "daemon", &bad).await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));

    let status = rig.status().await;
    let hosts = &status["hosts"];
    assert_eq!(host(hosts, "d1")["agent"], json!({"name": "kbf-updater"}));
    assert_eq!(
        host(hosts, "d2")["agent"],
        json!({"name": "hostkeeper-updater", "version": "1.2"})
    );
    assert_eq!(host(hosts, "d3")["agent"], json!({"name": "kbf-updater"}));
    let service_hosts = &status["per_service"]["kbf"]["hosts"];
    assert_eq!(
        host(service_hosts, "d2")["agent"],
        json!({"name": "hostkeeper-updater", "version": "1.2"})
    );
    assert_eq!(
        host(service_hosts, "d1")["agent"],
        json!({"name": "kbf-updater"})
    );
    assert_eq!(status["extras_dropped"], json!({"agent": 1}));

    // One `agent` key a host in each list, none from the beat that is flattened into it.
    let key = r#""agent":"#;
    assert_eq!(status["hosts"].to_string().matches(key).count(), 3);
    assert_eq!(status["per_service"].to_string().matches(key).count(), 3);
}
