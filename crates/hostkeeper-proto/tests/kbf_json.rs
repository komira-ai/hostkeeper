//! The heartbeat and its reply lose nothing of the JSON kbf's agents and coordinator speak today.
//!
//! `tests/fixtures/kbf` holds heartbeats and replies recorded from kbf's own wire tests, with
//! nothing that names a host. Every key of them, at every depth, has a field in
//! `hostkeeper.agent.v1` through the table below (the mapping the coordinator's JSON adapter
//! follows), every value fits its field (the JSON type, one value or a list or a map, and a
//! number's range), every enum spelling in them has a value of that field's enum, and `reply` is
//! one of the discriminators the reply carries as the presence of `order`. A key or a
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
    ("last_update.started_unix", "last_update.started_unix"),
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
    ("forced", "order.forced"),
];

/// Keys whose values kbf writes as enum spellings: their field must be an enum, so a field made a
/// string, which would take any spelling, is refused (`services[]` entries folded onto the facts).
const ENUM_KEYS: [&str; 6] = [
    "platform",
    "health",
    "last_update.outcome",
    "last_update.phase",
    "last_update.class",
    "commit",
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

/// The field a dotted path names, from `message`. A path that goes on past a field that is not a
/// message names nothing.
fn field_at(message: &MessageDescriptor, path: &str) -> Option<FieldDescriptor> {
    let mut at = Some(message.clone());
    let mut found = None;
    for part in path.split('.') {
        let field = at?.get_field_by_name(part)?;
        at = match field.kind() {
            Kind::Message(inner) => Some(inner),
            _ => None,
        };
        found = Some(field);
    }
    found
}

/// The values a JSON discriminator may take, where the proto carries it as the presence of a
/// message rather than as a value: `reply` is `nothing` (no `order`) or `update` (an `order`).
const DISCRIMINATORS: &[(&str, &[&str])] = &[("reply", &["nothing", "update"])];

/// A field's type as a problem names it: `uint32`, `enum <name>`, `repeated string`, `map`.
fn shape(field: &FieldDescriptor) -> String {
    let base = match field.kind() {
        Kind::Message(m) => format!("message {}", m.full_name()),
        Kind::Enum(e) => format!("enum {}", e.full_name()),
        scalar => format!("{scalar:?}"),
    };
    if field.is_map() {
        "map".to_owned()
    } else if field.is_list() {
        format!("repeated {base}")
    } else {
        base
    }
}

/// Whether one JSON value fits a field's element kind: the JSON type, and a number's range.
fn fits(kind: &Kind, value: &Value) -> bool {
    match (kind, value) {
        (Kind::String | Kind::Enum(_), Value::String(_)) | (Kind::Bool, Value::Bool(_)) => true,
        (Kind::Uint32, Value::Number(n)) => n.as_u64().is_some_and(|n| u32::try_from(n).is_ok()),
        (Kind::Uint64, Value::Number(n)) => n.as_u64().is_some(),
        (Kind::Message(_), Value::Object(_)) => true,
        _ => false,
    }
}

/// Whether a JSON value fits a field: its cardinality (a map, a list, one value), and each
/// element's type and range. `null` is an absent field, and fits any.
fn value_fits(field: &FieldDescriptor, value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    if field.is_map() {
        let Kind::Message(entry) = field.kind() else {
            return false;
        };
        let value_kind = entry.map_entry_value_field().kind();
        return value
            .as_object()
            .is_some_and(|o| o.values().all(|v| fits(&value_kind, v)));
    }
    if field.is_list() {
        return value
            .as_array()
            .is_some_and(|items| items.iter().all(|v| fits(&field.kind(), v)));
    }
    fits(&field.kind(), value)
}

/// What of `json` the message does not carry, sorted: keys with no field (`surprise`), values the
/// field cannot hold (`attempt: -1 is not uint32`), enum spellings with no value
/// (`health=sleepy`) and discriminators with no meaning (`reply=reboot`). Also records the table
/// entries the keys met.
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
        if let Some((_, allowed)) = DISCRIMINATORS.iter().find(|(k, _)| *k == key) {
            let text = value.as_str().unwrap_or_default();
            if !allowed.contains(&text) {
                out.push(format!("{key}={value}"));
            }
            continue;
        }
        let enum_key = ENUM_KEYS.contains(&key.replace("services[].", "").as_str());
        if enum_key && !matches!(field.kind(), Kind::Enum(_)) {
            out.push(format!(
                "{key}: {value} is not an enum, but {}",
                shape(&field)
            ));
            continue;
        }
        if !value_fits(&field, value) {
            out.push(format!("{key}: {value} is not {}", shape(&field)));
            continue;
        }
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
    // A path that goes on past a scalar names nothing.
    assert!(field_at(&beat, "platform.services.name").is_none());
    assert!(field_at(&beat, "services.version.name").is_none());
}

/// One fixture file, compiled from its text alone.
struct One(&'static str);

impl protox::file::FileResolver for One {
    fn open_file(&self, name: &str) -> Result<protox::file::File, protox::Error> {
        protox::file::File::from_source(name, self.0)
    }
}

/// The message `t.v1.T` of a fixture that has one field of each shape the heartbeat uses.
fn shapes() -> MessageDescriptor {
    let text = r#"syntax = "proto3";
package t.v1;
enum Health { HEALTH_UNSPECIFIED = 0; HEALTH_OK = 1; }
message T {
  string s = 1;
  uint32 u = 2;
  repeated string r = 3;
  map<string, uint32> step_secs = 4;
  bool b = 5;
  T t = 6;
  Health h = 7;
  uint64 w = 8;
}
"#;
    let mut compiler = protox::Compiler::with_file_resolver(One(text));
    compiler.open_file("t.proto").expect("the fixture compiles");
    compiler
        .descriptor_pool()
        .get_message_by_name("t.v1.T")
        .expect("the message")
}

fn same(key: &str) -> Option<String> {
    Some(key.to_owned())
}

/// Catches: a field kbf writes as an enum spelling made a string, which would take any spelling
/// and so pass the spelling check; in the legacy service's facts and in a service entry.
#[test]
fn an_enum_fact_carried_by_a_string_is_refused() {
    let text = r#"syntax = "proto3";
package x.v1;
enum Platform { PLATFORM_UNSPECIFIED = 0; PLATFORM_LINUX_X86_64 = 1; }
message ServiceBeat { string name = 1; string version = 2; string health = 3; }
message HeartbeatRequest { Platform platform = 1; repeated ServiceBeat services = 4; }
"#;
    let mut compiler = protox::Compiler::with_file_resolver(One(text));
    compiler.open_file("x.proto").expect("the fixture compiles");
    let beat = compiler
        .descriptor_pool()
        .get_message_by_name("x.v1.HeartbeatRequest")
        .expect("the message");
    let json: Value = serde_json::from_str(
        r#"{"platform": "linux-x86_64", "version": "1.0", "health": "healthy",
            "services": [{"name": "db", "version": "1.0", "health": "sleepy"}]}"#,
    )
    .expect("parses");
    let mut met = BTreeSet::new();
    assert_eq!(
        uncarried(&json, &beat, heartbeat_field, &mut met),
        [
            r#"health: "healthy" is not an enum, but string"#,
            r#"services[].health: "sleepy" is not an enum, but string"#,
        ]
    );
}

/// Catches: a value check that holds only key names: a scalar type, a narrower number, a list or
/// a map where kbf sends another shape, and a reply discriminator with no meaning.
#[test]
fn the_value_check_refuses_wrong_types_cardinality_ranges_and_discriminators() {
    let t = shapes();
    let mut met = BTreeSet::new();
    let good: Value = serde_json::from_str(
        r#"{"s": "x", "u": 4294967295, "r": ["x"], "step_secs": {"a": 1}, "b": true,
            "t": {"s": "y", "t": null}, "h": "ok", "w": 18446744073709551615}"#,
    )
    .expect("parses");
    assert_eq!(uncarried(&good, &t, same, &mut met), Vec::<String>::new());

    let bad: Value = serde_json::from_str(
        r#"{"s": 1, "u": 4294967296, "r": "x", "step_secs": {"a": -1, "b": 2}, "b": "yes",
            "t": 3, "h": 2, "w": -1}"#,
    )
    .expect("parses");
    assert_eq!(
        uncarried(&bad, &t, same, &mut met),
        [
            r#"b: "yes" is not bool"#,
            "h: 2 is not enum t.v1.Health",
            r#"r: "x" is not repeated string"#,
            r#"s: 1 is not string"#,
            r#"step_secs: {"a":-1,"b":2} is not map"#,
            "t: 3 is not message t.v1.T",
            "u: 4294967296 is not uint32",
            "w: -1 is not uint64",
        ]
    );

    let reply = message("HeartbeatResponse");
    for unknown in [r#"{"reply": "reboot"}"#, r#"{"reply": 1}"#] {
        let json: Value = serde_json::from_str(unknown).expect("parses");
        let reply_value = json.get("reply").expect("a reply").to_string();
        assert_eq!(
            uncarried(&json, &reply, reply_field, &mut met),
            [format!("reply={reply_value}")]
        );
    }
}
