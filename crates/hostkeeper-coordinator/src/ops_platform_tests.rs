//! The platform of an inventory record on disk (network-install design, section 11): it is kept
//! in the halts file under `platforms` and never in the ops file, so the ops file is byte for
//! byte the one a record without a platform gives; a platform joins back to the record of its
//! host id; one this release cannot read is passed over and counted; and the release before
//! reads what this release writes while this release reads what the release before wrote.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use hostkeeper_wire::Platform;
use serde_json::{Value, json};

use crate::inventory::InventoryRecord;
use crate::ops::OpsState;
use crate::ops_file::{digest, halts_path, joined, write};
use crate::testutil::scratch;

/// The reader of the release before this one: its ops file types as they were, strict as they
/// were (`deny_unknown_fields`), with a record that has no platform. Only reading matters.
#[allow(dead_code, reason = "the fields are read by serde only")]
mod previous {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct InventoryRecord {
        host_id: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        data_macs: Vec<String>,
        #[serde(default)]
        bmc_mac: String,
        #[serde(default)]
        serial: String,
        #[serde(default)]
        notes: String,
        by: String,
        at_unix: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OpsState {
        schema: u32,
        #[serde(default)]
        hosts: BTreeMap<String, Value>,
        #[serde(default)]
        excluded: Vec<Value>,
        #[serde(default)]
        services: BTreeMap<String, Value>,
        #[serde(default)]
        audit: Vec<Value>,
        #[serde(default)]
        holds: Vec<Value>,
        #[serde(default)]
        assignments: BTreeMap<String, Value>,
        #[serde(default)]
        inventory: Vec<InventoryRecord>,
    }

    /// The host ids of the records the previous release reads from `text`, its ops file.
    pub(super) fn reads(text: &str) -> Result<Vec<String>, String> {
        serde_json::from_str::<OpsState>(text)
            .map(|state| state.inventory.into_iter().map(|r| r.host_id).collect())
            .map_err(|e| e.to_string())
    }

    /// The halts file as the previous release reads it: it skips a key it does not know, so
    /// `platforms` is passed over.
    #[derive(Debug, Deserialize)]
    pub(super) struct Halts {
        #[serde(default)]
        pub(super) services: BTreeMap<String, Value>,
        #[serde(default)]
        pub(super) ops: Option<String>,
        #[serde(default)]
        pub(super) rollouts: BTreeMap<String, Value>,
    }
}

/// A record of `host` stating `platform`.
fn record(host: &str, mac: &str, platform: Option<Platform>) -> InventoryRecord {
    InventoryRecord {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: vec![mac.to_owned()],
        bmc_mac: String::new(),
        serial: String::new(),
        notes: String::new(),
        by: "operator".to_owned(),
        at_unix: 1_800_000_000,
        platform,
    }
}

/// A state holding `records`.
fn holding(records: Vec<InventoryRecord>) -> OpsState {
    let mut state = OpsState::default();
    state.inventory = records;
    state
}

fn text(path: &Path) -> String {
    fs::read_to_string(path).expect("the file")
}

fn halts(path: &Path) -> Value {
    serde_json::from_str(&text(&halts_path(path))).expect("the halts file")
}

/// What this release reads from the files at `path`: the ops file as the ops store parses it,
/// joined with the halts file, and how many records were passed over.
fn read(path: &Path) -> (OpsState, usize) {
    let ops: OpsState = serde_json::from_str(&text(path)).expect("the ops file");
    joined(path, ops).expect("read")
}

const TWO: [(&str, &str, Option<Platform>); 2] = [
    ("db-1", "0c:c4:7a:00:00:01", Some(Platform::LinuxArm64)),
    ("db-2", "0c:c4:7a:00:00:02", None),
];

fn two() -> OpsState {
    holding(
        TWO.iter()
            .map(|(host, mac, platform)| record(host, mac, *platform))
            .collect(),
    )
}

/// The ops file text a record of `db-1` with no platform gives, byte for byte.
const OPS_GOLDEN: &str = r#"{
  "schema": 1,
  "hosts": {},
  "excluded": [],
  "services": {},
  "audit": [],
  "inventory": [
    {
      "host_id": "db-1",
      "role": "storage",
      "data_macs": [
        "0c:c4:7a:00:00:01"
      ],
      "by": "operator",
      "at_unix": 1800000000
    }
  ]
}
"#;

/// Proves: records with no platform give the ops file `OPS_GOLDEN` byte for byte and create no
/// halts file; reading it back gives the same records with no platform.
///
/// Catches: a farm that states no platform given a halts file, or a key written to the ops file.
#[test]
fn records_with_no_platform_write_the_ops_file_as_before_and_no_halts_file() {
    let dir = scratch("ops-platform-none");
    let path = dir.path().join("ops.json");
    let state = holding(vec![record("db-1", "0c:c4:7a:00:00:01", None)]);
    write(&path, &state).expect("written");
    assert_eq!(text(&path), OPS_GOLDEN);
    assert!(!halts_path(&path).exists());
    assert_eq!(read(&path), (state, 0));
}

/// Proves: a record with a platform leaves the ops file the same text as without one, puts the
/// platform in the halts file (the whole file by exact equality, naming the digest of the ops
/// file), and reads back as it was written.
///
/// Catches: the platform in the ops file, where the release before refuses the key, or a
/// platform that is written and not read back.
#[test]
fn a_platform_is_written_to_the_halts_file_only_and_reads_back() {
    let dir = scratch("ops-platform-some");
    let path = dir.path().join("ops.json");
    let state = holding(vec![record(
        "db-1",
        "0c:c4:7a:00:00:01",
        Some(Platform::LinuxArm64),
    )]);
    write(&path, &state).expect("written");
    assert_eq!(text(&path), OPS_GOLDEN);
    let want = format!(
        "{{\n  \"services\": {{}},\n  \"ops\": \"{}\",\n  \"platforms\": {{\n    \"db-1\": \"linux-arm64\"\n  }}\n}}\n",
        digest(OPS_GOLDEN.as_bytes())
    );
    assert_eq!(text(&halts_path(&path)), want);
    assert_eq!(read(&path), (state, 0));
}

/// Proves: an ops file written before the field, with a record, and no halts file, reads with
/// the record unchanged and no platform.
///
/// Catches: a file from before the field refused, or a platform made up for a record.
#[test]
fn an_ops_file_written_before_the_field_reads_unchanged() {
    let dir = scratch("ops-platform-before");
    let path = dir.path().join("ops.json");
    fs::write(&path, OPS_GOLDEN).expect("written");
    let (state, skipped) = read(&path);
    assert_eq!(skipped, 0);
    assert_eq!(
        state.inventory,
        vec![record("db-1", "0c:c4:7a:00:00:01", None)]
    );
}

/// Proves: a platform in the halts file whose host id has no record in the ops file is dropped
/// (not counted as passed over), and the record that has one keeps it.
///
/// Catches: a platform that outlives its record, or joined to a record of another host.
#[test]
fn a_platform_whose_record_is_not_in_the_ops_file_is_dropped() {
    let dir = scratch("ops-platform-orphan");
    let path = dir.path().join("ops.json");
    fs::write(&path, OPS_GOLDEN).expect("written");
    fs::write(
        halts_path(&path),
        json!({"platforms": {"db-1": "linux-x86_64", "gone": "linux-arm64"}}).to_string(),
    )
    .expect("written");
    let (state, skipped) = read(&path);
    assert_eq!(skipped, 0);
    assert_eq!(
        state.inventory,
        vec![record(
            "db-1",
            "0c:c4:7a:00:00:01",
            Some(Platform::LinuxX86_64)
        )]
    );
}

/// Proves: of the platforms in the halts file, "darwin-arm64", "linux-riscv64" and a number are
/// passed over and counted (three), those records read with none, the readable one is kept, and
/// the rest of the file (a clear generation) still reads, both through [`joined`] and when the
/// ops store opens.
///
/// Catches: a platform a later release wrote that makes the whole halts file refuse the start,
/// is not counted, or takes the readable platform with it.
#[test]
fn a_platform_this_release_cannot_read_is_passed_over_and_counted() {
    let dir = scratch("ops-platform-unreadable");
    let path = dir.path().join("ops.json");
    let records = vec![
        record("db-1", "0c:c4:7a:00:00:01", None),
        record("db-2", "0c:c4:7a:00:00:02", None),
        record("db-3", "0c:c4:7a:00:00:03", None),
        record("db-4", "0c:c4:7a:00:00:04", None),
    ];
    write(&path, &holding(records)).expect("written");
    fs::write(
        halts_path(&path),
        json!({
            "services": {"kbf": {"halt_generation": 3}},
            "platforms": {
                "db-1": "darwin-arm64",
                "db-2": "linux-riscv64",
                "db-3": 7,
                "db-4": "linux-arm64"
            }
        })
        .to_string(),
    )
    .expect("written");
    let (state, skipped) = read(&path);
    assert_eq!(skipped, 3);
    let platforms: Vec<Option<Platform>> = state.inventory.iter().map(|r| r.platform).collect();
    assert_eq!(platforms, [None, None, None, Some(Platform::LinuxArm64)]);
    assert_eq!(state.services["kbf"].halt_generation, 3);
    let opened = crate::ops::OpsStore::open(&path)
        .expect("the start is not refused")
        .state();
    assert_eq!(opened.inventory, state.inventory);
}

/// Proves: when a record is removed its platform is gone from the halts file at the next write;
/// when the last one is removed the halts file (which exists by then) keeps no `platforms` key.
///
/// Catches: a platform left behind that joins to a record added later under the same host id.
#[test]
fn removing_a_record_drops_its_platform_on_the_next_write() {
    let dir = scratch("ops-platform-removed");
    let path = dir.path().join("ops.json");
    let mut state = two();
    state.inventory[1].platform = Some(Platform::LinuxX86_64);
    write(&path, &state).expect("written");
    assert_eq!(
        halts(&path)["platforms"],
        json!({"db-1": "linux-arm64", "db-2": "linux-x86_64"})
    );
    state.inventory.remove(0);
    write(&path, &state).expect("written");
    assert_eq!(halts(&path)["platforms"], json!({"db-2": "linux-x86_64"}));
    state.inventory.clear();
    write(&path, &state).expect("written");
    assert!(halts(&path).get("platforms").is_none());
    assert_eq!(read(&path), (state, 0));
}

/// Proves: the release before reads the ops file this release writes with platforms recorded
/// (it refuses a key it does not know, and there is none), sees the same records, and reads
/// the halts file beside it, skipping `platforms`; the ops file is the one the same records
/// give without platforms.
///
/// Catches: a platform written where the release before refuses to start on it (B8: the release
/// before on this release's data).
#[test]
fn the_release_before_reads_the_files_written_with_platforms() {
    let dir = scratch("ops-platform-prev-reads");
    let path = dir.path().join("ops.json");
    write(&path, &two()).expect("written");
    let without = dir.path().join("without.json");
    let mut bare = two();
    bare.inventory[0].platform = None;
    write(&without, &bare).expect("written");
    assert_eq!(text(&path), text(&without));
    assert_eq!(
        previous::reads(&text(&path)),
        Ok(vec!["db-1".into(), "db-2".into()])
    );
    let kept: previous::Halts =
        serde_json::from_str(&text(&halts_path(&path))).expect("the release before reads it");
    assert_eq!(kept.ops, Some(digest(text(&path).as_bytes())));
    assert!(kept.services.is_empty() && kept.rollouts.is_empty());
}

/// Proves: the files the release before wrote (an ops file with records, and a halts file with
/// a clear generation and a plan position, which has no `platforms`) read in this release, with
/// every record reading with no platform and the rest of the halts file joined.
///
/// Catches: files from before the field refused or misread (B8: this release on the release
/// before's data).
#[test]
fn this_release_reads_the_files_the_release_before_wrote() {
    let dir = scratch("ops-platform-prev-wrote");
    let path = dir.path().join("ops.json");
    fs::write(&path, OPS_GOLDEN).expect("written");
    assert_eq!(previous::reads(OPS_GOLDEN), Ok(vec!["db-1".into()]));
    let halts_text = json!({
        "services": {"kbf": {"halt_generation": 2}},
        "ops": digest(OPS_GOLDEN.as_bytes()),
        "rollouts": {"kbf": {"version": "2.0", "entered": 1}}
    })
    .to_string();
    let before: previous::Halts = serde_json::from_str(&halts_text).expect("json");
    assert_eq!(before.services.len(), 1);
    fs::write(halts_path(&path), halts_text).expect("written");
    let (state, skipped) = read(&path);
    assert_eq!(skipped, 0);
    assert_eq!(
        state.inventory,
        vec![record("db-1", "0c:c4:7a:00:00:01", None)]
    );
    assert_eq!(state.services["kbf"].halt_generation, 2);
    assert!(state.rollouts.contains_key("kbf"));
}

/// Proves: the halts file read from a platform map holds exactly the strings
/// [`Platform::as_str`] names, for both Linux platforms.
///
/// Catches: a spelling in the halts file that the release reading it does not parse.
#[test]
fn the_halts_file_spells_each_linux_platform_as_its_name() {
    let dir = scratch("ops-platform-spelling");
    let path = dir.path().join("ops.json");
    let state = holding(vec![
        record("db-1", "0c:c4:7a:00:00:01", Some(Platform::LinuxX86_64)),
        record("db-2", "0c:c4:7a:00:00:02", Some(Platform::LinuxArm64)),
    ]);
    write(&path, &state).expect("written");
    let platforms: BTreeMap<String, String> =
        serde_json::from_value(halts(&path)["platforms"].clone()).expect("a map");
    assert_eq!(
        platforms.values().map(String::as_str).collect::<Vec<_>>(),
        ["linux-x86_64", "linux-arm64"]
    );
    assert_eq!(read(&path), (state, 0));
}
