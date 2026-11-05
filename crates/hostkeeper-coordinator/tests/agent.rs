//! The agent a heartbeat names: shown in the status, and counted as the updater that predates the
//! field when it names none.

mod support;

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};
use support::rig::{Rig, Spec};
use support::{BEAT, OPERATOR};

/// How many keys named `agent` whose value is an object (an agent, not the count of a dropped
/// fact) a JSON text holds at any depth, and whether the text itself is an object. Reading the
/// text, not a parsed value, it sees a key said twice in one object (which a parsed value
/// collapses) and refuses it.
struct AgentKeys(usize, bool);

impl<'de> Deserialize<'de> for AgentKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Walk;
        impl<'de> Visitor<'de> for Walk {
            type Value = AgentKeys;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON without a repeated key")
            }
            fn visit_bool<E>(self, _: bool) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_i64<E>(self, _: i64) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_u64<E>(self, _: u64) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_f64<E>(self, _: f64) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_str<E>(self, _: &str) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_unit<E>(self) -> Result<AgentKeys, E> {
                Ok(AgentKeys(0, false))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<AgentKeys, A::Error> {
                let mut total = 0;
                while let Some(AgentKeys(n, _)) = seq.next_element()? {
                    total += n;
                }
                Ok(AgentKeys(total, false))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AgentKeys, A::Error> {
                let mut seen = BTreeSet::new();
                let mut total = 0;
                while let Some(key) = map.next_key::<String>()? {
                    let AgentKeys(n, object) = map.next_value()?;
                    total += n + usize::from(key == "agent" && object);
                    if !seen.insert(key.clone()) {
                        return Err(serde::de::Error::custom(format!("repeated key {key}")));
                    }
                }
                Ok(AgentKeys(total, true))
            }
        }
        deserializer.deserialize_any(Walk)
    }
}

/// The number of `agent` keys holding an object in `text`, which has no repeated key.
fn agent_keys(text: &str) -> usize {
    let AgentKeys(n, _) = serde_json::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"));
    n
}

/// A heartbeat that lists the service `kbf` and, if given, names `agent` (raw JSON).
fn listing(agent: Option<&str>) -> String {
    let extra = agent.map_or_else(String::new, |a| format!(r#","agent":{a}"#));
    format!(
        r#"{{"platform":"linux-x86_64","version":"1.0","health":"healthy","services":[{{"name":"kbf","version":"1.0","health":"healthy"}}]{extra}}}"#
    )
}

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

    // One `agent` key a host in each list, none from the beat that is flattened into it: counted
    // in the bytes the coordinator sent, where a repeated key in one object is an error.
    let (code, raw) = rig.control("GET", "/v1/status", Some(OPERATOR), "").await;
    assert_eq!(code, 200, "{raw}");
    assert_eq!(agent_keys(&raw), 6);
}

/// Proves: the agent of one host, with a service named and no legacy service, follows its last
/// heartbeat: named, it is the name and version given; omitted, it is the updater that predates
/// the field; malformed, it is dropped as the fact `agent` (counted once) and the host shows as
/// that same updater. The status leaves `extras_dropped` out until a fact is dropped. The raw
/// status text holds one `agent` key for the host in the service, none repeated.
/// Catches: an agent that sticks after a beat that names none, a malformed agent that costs the
/// host its heartbeat or is shown as named, and an `agent` key said twice in one object.
#[tokio::test]
async fn one_host_agent_follows_its_beats_named_then_omitted_then_malformed() {
    let spec = Spec::recorded().without_legacy();
    let rig = Rig::new("agent-follows", spec);
    rig.at(1_100);
    let shown =
        |status: &Value| host(&status["per_service"]["kbf"]["hosts"], "d1")["agent"].clone();

    let named = listing(Some(r#"{"name":"hostkeeper-updater","version":"1.2"}"#));
    let (code, _) = rig.heartbeat("d1", "daemon", &named).await;
    assert_eq!(code, 200);
    let status = rig.status().await;
    assert_eq!(
        shown(&status),
        json!({"name": "hostkeeper-updater", "version": "1.2"})
    );
    assert_eq!(status["extras_dropped"], Value::Null);

    let (code, _) = rig.heartbeat("d1", "daemon", &listing(None)).await;
    assert_eq!(code, 200);
    let status = rig.status().await;
    assert_eq!(shown(&status), json!({"name": "kbf-updater"}));
    assert_eq!(status["extras_dropped"], Value::Null);

    let bad = listing(Some(r#"{"name":"Bad Name","version":"1.0"}"#));
    let (code, body) = rig.heartbeat("d1", "daemon", &bad).await;
    assert_eq!((code, body.as_str()), (200, r#"{"reply":"nothing"}"#));
    let status = rig.status().await;
    assert_eq!(shown(&status), json!({"name": "kbf-updater"}));
    assert_eq!(status["extras_dropped"], json!({"agent": 1}));

    let (code, raw) = rig.control("GET", "/v1/status", Some(OPERATOR), "").await;
    assert_eq!(code, 200, "{raw}");
    assert_eq!(agent_keys(&raw), 1);
}

/// Proves: the key counter refuses a repeated key in one object, however deep, and counts
/// `agent` keys in objects and arrays.
/// Catches: a duplicate-key check that is silently satisfied because the text was parsed into a
/// value first.
#[test]
fn agent_key_counter_sees_nested_keys_and_refuses_a_repeated_key() {
    assert_eq!(
        agent_keys(
            r#"{"agent":{"a":[1,-2,1.5,true,null,"x"]},"b":[{"agent":{}}],"c":{"agent":1}}"#
        ),
        2
    );
    let repeated = serde_json::from_str::<AgentKeys>(r#"{"x":[{"agent":1,"agent":2}]}"#);
    assert_eq!(
        repeated
            .err()
            .map(|e| e.to_string().starts_with("repeated key agent")),
        Some(true)
    );
}
