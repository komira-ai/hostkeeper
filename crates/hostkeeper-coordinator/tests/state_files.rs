//! The state files across the two coordinators: what kbf's coordinator wrote is read here, and
//! what this one writes is still read by kbf's, which refuses a key it does not know and a
//! schema newer than its own.
//!
//! The fixtures are kbf v0.0.64's bytes (`fixtures/recorded`). `strict-readers.json` lists, for
//! every struct kbf reads from these files with `deny_unknown_fields`, the keys it allows, the
//! keys it requires and the schema it reads; the checks below mirror those readers, so a file
//! this coordinator writes that kbf would refuse fails here.

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::ops::{OpsError, OpsStore};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use support::rig::{Rig, Spec};
use support::{Flags, recorded, recorded_bytes, scratch};

/// What one of kbf's strict readers takes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reader {
    source: String,
    #[serde(default)]
    schema: Option<u64>,
    allowed: Vec<String>,
    required: Vec<String>,
    nested: BTreeMap<String, String>,
}

/// `strict-readers.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Readers {
    from: String,
    files: BTreeMap<String, String>,
    structs: BTreeMap<String, Reader>,
}

impl Readers {
    fn recorded() -> Self {
        serde_json::from_str(&recorded("strict-readers.json")).expect("strict-readers.json")
    }

    /// Whether kbf's reader `name` takes `value`: no key it does not know, every key it
    /// requires, the schema it reads, and the same of everything nested in it.
    fn check(&self, name: &str, value: &Value, at: &str) -> Result<(), String> {
        let reader = self
            .structs
            .get(name)
            .ok_or_else(|| format!("{at}: no reader named {name}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{at}: {name} is not an object"))?;
        if let Some(key) = object.keys().find(|k| !reader.allowed.contains(k)) {
            return Err(format!(
                "{at}: {key} is a key {name} refuses ({})",
                reader.source
            ));
        }
        if let Some(key) = reader.required.iter().find(|k| !object.contains_key(*k)) {
            return Err(format!("{at}: {name} requires {key} ({})", reader.source));
        }
        if let Some(schema) = reader.schema
            && object.get("schema").and_then(Value::as_u64) != Some(schema)
        {
            return Err(format!("{at}: {name} reads schema {schema} only"));
        }
        for (key, shape) in &reader.nested {
            let Some(inner) = object.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let at = format!("{at}.{key}");
            if let Some(item) = shape.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let items = inner.as_array().ok_or(format!("{at}: not an array"))?;
                for (n, one) in items.iter().enumerate() {
                    self.check(item, one, &format!("{at}[{n}]"))?;
                }
            } else if let Some(item) = shape.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                let map = inner.as_object().ok_or(format!("{at}: not an object"))?;
                for (name, one) in map {
                    self.check(item, one, &format!("{at}.{name}"))?;
                }
            } else {
                self.check(shape, inner, &at)?;
            }
        }
        Ok(())
    }
}

fn parse(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("a file");
    assert!(
        text.ends_with("}\n"),
        "{}: one JSON value and a newline",
        path.display()
    );
    serde_json::from_str(&text).expect("JSON")
}

/// The desired-state file the rig's coordinator has written, checked against kbf's readers: the
/// single state, or the map.
fn check_desired(rig: &Rig, readers: &Readers, reader: &str) {
    let path = rig.dir.path().join("desired.json");
    readers
        .check(reader, &parse(&path), "desired.json")
        .unwrap_or_else(|why| panic!("{why}"));
}

/// The ops file and the halts file the rig's coordinator has written, checked against kbf's
/// readers; the halts file names the digest of the ops file it goes with.
fn check_ops(rig: &Rig, readers: &Readers) {
    let ops = rig.dir.path().join("desired.json.ops");
    let value = parse(&ops);
    readers
        .check("OpsState", &value, "ops")
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(value["schema"], 1);
    let halts = rig.dir.path().join("desired.json.ops.halts");
    if halts.exists() {
        let halts = parse(&halts);
        let mut keys: Vec<&str> = halts
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["ops", "services"], "the halts file's keys");
        let mut digest = String::new();
        for b in Sha256::digest(std::fs::read(&ops).expect("ops file")).iter() {
            write!(digest, "{b:02x}").expect("a string takes text");
        }
        assert_eq!(
            halts["ops"],
            digest.as_str(),
            "the halts file goes with this ops file"
        );
    }
}

fn bytes(rig: &Rig, file: &str) -> Vec<u8> {
    std::fs::read(rig.dir.path().join(file)).unwrap_or_else(|why| panic!("{file}: {why}"))
}

const HALT_BEAT: &str = concat!(
    r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","#,
    r#""last_update":{"target":"2.0","outcome":"failed","phase":"halt","reason":"disk_tainted"}}"#
);

/// kbf's desired-state files (the single state and the map) are read with the recorded flags and
/// written back to the same bytes.
fn kbf_desired_files_are_read_and_rewritten_to_the_same_bytes() {
    let flags = Flags::recorded();
    let legacy = flags.legacy_service.as_deref();
    for (fixture, names) in [
        ("desired-single.json", vec!["kbf"]),
        ("desired-map.json", vec!["kbf", "tierstore"]),
    ] {
        let dir = scratch("state-kbf-desired");
        let path = dir.path().join("desired.json");
        std::fs::write(&path, recorded_bytes(fixture)).expect("written");
        let store = DesiredStore::open(&path, legacy).expect("kbf's file is read");
        let held: Vec<String> = store.all().into_keys().collect();
        assert_eq!(held, names, "{fixture}");
        let kbf = store.get("kbf").expect("kbf's state");
        assert_eq!(kbf.version.to_string(), "2.0");
        assert_eq!(kbf.max_updating.get("daemon"), Some(1));
        assert_eq!(kbf.max_updating.get("mac-daemon"), Some(2));
        assert_eq!(kbf.min_up.get("storage"), Some(1));
        assert_eq!(kbf.min_up.get("frontend"), Some(1));
        if let Some(tier) = store.get("tierstore") {
            assert_eq!(tier.version.to_string(), "7.1");
            assert_eq!(tier.min_up.get("storage"), Some(4));
            assert_eq!(tier.min_up.get("frontend"), Some(0));
        }
        store.set("kbf", kbf).expect("rewritten");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            recorded_bytes(fixture),
            "{fixture} is written back as kbf wrote it"
        );
    }
}

/// kbf's ops file and the halts file beside it are read: the exclusion, the label, the halt with
/// its report and the audit ring; a change made here rewrites both within kbf's readers.
async fn kbf_ops_files_are_read_and_rewritten_within_kbfs_readers(readers: &Readers) {
    let mut rig = Rig::recorded("state-kbf-ops");
    for (fixture, file) in [
        ("desired-single.json", "desired.json"),
        ("ops.json", "desired.json.ops"),
        ("ops.json.halts", "desired.json.ops.halts"),
    ] {
        std::fs::write(rig.dir.path().join(file), recorded_bytes(fixture)).expect("written");
    }
    let store = OpsStore::open(&rig.dir.path().join("desired.json.ops")).expect("kbf's files");
    let state = store.state();
    assert_eq!(state.excluded.len(), 1);
    assert_eq!(
        (
            state.excluded[0].service.as_str(),
            state.excluded[0].node_id.as_str(),
            state.excluded[0].reason.as_str()
        ),
        ("kbf", "s2", "bad disk")
    );
    assert_eq!(state.hosts["s1"].labels["rack"], "r1");
    let halt = state.services["kbf"].halt.clone().expect("the halt");
    assert_eq!(
        (
            halt.node_id.as_str(),
            halt.check.as_str(),
            halt.target.to_string().as_str()
        ),
        ("s1", "disk_tainted", "2.0")
    );
    assert_eq!(
        serde_json::to_value(halt.report.expect("the report of the halts file")).expect("json"),
        json!({"target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"})
    );
    let actions: Vec<&str> = state.audit.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["exclude", "set_labels", "halt"]);

    rig.restart();
    rig.at(1_100);
    let (code, body) = rig.convergence(None).await;
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains(r#""state":"halted""#),
        "kbf's halt stands: {body}"
    );
    let (code, body) = rig
        .operator(r#"{"action":"include","service":"kbf","node_id":"s2"}"#)
        .await;
    assert_eq!(code, 200, "{body}");
    check_ops(&rig, readers);
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert_eq!(ops["excluded"], json!([]));
    assert_eq!(ops["services"]["kbf"]["halt"]["node_id"], "s1");
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    assert_eq!(
        halts["services"]["kbf"]["halt"]["report"]["reason"], "disk_tainted",
        "the halt keeps its report in the halts file"
    );
}

/// What this coordinator writes, step by step, is within kbf's readers; and for the states and
/// the changes kbf's fixtures record it is kbf's bytes.
async fn what_this_coordinator_writes_is_within_kbfs_readers(readers: &Readers) {
    let rig = Rig::new(
        "state-written",
        Spec::recorded().with_oidc_services(&["kbf", "tierstore"]),
    );
    rig.at(1_100);
    assert!(!rig.dir.path().join("desired.json").exists());
    let (code, body) = rig.desired(support::STATE).await;
    assert_eq!(code, 200, "{body}");
    check_desired(&rig, readers, "Desired");
    assert_eq!(
        bytes(&rig, "desired.json"),
        recorded_bytes("desired-single.json")
    );
    assert!(
        !rig.dir.path().join("desired.json.ops").exists(),
        "a farm that changes nothing keeps no ops file"
    );

    let tier = r#"{"service":"tierstore","version":"7.1","min_up":{"storage":4}}"#;
    let (code, body) = rig.desired(tier).await;
    assert_eq!(code, 200, "{body}");
    check_desired(&rig, readers, "MapFile");
    assert_eq!(
        bytes(&rig, "desired.json"),
        recorded_bytes("desired-map.json")
    );

    let exclude = r#"{"action":"exclude","service":"kbf","node_id":"s2","reason":"bad disk"}"#;
    let labels = r#"{"action":"set_labels","node_id":"s1","labels":{"rack":"r1"}}"#;
    for body in [exclude, labels] {
        let (code, answer) = rig.operator(body).await;
        assert_eq!(code, 200, "{answer}");
        check_ops(&rig, readers);
    }
    let (code, _) = rig.heartbeat("s1", "storage", HALT_BEAT).await;
    assert_eq!(code, 200);
    check_ops(&rig, readers);
    assert_eq!(bytes(&rig, "desired.json.ops"), recorded_bytes("ops.json"));
    assert_eq!(
        bytes(&rig, "desired.json.ops.halts"),
        recorded_bytes("ops.json.halts"),
        "the halt and its report are written as kbf wrote them"
    );

    let clear = r#"{"action":"clear_halt","service":"kbf","reason":"disk replaced"}"#;
    let include = r#"{"action":"include","service":"kbf","node_id":"s2"}"#;
    let unlabel = r#"{"action":"set_labels","node_id":"s1","labels":{}}"#;
    for body in [clear, include, unlabel] {
        let (code, answer) = rig.operator(body).await;
        assert_eq!(code, 200, "{answer}");
        check_ops(&rig, readers);
    }
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert_eq!(ops["services"]["kbf"]["halt"], Value::Null);
    // The clear's generation is kept in the halts file, as kbf keeps it, so the ops file
    // stays one that kbf's strict reader takes.
    assert_eq!(ops["services"]["kbf"]["halt_generation"], Value::Null);
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    assert_eq!(halts["services"]["kbf"]["halt_generation"], 1);
    assert_eq!(ops["audit"].as_array().map(Vec::len), Some(6));
}

/// An ops file of a newer schema is refused at open and never rewritten: its bytes are the same
/// afterwards and nothing else is made beside it.
fn a_newer_ops_file_is_refused_and_left_alone() {
    let dir = scratch("state-newer");
    let path = dir.path().join("ops.json");
    std::fs::write(&path, recorded_bytes("ops-schema2.json")).expect("written");
    match OpsStore::open(&path) {
        Err(OpsError::Newer { found: 2, .. }) => {}
        Err(other) => panic!("not the refusal of a newer schema: {other}"),
        Ok(_) => panic!("a newer schema was read"),
    }
    assert_eq!(
        std::fs::read(&path).expect("read"),
        recorded_bytes("ops-schema2.json")
    );
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("a directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(files, ["ops.json"], "nothing was made beside it");
}

/// Proves: the state files are compatible both ways. A desired-state file (single and map) and
/// an ops file with its halts file, as kbf's coordinator wrote them, are read by this one with
/// the recorded flags, and a desired-state file written back is the same bytes. What this one
/// writes (the desired state in both forms; the ops file after an exclusion, a label, a halt, a
/// clear, an include and a removal of labels; the halts file) parses as JSON and, checked by a
/// mirror of kbf's `deny_unknown_fields` readers (`strict-readers.json`), has only keys they
/// allow, every key they require and schema 1; for the changes kbf's fixtures record it is kbf's
/// bytes. An ops file of schema 2 is refused at open, and its bytes are unchanged afterwards.
#[tokio::test]
async fn state_files_written_by_kbf_are_read_and_the_reverse_holds_by_schema_rules() {
    let readers = Readers::recorded();
    assert!(readers.from.contains("v0.0.64"), "{}", readers.from);
    assert_eq!(
        readers.files.keys().map(String::as_str).collect::<Vec<_>>(),
        ["desired-map", "desired-single", "ops"]
    );
    kbf_desired_files_are_read_and_rewritten_to_the_same_bytes();
    kbf_ops_files_are_read_and_rewritten_within_kbfs_readers(&readers).await;
    what_this_coordinator_writes_is_within_kbfs_readers(&readers).await;
    a_newer_ops_file_is_refused_and_left_alone();
}

/// Catches: a mirror that takes anything. A key kbf's reader does not have, a key it requires
/// that is missing, a schema it does not read, and the same inside an array or a map are each
/// refused, naming where; kbf's own ops file is taken.
#[test]
fn the_mirror_of_kbfs_readers_refuses_what_they_refuse() {
    let readers = Readers::recorded();
    let kbf: Value = serde_json::from_str(&recorded("ops.json")).expect("kbf's ops file");
    readers.check("OpsState", &kbf, "ops").expect("kbf's file");
    let mut cases: Vec<(Value, &str)> = Vec::new();
    for (patch, word) in [
        (json!({"future": true}), "future"),
        (json!({"schema": 2}), "schema 1"),
    ] {
        let mut value = kbf.clone();
        value
            .as_object_mut()
            .expect("object")
            .extend(patch.as_object().expect("object").clone());
        cases.push((value, word));
    }
    let mut no_schema = kbf.clone();
    no_schema.as_object_mut().expect("object").remove("schema");
    cases.push((no_schema, "requires schema"));
    let mut nested = kbf.clone();
    nested["excluded"][0]["why"] = json!("because");
    cases.push((nested, "excluded[0]"));
    let mut deeper = kbf.clone();
    deeper["services"]["kbf"]["halt"]["extra"] = json!(1);
    cases.push((deeper, "services.kbf.halt"));
    let mut lacking = kbf.clone();
    lacking["audit"][1]
        .as_object_mut()
        .expect("object")
        .remove("by");
    cases.push((lacking, "audit[1]"));
    for (value, word) in cases {
        let why = readers
            .check("OpsState", &value, "ops")
            .expect_err("refused");
        assert!(why.contains(word), "{why} lacks {word}");
    }
    let desired: Value = serde_json::from_str(&recorded("desired-map.json")).expect("desired");
    readers
        .check("MapFile", &desired, "desired")
        .expect("kbf's map");
    let mut stray = desired.clone();
    stray["services"]["kbf"]["version2"] = json!("3");
    assert!(readers.check("MapFile", &stray, "desired").is_err());
}
