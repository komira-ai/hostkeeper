//! The host groups in the file store: the state file's `host_groups` key (the groups and their
//! desired versions), the ops file's `host_groups` key (each (group, package)'s marks), and the
//! `host_group` of an inventory record and of an audit entry. Each is written only when it holds
//! something, so a coordinator that holds no group writes what the release before reads; once one
//! is held, the strict readers of the release before refuse the file. No clock, no timing.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::conformance_tests::{desired_at, rich_ops};
use super::{Backend, BackendError, Commit, Files};
use crate::audit_file::{self, AuditLine, DEFAULT_ROTATION};
use crate::desired::{self, StoreError};
use crate::host_groups::tests::{every_mark, web};
use crate::host_groups::{GroupMarks, HostGroups};
use crate::ops::{AuditEntry, OpsError, OpsState};
use crate::testutil::{LEGACY, scratch};

/// The readers of the release before this one, as strict as they are (`deny_unknown_fields`), with
/// the keys they know: the state file's map, the ops file, its ring's entries and its inventory
/// records; and its audit line, which passes over a key it does not know. What they hold below
/// these keys is read as any JSON, as only the keys this release adds matter here.
#[allow(dead_code, reason = "the fields are read by serde only")]
mod previous {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MapFile {
        services: BTreeMap<String, Value>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AuditEntry {
        seq: u64,
        by: String,
        action: String,
        service: Option<String>,
        node_id: Option<String>,
        reason: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct InventoryRecord {
        host_id: String,
        role: Option<String>,
        data_macs: Option<Value>,
        bmc_mac: Option<String>,
        serial: Option<String>,
        notes: Option<String>,
        by: String,
        at_unix: u64,
        approve_until_unix: Option<u64>,
        used: Option<Value>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OpsFile {
        schema: u32,
        hosts: Option<Value>,
        excluded: Option<Value>,
        services: Option<Value>,
        #[serde(default)]
        audit: Vec<AuditEntry>,
        holds: Option<Value>,
        assignments: Option<Value>,
        #[serde(default)]
        inventory: Vec<InventoryRecord>,
    }

    #[derive(Deserialize)]
    struct Detail {
        service: Option<String>,
        node_id: Option<String>,
        reason: Option<String>,
    }

    #[derive(Deserialize)]
    struct AuditLine {
        seq: u64,
        at_unix: u64,
        by: String,
        action: String,
        detail: Detail,
    }

    fn reads<T: serde::de::DeserializeOwned>(text: &[u8]) -> Result<(), String> {
        serde_json::from_slice::<T>(text)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Whether the release before starts on the state file `text` in its map form.
    pub(super) fn reads_state(text: &[u8]) -> Result<(), String> {
        reads::<MapFile>(text)
    }

    /// Whether the release before starts on the ops file `text`.
    pub(super) fn reads_ops(text: &[u8]) -> Result<(), String> {
        reads::<OpsFile>(text)
    }

    /// Whether the release before reads the audit line `text`.
    pub(super) fn reads_line(text: &[u8]) -> Result<(), String> {
        reads::<AuditLine>(text)
    }
}

fn state_path(dir: &Path) -> std::path::PathBuf {
    dir.join("desired.json")
}

fn files_over(dir: &Path) -> Files {
    Files::default()
        .with_desired(&state_path(dir), Some(LEGACY))
        .with_ops(&dir.join("desired.json.ops"), DEFAULT_ROTATION)
        .expect("the ops files")
}

fn commit(files: &Files, change: &Commit<'_>) -> Result<(), BackendError> {
    files.commit(change)
}

/// The desired state of the legacy service alone at `2.0`, with the group `web` beside it.
fn with_groups() -> desired::Held {
    desired::Held {
        desired: desired_at("2.0", 'a')
            .desired
            .into_values()
            .map(|d| (LEGACY.to_owned(), d))
            .collect(),
        host_groups: HostGroups(BTreeMap::from([("web".parse().expect("a name"), web())])),
        ..desired::Held::default()
    }
}

/// Proves: a desired state that holds a group is written in the map form, its groups under the
/// top-level `host_groups`, even when the legacy service is its only service (the single form
/// has no place for them), and is read back equal; the release before's map reader refuses the
/// file by the key, and the same state without its group is the single state, which names no
/// group.
///
/// Catches: a group written into a file the release before reads as if it held none, or the
/// single form lost for a farm that holds no group.
#[test]
fn a_state_file_with_a_group_is_the_map_and_the_release_before_refuses_it() {
    let dir = scratch("files-host-groups-state");
    let files = files_over(dir.path());
    let after = with_groups();
    commit(
        &files,
        &Commit {
            desired: Some((&desired::Held::default(), &after)),
            ..Commit::default()
        },
    )
    .expect("committed");
    let text = fs::read(state_path(dir.path())).expect("the state file");
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    assert_eq!(
        value["host_groups"]["web"]["packages"],
        serde_json::json!(["tierstore", "hostkeeper"])
    );
    assert!(value["services"][LEGACY].is_object(), "{value}");
    let refused = previous::reads_state(&text).expect_err("the release before is strict");
    assert!(
        refused.starts_with("unknown field `host_groups`"),
        "{refused}"
    );
    let held = files_over(dir.path()).load_desired().expect("loads");
    assert_eq!(held.host_groups, after.host_groups);
    assert_eq!(held.desired, after.desired);

    let plain = desired::Held {
        host_groups: HostGroups::default(),
        ..after.clone()
    };
    let bytes = desired::encode(&plain, Some(LEGACY)).expect("encodes");
    assert!(!String::from_utf8_lossy(&bytes).contains("host_groups"));
    let single: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
    assert!(
        single.get("services").is_none(),
        "the single form: {single}"
    );
}

/// Proves: a state file whose group breaks a rule (no `hostkeeper` in its list) or whose
/// top-level key is unknown is refused at load, naming the file and the group or the key.
///
/// Catches: a start that runs on a group it would place hosts wrongly by.
#[test]
fn a_state_file_with_a_bad_group_is_refused_naming_the_file_and_the_group() {
    let dir = scratch("files-host-groups-bad");
    let path = state_path(dir.path());
    let mut value = serde_json::to_value(serde_json::json!({
        "services": {},
        "host_groups": {"web": web()},
    }))
    .expect("JSON");
    value["host_groups"]["all"] = value["host_groups"]["web"].clone();
    value["host_groups"]["all"]["packages"] = serde_json::json!(["tierstore"]);
    fs::write(&path, value.to_string()).expect("written");
    let error = files_over(dir.path()).load_desired().expect_err("refused");
    assert!(matches!(error, StoreError::Invalid { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the desired state is malformed: host group all: does not list hostkeeper",
            path.display()
        )
    );
    fs::write(&path, r#"{"services": {}, "host_group": {}}"#).expect("written");
    let error = files_over(dir.path()).load_desired().expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the desired state is malformed: unknown field `host_group`",
            path.display()
        )
    );
}

/// An operations state as `rich_ops` makes it, with the marks of `tierstore` in `web` and of a
/// pair at its default.
fn ops_with_marks() -> OpsState {
    let mut state = rich_ops();
    state.host_groups.0.insert(
        "web".parse().expect("a name"),
        BTreeMap::from([
            ("tierstore".to_owned(), every_mark()),
            ("hostkeeper".to_owned(), GroupMarks::default()),
        ]),
    );
    state
}

fn ops_text(dir: &Path) -> Vec<u8> {
    fs::read(dir.join("desired.json.ops")).expect("the ops file")
}

fn commit_ops(files: &Files, after: &OpsState, audit: &[AuditLine]) {
    commit(
        files,
        &Commit {
            ops: Some((&OpsState::default(), after)),
            audit,
            ..Commit::default()
        },
    )
    .expect("committed");
}

/// Proves: the marks of a (group, package) are written in the ops file under its top-level
/// `host_groups`, a pair at its default left out, and read back as written, the group's halt and
/// plan position in the ops file itself (the halts file holds none of them); the release before
/// refuses the file by the key; an ops file whose groups hold only marks at their default has no
/// key, and the release before reads it.
///
/// Catches: a group's halt or plan lost at a restart, a key written for nothing (which would
/// refuse the release before on a farm that holds no group), or a group's marks the release
/// before would run without.
#[test]
fn an_ops_file_with_group_marks_is_refused_by_the_release_before_and_read_back() {
    let dir = scratch("files-host-groups-ops");
    let files = files_over(dir.path());
    let after = ops_with_marks();
    commit_ops(&files, &after, &[]);
    let text = ops_text(dir.path());
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    assert_eq!(
        value["host_groups"],
        serde_json::json!({"web": {"tierstore": serde_json::to_value(every_mark())
            .expect("serialises")}})
    );
    let halts = fs::read_to_string(dir.path().join("desired.json.ops.halts")).expect("halts");
    assert!(!halts.contains("web-01"), "{halts}");
    let refused = previous::reads_ops(&text).expect_err("the release before is strict");
    assert!(
        refused.starts_with("unknown field `host_groups`"),
        "{refused}"
    );
    let back = files_over(dir.path()).load_ops().expect("loads");
    assert_eq!(
        back.host_groups,
        after.host_groups.clone().without_defaults()
    );
    let web: crate::host_groups::GroupName = "web".parse().expect("a name");
    assert_eq!(
        back.host_groups.0[&web]["tierstore"].force,
        every_mark().force,
        "the standing force, whole"
    );

    let calm = scratch("files-host-groups-ops-calm");
    let mut defaults = rich_ops();
    defaults.host_groups.0.insert(
        "web".parse().expect("a name"),
        BTreeMap::from([("hostkeeper".to_owned(), GroupMarks::default())]),
    );
    commit_ops(&files_over(calm.path()), &defaults, &[]);
    let text = ops_text(calm.path());
    assert!(!String::from_utf8_lossy(&text).contains("host_groups"));
    assert_eq!(previous::reads_ops(&text), Ok(()));
}

/// Proves: an ops file whose marks name a package that is no package name is refused at load,
/// naming the file, the group and the package.
///
/// Catches: a load less strict than its rules.
#[test]
fn an_ops_file_with_a_bad_package_in_a_group_is_refused_naming_it() {
    let dir = scratch("files-host-groups-ops-bad");
    let path = dir.path().join("desired.json.ops");
    fs::write(
        &path,
        r#"{"schema": 1, "host_groups": {"web": {"a b": {"last_good": "1.0"}}}}"#,
    )
    .expect("written");
    let error = files_over(dir.path()).load_ops().expect_err("refused");
    assert!(matches!(error, OpsError::Invalid { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: host group web: \"a b\" is not a package name",
            path.display()
        )
    );
}

/// Proves: an inventory record that names a host group and an audit entry of a group are written
/// with `host_group` (the record in the ops file, the entry in its ring and in the audit file's
/// `detail`, after `node_id`) and read back with it; the release before refuses the ops file by
/// the key, and its audit reader reads the line, passing over the key. A record and an entry that
/// name no group are written without the key, and the release before reads the file.
///
/// Catches: a record's group lost, an audit line the release before cannot read (its history
/// would lose the line), or a key written for nothing.
#[test]
fn a_record_and_an_audit_entry_of_a_group_are_written_with_it_and_read_back() {
    let dir = scratch("files-host-groups-record");
    let files = files_over(dir.path());
    let mut entry = AuditEntry::new("alice", "host_move");
    entry.seq = 1;
    entry.node_id = Some("web-04".to_owned());
    entry.host_group = Some("web".to_owned());
    entry.reason = Some("from db".to_owned());
    let line = AuditLine::of(&entry, 1_800_000_000);
    let mut after = rich_ops();
    after.inventory[1].host_group = "web".to_owned();
    after.audit = vec![entry.clone()];
    commit_ops(&files, &after, std::slice::from_ref(&line));
    let text = ops_text(dir.path());
    let value: serde_json::Value = serde_json::from_slice(&text).expect("JSON");
    assert_eq!(value["inventory"][1]["host_group"], "web");
    assert_eq!(value["audit"][0]["host_group"], "web");
    let refused = previous::reads_ops(&text).expect_err("the release before is strict");
    assert!(
        refused.starts_with("unknown field `host_group`"),
        "{refused}"
    );
    let back = files_over(dir.path()).load_ops().expect("loads");
    assert_eq!(back.inventory, after.inventory);
    assert_eq!(back.audit, after.audit);
    let audit = fs::read(audit_file::path_of(&dir.path().join("desired.json.ops"))).expect("audit");
    assert_eq!(
        String::from_utf8_lossy(&audit),
        "{\"seq\":1,\"at_unix\":1800000000,\"by\":\"alice\",\"action\":\"host_move\",\"detail\":\
         {\"node_id\":\"web-04\",\"host_group\":\"web\",\"reason\":\"from db\"}}\n"
    );
    assert_eq!(previous::reads_line(audit.trim_ascii_end()), Ok(()));
    let read =
        audit_file::read(&audit_file::path_of(&dir.path().join("desired.json.ops"))).expect("read");
    assert_eq!(read.lines[0].clone().into_logged().entry, entry);

    let plain = scratch("files-host-groups-record-plain");
    let mut none = rich_ops();
    none.audit = vec![AuditEntry {
        host_group: None,
        ..entry
    }];
    commit_ops(&files_over(plain.path()), &none, &[]);
    let text = ops_text(plain.path());
    assert!(!String::from_utf8_lossy(&text).contains("host_group"));
    assert_eq!(previous::reads_ops(&text), Ok(()));
}
