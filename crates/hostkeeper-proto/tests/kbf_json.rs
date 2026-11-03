//! The heartbeat and its reply lose nothing of the JSON kbf's agents and coordinator speak today.
//!
//! `tests/fixtures/kbf` holds heartbeats and replies recorded from kbf's own wire tests, with
//! nothing that names a host. Every key of them, at every depth, has a field in
//! `hostkeeper.agent.v1` through the table below (the mapping the coordinator's JSON adapter
//! follows), and every enum spelling in them has a value of that field's enum. A key or a
//! spelling the table does not carry fails the test, and every entry of the table is met by some
//! fixture, so the table cannot carry a key kbf never sends.

use std::collections::BTreeSet;

use prost_reflect::{DescriptorPool, FieldDescriptor, Kind, MessageDescriptor};
use serde_json::Value;

/// A service's facts, as kbf writes them at the top level (for its legacy service) and in each
/// entry of `services`: the JSON key, and the field of `ServiceBeat`.
const SERVICE_FACTS: &[(&str, &str)] = &[
    ("version", "version"),
    ("health", "health"),
    ("last_update", "last_update"),
    ("last_update.target", "last_update.target"),
    ("last_update.outcome", "last_update.outcome"),
    ("last_update.phase", "last_update.phase"),
    ("last_update.class", "last_update.class"),
    ("last_update.reason", "last_update.reason"),
    ("updating", "updating"),
    ("step", "step"),
    ("step.name", "step.name"),
    ("step.attempt", "step.attempt"),
    ("commit", "commit"),
    ("step_secs", "step_secs"),
    ("promotion_pending", "promotion_pending"),
    ("deferred", "deferred"),
    ("deferred.target", "deferred.target"),
    ("deferred.reason", "deferred.reason"),
    ("deferred.retry_after_secs", "deferred.retry_after_secs"),
    ("pinned", "pinned"),
    ("pinned.commit", "pinned.commit"),
];

/// The heartbeat's own keys: the JSON key, and the field of `HeartbeatRequest`.
const HEARTBEAT: &[(&str, &str)] = &[
    ("platform", "platform"),
    ("agent", "agent"),
    ("agent.name", "agent.name"),
    ("agent.version", "agent.version"),
    ("agent_features", "features"),
    ("services", "services"),
    ("services[].name", "services.name"),
];

/// The reply's keys: the JSON key, and the field of `HeartbeatResponse`. `reply` is `nothing`
/// (no `order`) or `update` (an `order`).
const REPLY: &[(&str, &str)] = &[
    ("reply", "order"),
    ("service", "order.service"),
    ("version", "order.version"),
    ("features", "features"),
    ("halts", "halt_generations"),
];

/// Keys whose values are maps of names to numbers: their keys are data, not fields.
const MAPS: [&str; 2] = ["step_secs", "halts"];

/// The field of `HeartbeatRequest` a heartbeat's JSON key is carried by: a fact of the legacy
/// service at the top level, and of the listed service in an entry of `services`, both in
/// `services`.
fn heartbeat_field(key: &str) -> Option<String> {
    let fact = key.strip_prefix("services[].").unwrap_or(key);
    let as_fact = SERVICE_FACTS
        .iter()
        .find(|(json, _)| *json == fact)
        .map(|(_, proto)| format!("services.{proto}"));
    as_fact.or_else(|| lookup(HEARTBEAT, key))
}

fn reply_field(key: &str) -> Option<String> {
    lookup(REPLY, key)
}

fn lookup(table: &[(&str, &str)], key: &str) -> Option<String> {
    table
        .iter()
        .find(|(json, _)| *json == key)
        .map(|(_, proto)| (*proto).to_owned())
}

/// Every key of `value` with its path (`services[].step.name`) and value, maps not entered.
fn keys<'a>(value: &'a Value, path: &str, out: &mut Vec<(String, &'a Value)>) {
    let Value::Object(object) = value else {
        return;
    };
    for (key, inner) in object {
        let at = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        out.push((at.clone(), inner));
        if MAPS.contains(&key.as_str()) {
            continue;
        }
        match inner {
            Value::Array(items) => {
                for item in items {
                    keys(item, &format!("{at}[]"), out);
                }
            }
            other => keys(other, &at, out),
        }
    }
}

/// The field a dotted path names, from `message`.
fn field_at(message: &MessageDescriptor, path: &str) -> Option<FieldDescriptor> {
    let mut at = message.clone();
    let mut found = None;
    for part in path.split('.') {
        let field = at.get_field_by_name(part)?;
        if let Kind::Message(inner) = field.kind() {
            at = inner;
        }
        found = Some(field);
    }
    found
}

/// What of `json` the message does not carry, sorted: keys with no field (`surprise`), and enum
/// spellings with no value (`health=sleepy`). Also records the table entries the keys met.
fn uncarried(
    json: &Value,
    message: &MessageDescriptor,
    field_of: fn(&str) -> Option<String>,
    met: &mut BTreeSet<String>,
) -> Vec<String> {
    let mut found = Vec::new();
    keys(json, "", &mut found);
    let mut out = Vec::new();
    for (key, value) in found {
        let Some(path) = field_of(&key) else {
            out.push(key);
            continue;
        };
        let field = field_at(message, &path)
            .unwrap_or_else(|| panic!("{key} maps to {path}, which is no field"));
        met.insert(key.replace("services[].", ""));
        if let (Kind::Enum(e), Value::String(text)) = (field.kind(), value) {
            let name = format!(
                "{}_{}",
                screaming(e.name()),
                text.to_ascii_uppercase().replace('-', "_")
            );
            if e.get_value_by_name(&name).is_none() {
                out.push(format!("{key}={text}"));
            }
        }
    }
    out.sort();
    out
}

/// `FailurePhase` as `FAILURE_PHASE`.
fn screaming(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

fn pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

fn message(name: &str) -> MessageDescriptor {
    pool()
        .get_message_by_name(&format!("hostkeeper.agent.v1.{name}"))
        .expect("the message")
}

/// The `json` of each fixture in `tests/fixtures/kbf/<file>`.
fn fixtures(file: &str) -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kbf")
        .join(file);
    let text = std::fs::read_to_string(&path).expect("read the fixtures");
    let Value::Array(list) = serde_json::from_str(&text).expect("the fixtures parse") else {
        panic!("{} holds no list", path.display());
    };
    list.into_iter()
        .map(|f| f.get("json").cloned().expect("a fixture has json"))
        .collect()
}

/// Every JSON key of a table, as a fixture meets it (entries of `services` folded onto the
/// facts).
fn table_keys(tables: &[&[(&str, &str)]]) -> BTreeSet<String> {
    tables
        .iter()
        .flat_map(|t| t.iter().map(|(json, _)| json.replace("services[].", "")))
        .collect()
}

/// Catches: a field of kbf's heartbeat with no field in `HeartbeatRequest`, an enum spelling
/// with no value, and a table that maps a key onto a field that does not exist. The fixture with
/// an unknown key, and the one with an unknown spelling, prove the check is live.
#[test]
fn heartbeat_carries_every_field_of_recorded_kbf_heartbeats() {
    let beat = message("HeartbeatRequest");
    let mut met = BTreeSet::new();
    let recorded = fixtures("heartbeats.json");
    assert!(recorded.len() >= 8, "only {} fixtures", recorded.len());
    for json in &recorded {
        assert_eq!(
            uncarried(json, &beat, heartbeat_field, &mut met),
            Vec::<String>::new(),
            "{json}"
        );
    }
    assert_eq!(
        met,
        table_keys(&[SERVICE_FACTS, HEARTBEAT]),
        "a table key no fixture has"
    );

    let mut ignored = BTreeSet::new();
    let unknown: Value = serde_json::from_str(
        r#"{"platform": "linux-x86_64", "version": "1.0", "health": "healthy", "surprise": 1,
            "services": [{"name": "db", "version": "1.0", "health": "sleepy", "bogus": 1}]}"#,
    )
    .expect("parses");
    assert_eq!(
        uncarried(&unknown, &beat, heartbeat_field, &mut ignored),
        ["services[].bogus", "services[].health=sleepy", "surprise"]
    );
}

/// Catches: a field of kbf's reply with no field in `HeartbeatResponse`, and a table entry no
/// recorded reply has.
#[test]
fn reply_carries_every_field_of_recorded_kbf_replies() {
    let reply = message("HeartbeatResponse");
    let mut met = BTreeSet::new();
    for json in &fixtures("answers.json") {
        assert_eq!(
            uncarried(json, &reply, reply_field, &mut met),
            Vec::<String>::new(),
            "{json}"
        );
    }
    assert_eq!(met, table_keys(&[REPLY]));

    let mut ignored = BTreeSet::new();
    let unknown: Value =
        serde_json::from_str(r#"{"reply": "update", "version": "2.0", "url": "x"}"#)
            .expect("parses");
    assert_eq!(
        uncarried(&unknown, &reply, reply_field, &mut ignored),
        ["url"]
    );
}

/// Catches: a table that maps a key onto no field, which the fixture checks would report as a
/// panic rather than a gap.
#[test]
fn every_table_entry_names_a_field() {
    let beat = message("HeartbeatRequest");
    for (json, _) in SERVICE_FACTS.iter().chain(HEARTBEAT) {
        let path = heartbeat_field(json).expect("in the table");
        assert!(field_at(&beat, &path).is_some(), "{json} -> {path}");
    }
    let reply = message("HeartbeatResponse");
    for (json, proto) in REPLY {
        assert!(field_at(&reply, proto).is_some(), "{json} -> {proto}");
    }
    assert!(field_at(&beat, "services.nothing").is_none());
}
