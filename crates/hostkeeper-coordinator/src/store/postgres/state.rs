//! The coordinator's state as the rows of `0001`'s tables, and back (section 3).
//!
//! Each part has its tables, written in the order of its list (a row a later table refers to
//! first) and its two ways: [`desired_rows`] and [`desired_of`], [`ops_rows`] and [`ops_of`],
//! [`verdicts_rows`] and [`verdicts_of`].
//!
//! The desired state and the verdicts go through the JSON their files hold: the rows are cut from
//! the JSON their writer makes, and a load puts that JSON together again and reads it with the
//! files' own strict readers (`desired::parse_file`, `verdict_file::parse`), so a load is exactly
//! as strict as a start on the files. The operations state is mapped field by field, its `jsonb`
//! values (a report, a force) written and read by their serde code.
//!
//! A per-service entry whose every field is at its default is not stored: a load reads it as
//! absent, which the code reads as the default. A list the code keeps in order has a `position`.
//! The audit ring is not stored apart: a load fills it from the newest rows of `audit`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hostkeeper_wire::Platform;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::UpdateResult;
use serde::Serialize;

use super::at::At;
use super::groups;
use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use crate::assignment::{Entry, ServiceAssignments};
use crate::desired::{self, StoreError};
use crate::force::ForceRecord;
use crate::holds::{HoldMark, HoldScope};
use crate::inventory::{InventoryRecord, RecordUse};
use crate::ops::{
    Exclusion, FailureAck, Halt, HaltAck, HaltOrigin, HostOps, OpsError, OpsState, ServiceOps,
};
use crate::rollouts::{Pause, Rollout};
use crate::verdict_file::{self, VerdictsError};

/// The desired state's tables, the releases' (`0004`), the registry's (`0007`) and the host
/// groups' (`0008`) last.
pub(crate) const DESIRED: [Table; 8] = [
    Table {
        name: "desired",
        keys: &[("service", KeyTy::Text)],
        values: &[
            ("version", Ty::Text),
            ("max_updating", Ty::Json),
            ("min_up", Ty::Json),
            ("release_digest", Ty::Text),
            ("release_index", Ty::Json),
            // Migration `0003`: the write record ([`super::generations`]).
            ("generation", Ty::Int),
            ("write_id", Ty::Text),
            ("replaced_version", Ty::Text),
            ("replaced_min_up", Ty::Json),
        ],
    },
    Table {
        name: "desired_digests",
        keys: &[("service", KeyTy::Text), ("version", KeyTy::Text)],
        values: &[("release", Ty::Json)],
    },
    super::releases::TABLE,
    super::repositories::TABLE,
    super::repositories::GENERATION,
    groups::HOST_GROUPS,
    groups::PACKAGES,
    groups::DESIRED,
];

/// The verdicts' tables.
pub(crate) const VERDICTS: [Table; 2] = [
    Table {
        name: "verdict_standing",
        keys: &[("service", KeyTy::Text), ("node_id", KeyTy::Text)],
        values: &[("report", Ty::Json)],
    },
    Table {
        name: "verdict_counted",
        keys: &[("service", KeyTy::Text), ("position", KeyTy::Int)],
        values: &[("node_id", Ty::Text), ("report", Ty::Json)],
    },
];

/// The operations state's tables, the host groups' (`0008`) last.
pub(crate) const OPS: [Table; 18] = [
    Table {
        name: "services",
        keys: &[("service", KeyTy::Text)],
        values: &[
            ("last_good", Ty::Text),
            ("halt_generation", Ty::Int),
            ("force_generation", Ty::Int),
            ("uninstall_generation", Ty::Int),
            ("force", Ty::Json),
        ],
    },
    Table {
        name: "bad_versions",
        keys: &[("service", KeyTy::Text), ("version", KeyTy::Text)],
        values: &[("by_force", Ty::Bool), ("position", Ty::Int)],
    },
    Table {
        name: "failure_acks",
        keys: &[("service", KeyTy::Text), ("node_id", KeyTy::Text)],
        values: &[("report", Ty::Json), ("position", Ty::Int)],
    },
    Table {
        name: "host_labels",
        keys: &[("node_id", KeyTy::Text), ("key", KeyTy::Text)],
        values: &[("value", Ty::Text)],
    },
    Table {
        name: "exclusions",
        keys: &[("service", KeyTy::Text), ("node_id", KeyTy::Text)],
        values: &[("reason", Ty::Text), ("position", Ty::Int)],
    },
    Table {
        name: "holds",
        keys: &[
            ("service", KeyTy::Text),
            ("scope", KeyTy::Text),
            ("target", KeyTy::Text),
        ],
        values: &[
            ("reason", Ty::Text),
            ("by_name", Ty::Text),
            ("at_unix", Ty::Int),
            ("position", Ty::Int),
        ],
    },
    Table {
        name: "assignments",
        keys: &[
            ("service", KeyTy::Text),
            ("scope", KeyTy::Text),
            ("target", KeyTy::Text),
        ],
        values: &[("entry", Ty::Text)],
    },
    Table {
        name: "min_versions",
        keys: &[
            ("service", KeyTy::Text),
            ("scope", KeyTy::Text),
            ("target", KeyTy::Text),
        ],
        values: &[("version", Ty::Text)],
    },
    Table {
        name: "uninstall_intents",
        keys: &[("service", KeyTy::Text), ("node_id", KeyTy::Text)],
        values: &[("generation", Ty::Int)],
    },
    Table {
        name: "inventory",
        keys: &[("host_id", KeyTy::Text)],
        values: &[
            ("role", Ty::Text),
            ("data_macs", Ty::Texts),
            ("bmc_mac", Ty::Text),
            ("serial", Ty::Text),
            ("notes", Ty::Text),
            ("by_name", Ty::Text),
            ("at_unix", Ty::Int),
            ("position", Ty::Int),
            ("approve_until_unix", Ty::Int),
            ("used_request_id", Ty::Text),
            ("used_unix", Ty::Int),
            ("platform", Ty::Text),
            ("host_group", Ty::Text),
        ],
    },
    Table {
        name: "halts",
        keys: &[("service", KeyTy::Text)],
        values: &[
            ("node_id", Ty::Text),
            ("check_name", Ty::Text),
            ("target", Ty::Text),
            ("origin", Ty::Text),
            ("report", Ty::Json),
        ],
    },
    Table {
        name: "halt_reports",
        keys: &[
            ("service", KeyTy::Text),
            ("kind", KeyTy::Text),
            ("node_id", KeyTy::Text),
        ],
        values: &[
            ("target", Ty::Text),
            ("report", Ty::Json),
            ("position", Ty::Int),
        ],
    },
    Table {
        name: "rollouts",
        keys: &[("service", KeyTy::Text)],
        values: &[
            ("version", Ty::Text),
            ("entered", Ty::Int),
            ("paused_by", Ty::Text),
            ("paused_reason", Ty::Text),
        ],
    },
    Table {
        name: "rollout_done",
        keys: &[("service", KeyTy::Text), ("node_id", KeyTy::Text)],
        values: &[],
    },
    groups::MIN_VERSIONS,
    groups::SERVICES,
    groups::ROLLOUTS,
    groups::ROLLOUT_DONE,
];

/// Where an error of a table says it is: the table, as a path the stores' errors name.
pub(crate) fn place(table: &str) -> PathBuf {
    PathBuf::from(format!("database:{table}"))
}

fn text(s: &str) -> KeyVal {
    KeyVal::Text(s.to_owned())
}

/// A counter of the code as a `bigint`.
///
/// # Errors
/// [`StoreError::Io`]: it is above `i64::MAX`, which the store refuses at the write.
pub(crate) fn int(n: u64) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Io {
        path: place("counter"),
        source: std::io::Error::other(format!(
            "{n} is above the largest counter the database keeps"
        )),
    })
}

fn json<T: Serialize>(value: &T) -> Val {
    // The values written as JSON are the code's own types, which serialise.
    serde_json::to_value(value).map_or(Val::Null, Val::Json)
}

/// The rows of `held`.
///
/// # Errors
/// [`StoreError`]: the state could not be serialised.
pub(crate) fn desired_rows(held: &desired::Held) -> Result<Tables, StoreError> {
    let encoded = desired::encode(held, None).map_err(|e| StoreError::Io {
        path: place("desired"),
        source: e.into(),
    })?;
    let value: serde_json::Value =
        serde_json::from_slice(&encoded).map_err(|e| StoreError::Io {
            path: place("desired"),
            source: e.into(),
        })?;
    let mut desired = Rows::new();
    let mut digests = Rows::new();
    let services = value
        .get("services")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (service, mut entry) in services {
        let field = |entry: &mut serde_json::Value, name: &str| {
            entry
                .as_object_mut()
                .and_then(|map| map.remove(name))
                .unwrap_or(serde_json::Value::Null)
        };
        if let Some(map) = field(&mut entry, "digests").as_object() {
            for (version, release) in map {
                digests.insert(
                    vec![text(&service), text(version)],
                    vec![Val::Json(release.clone())],
                );
            }
        }
        let version = field(&mut entry, "version");
        let digest = field(&mut entry, "release_digest");
        let index = field(&mut entry, "release_index");
        let mut row = vec![
            Val::text_or_null(version.as_str()),
            Val::Json(field(&mut entry, "max_updating")),
            Val::Json(field(&mut entry, "min_up")),
            Val::text_or_null(digest.as_str()),
            if index.is_null() {
                Val::Null
            } else {
                Val::Json(index)
            },
        ];
        row.extend(super::generations::columns(held.records.get(&service))?);
        desired.insert(vec![text(&service)], row);
    }
    let releases = super::releases::rows(&held.releases);
    let mut tables = Tables::from([
        ("desired", desired),
        ("desired_digests", digests),
        ("releases", releases),
    ]);
    super::repositories::insert(&held.repositories, &mut tables)?;
    groups::insert_groups(&held.host_groups, &mut tables)?;
    Ok(tables)
}

/// The desired state the rows `tables` hold, read as the desired-state file's reader reads it.
///
/// # Errors
/// [`StoreError::Invalid`]: a row holds what the reader refuses, the service named.
pub(crate) fn desired_of(tables: &Tables) -> Result<desired::Held, StoreError> {
    let empty = Rows::new();
    let mut services: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for (key, values) in tables.get("desired").unwrap_or(&empty) {
        let names = [
            "version",
            "max_updating",
            "min_up",
            "release_digest",
            "release_index",
        ];
        let entry = services.entry(key_text(key, 0)).or_default();
        for (name, value) in names.iter().zip(values) {
            if let Some(value) = json_of(value) {
                entry.insert((*name).to_owned(), value);
            }
        }
    }
    let mut digests: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (key, values) in tables.get("desired_digests").unwrap_or(&empty) {
        let release = values.first().and_then(json_of).unwrap_or_default();
        digests
            .entry(key_text(key, 0))
            .or_default()
            .insert(key_text(key, 1), release);
    }
    for (service, registry) in digests {
        services
            .entry(service)
            .or_default()
            .insert("digests".to_owned(), registry.into());
    }
    let file = serde_json::json!({ "services": services });
    let mut held = desired::parse_file(file.to_string().as_bytes(), None).map_err(|source| {
        StoreError::Invalid {
            path: place("desired"),
            source,
        }
    })?;
    held.records = super::generations::records_of(tables.get("desired").unwrap_or(&empty))?;
    held.releases = super::releases::of(tables.get("releases").unwrap_or(&empty))?;
    held.repositories = super::repositories::of_tables(tables)?;
    held.host_groups = groups::groups_of(tables)?;
    Ok(held)
}

/// The text of column `i` of a key: empty for a key that is not text, which no table of a part
/// read as JSON has.
fn key_text(key: &[KeyVal], i: usize) -> String {
    key.get(i)
        .and_then(KeyVal::as_text)
        .unwrap_or_default()
        .to_owned()
}

/// A value as the JSON its file holds: text as a string, JSON as itself; `None` for `NULL`, which
/// the file leaves out.
pub(crate) fn json_of(value: &Val) -> Option<serde_json::Value> {
    match value {
        Val::Text(text) => Some(text.clone().into()),
        Val::Json(value) => Some(value.clone()),
        _ => None,
    }
}

/// The rows of the verdicts `held`.
///
/// # Errors
/// [`VerdictsError`]: they could not be serialised.
pub(crate) fn verdicts_rows(held: &verdict_file::Held) -> Result<Tables, VerdictsError> {
    let value = serde_json::to_value(held).map_err(|e| VerdictsError::Invalid {
        path: place("verdicts"),
        why: e.to_string(),
    })?;
    let mut standing = Rows::new();
    let mut counted = Rows::new();
    let services = value
        .get("services")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (service, kept) in services {
        if let Some(map) = kept.get("standing").and_then(serde_json::Value::as_object) {
            for (node, report) in map {
                standing.insert(
                    vec![text(&service), text(node)],
                    vec![Val::Json(report.clone())],
                );
            }
        }
        let list = kept.get("counted").and_then(serde_json::Value::as_array);
        for (position, one) in list.into_iter().flatten().enumerate() {
            let node = one.get("node_id").and_then(serde_json::Value::as_str);
            counted.insert(
                vec![text(&service), KeyVal::Int(position as i64)],
                vec![
                    Val::text_or_null(node),
                    Val::Json(one.get("report").cloned().unwrap_or_default()),
                ],
            );
        }
    }
    Ok(Tables::from([
        ("verdict_standing", standing),
        ("verdict_counted", counted),
    ]))
}

/// The verdicts the rows `tables` hold, read as the verdicts' file reader reads them.
///
/// # Errors
/// [`VerdictsError::Invalid`]: a row holds what the reader refuses.
pub(crate) fn verdicts_of(tables: &Tables) -> Result<verdict_file::Held, VerdictsError> {
    let empty = Rows::new();
    let mut standing: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for (key, values) in tables.get("verdict_standing").unwrap_or(&empty) {
        let report = values.first().and_then(json_of).unwrap_or_default();
        standing
            .entry(key_text(key, 0))
            .or_default()
            .insert(key_text(key, 1), report);
    }
    let mut counted: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for (key, values) in tables.get("verdict_counted").unwrap_or(&empty) {
        let node = values.first().and_then(json_of).unwrap_or_default();
        let report = values.get(1).and_then(json_of).unwrap_or_default();
        counted
            .entry(key_text(key, 0))
            .or_default()
            .push(serde_json::json!({ "node_id": node, "report": report }));
    }
    let mut services: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for (service, map) in standing {
        services
            .entry(service)
            .or_default()
            .insert("standing".to_owned(), map.into());
    }
    for (service, list) in counted {
        services
            .entry(service)
            .or_default()
            .insert("counted".to_owned(), list.into());
    }
    let file = serde_json::json!({ "services": services });
    verdict_file::parse(&place("verdicts"), file.to_string().as_bytes())
}

/// The spelling of a hold's scope and its target.
fn hold_scope(scope: &HoldScope) -> (&'static str, &str) {
    match scope {
        HoldScope::Host(host) => ("host", host),
        HoldScope::Role(role) => ("role", role),
        HoldScope::Fleet => ("fleet", ""),
    }
}

fn entry_name(entry: Entry) -> &'static str {
    match entry {
        Entry::Include => "include",
        Entry::Exclude => "exclude",
    }
}

fn origin_name(origin: HaltOrigin) -> &'static str {
    match origin {
        HaltOrigin::Hook => "hook",
        HaltOrigin::Updater => "updater",
        HaltOrigin::Operator => "operator",
    }
}

/// The rows of the operations state `state`, its ring left out.
///
/// # Errors
/// [`StoreError`]: a counter is above what the database keeps.
pub(crate) fn ops_rows(state: &OpsState) -> Result<Tables, StoreError> {
    let mut tables: Tables = OPS.iter().map(|t| (t.name, Rows::new())).collect();
    let mut put = |table: &'static str, key: Vec<KeyVal>, values: Vec<Val>| {
        let rows = tables.entry(table).or_default();
        // The first of a key the code holds twice stands, as the file's first line would.
        rows.entry(key).or_insert(values);
    };
    let names: std::collections::BTreeSet<&String> = state
        .services
        .keys()
        .chain(state.assignments.keys())
        .collect();
    for service in names {
        let marks = state.services.get(service).cloned().unwrap_or_default();
        let assigned = state.assignments.get(service).cloned().unwrap_or_default();
        let s = || text(service);
        let row = vec![
            Val::text_or_null(marks.last_good.as_ref().map(Version::as_str)),
            Val::Int(int(marks.halt_generation)?),
            Val::Int(int(marks.force_generation)?),
            Val::Int(int(assigned.uninstall_generation)?),
            marks.force.as_ref().map_or(Val::Null, json),
        ];
        if row != [Val::Null, Val::Int(0), Val::Int(0), Val::Int(0), Val::Null] {
            put("services", vec![s()], row);
        }
        for (position, version) in marks.bad.iter().enumerate() {
            put(
                "bad_versions",
                vec![s(), text(version.as_str())],
                vec![
                    Val::Bool(marks.bad_by_force.contains(version)),
                    Val::Int(position as i64),
                ],
            );
        }
        for (position, ack) in marks.failure_acked.iter().enumerate() {
            put(
                "failure_acks",
                vec![s(), text(&ack.node_id)],
                vec![json(&ack.report), Val::Int(position as i64)],
            );
        }
        if let Some(halt) = &marks.halt {
            put(
                "halts",
                vec![s()],
                vec![
                    Val::Text(halt.node_id.clone()),
                    Val::Text(halt.check.clone()),
                    Val::Text(halt.target.to_string()),
                    Val::Text(origin_name(halt.origin).to_owned()),
                    halt.report.as_ref().map_or(Val::Null, json),
                ],
            );
        }
        for (kind, list) in [("acked", &marks.halt_acked), ("seen", &marks.halt_seen)] {
            for (position, ack) in list.iter().enumerate() {
                put(
                    "halt_reports",
                    vec![s(), text(kind), text(&ack.node_id)],
                    vec![
                        Val::Text(ack.target.to_string()),
                        ack.report.as_ref().map_or(Val::Null, json),
                        Val::Int(position as i64),
                    ],
                );
            }
        }
        for (scope, entries) in [("role", &assigned.roles), ("host", &assigned.hosts)] {
            for (target, entry) in entries {
                put(
                    "assignments",
                    vec![s(), text(scope), text(target)],
                    vec![Val::Text(entry_name(*entry).to_owned())],
                );
            }
        }
        let minimums = [
            ("role", &assigned.role_min_versions),
            ("host", &assigned.host_min_versions),
        ];
        for (scope, versions) in minimums {
            for (target, version) in versions {
                put(
                    "min_versions",
                    vec![s(), text(scope), text(target)],
                    vec![Val::Text(version.to_string())],
                );
            }
        }
        for (node, generation) in &assigned.uninstall {
            put(
                "uninstall_intents",
                vec![s(), text(node)],
                vec![Val::Int(int(*generation)?)],
            );
        }
    }
    for (node, host) in &state.hosts {
        for (key, value) in &host.labels {
            put(
                "host_labels",
                vec![text(node), text(key)],
                vec![Val::Text(value.clone())],
            );
        }
    }
    for (position, exclusion) in state.excluded.iter().enumerate() {
        put(
            "exclusions",
            vec![text(&exclusion.service), text(&exclusion.node_id)],
            vec![
                Val::Text(exclusion.reason.clone()),
                Val::Int(position as i64),
            ],
        );
    }
    for (position, hold) in state.holds.iter().enumerate() {
        let (scope, target) = hold_scope(&hold.scope);
        put(
            "holds",
            vec![text(&hold.service), text(scope), text(target)],
            vec![
                Val::Text(hold.reason.clone()),
                Val::Text(hold.by.clone()),
                Val::Int(int(hold.at_unix)?),
                Val::Int(position as i64),
            ],
        );
    }
    for (position, record) in state.inventory.iter().enumerate() {
        put(
            "inventory",
            vec![text(&record.host_id)],
            vec![
                Val::Text(record.role.clone()),
                Val::Texts(record.data_macs.clone()),
                Val::Text(record.bmc_mac.clone()),
                Val::Text(record.serial.clone()),
                Val::Text(record.notes.clone()),
                Val::Text(record.by.clone()),
                Val::Int(int(record.at_unix)?),
                Val::Int(position as i64),
                Val::Int(int(record.approve_until_unix)?),
                Val::text_or_null(record.used.as_ref().map(|used| used.request_id.as_str())),
                match &record.used {
                    Some(used) => Val::Int(int(used.at_unix)?),
                    None => Val::Null,
                },
                Val::Text(record.platform.map_or("", |p| p.as_str()).to_owned()),
                Val::text_or_null(Some(record.host_group.as_str()).filter(|g| !g.is_empty())),
            ],
        );
    }
    for (service, rollout) in &state.rollouts {
        if *rollout == Rollout::default() {
            continue;
        }
        let (by, reason) = rollout.paused.as_ref().map_or((None, None), |p| {
            (Some(p.by.as_str()), Some(p.reason.as_str()))
        });
        put(
            "rollouts",
            vec![text(service)],
            vec![
                Val::text_or_null(rollout.version.as_ref().map(Version::as_str)),
                Val::Int(i64::from(rollout.entered)),
                Val::text_or_null(by),
                Val::text_or_null(reason),
            ],
        );
        for node in &rollout.done {
            put("rollout_done", vec![text(service), text(node)], vec![]);
        }
    }
    groups::insert_marks(&state.host_groups, &mut tables)?;
    Ok(tables)
}

/// The operations state the rows `tables` hold, its ring left empty: every value parsed again,
/// a row that does not parse refused, naming its table and its key.
///
/// # Errors
/// [`OpsError::Invalid`]: a row holds what this build cannot read.
#[expect(
    clippy::too_many_lines,
    reason = "one table after the other, each a few lines; split, the order of the tables would be harder to follow"
)]
pub(crate) fn ops_of(tables: &Tables) -> Result<OpsState, OpsError> {
    let empty = Rows::new();
    let rows = |table: &'static str| {
        tables
            .get(table)
            .unwrap_or(&empty)
            .iter()
            .map(move |(key, values)| At { table, key, values })
    };
    let mut state = OpsState::default();
    let mut ordered: BTreeMap<(String, &str), Vec<(i64, HaltAck)>> = BTreeMap::new();
    let mut bad: BTreeMap<String, Vec<(i64, Version, bool)>> = BTreeMap::new();
    let mut acks: BTreeMap<String, Vec<(i64, FailureAck)>> = BTreeMap::new();
    for at in rows("services") {
        let service = at.key(0)?;
        let marks = state.services.entry(service.clone()).or_default();
        marks.last_good = at.version(0)?;
        marks.halt_generation = at.count(1)?;
        marks.force_generation = at.count(2)?;
        marks.force = at.json::<ForceRecord>(4)?;
        let uninstall = at.count(3)?;
        if uninstall != 0 {
            state
                .assignments
                .entry(service)
                .or_default()
                .uninstall_generation = uninstall;
        }
    }
    for at in rows("bad_versions") {
        let version = Version::parse(&at.key(1)?).map_err(|e| at.no(e))?;
        let by_force = matches!(at.values.first(), Some(Val::Bool(true)));
        bad.entry(at.key(0)?)
            .or_default()
            .push((at.position(1)?, version, by_force));
    }
    for (service, mut list) in bad {
        list.sort_by_key(|(position, ..)| *position);
        let marks = state.services.entry(service).or_default();
        for (_, version, by_force) in list {
            if by_force {
                marks.bad_by_force.push(version.clone());
            }
            marks.bad.push(version);
        }
    }
    for at in rows("failure_acks") {
        let report: UpdateResult = at.json(0)?.ok_or_else(|| at.no("the report is empty"))?;
        acks.entry(at.key(0)?).or_default().push((
            at.position(1)?,
            FailureAck {
                node_id: at.key(1)?,
                report,
            },
        ));
    }
    for (service, mut list) in acks {
        list.sort_by_key(|(position, _)| *position);
        state.services.entry(service).or_default().failure_acked =
            list.into_iter().map(|(_, ack)| ack).collect();
    }
    for at in rows("host_labels") {
        state
            .hosts
            .entry(at.key(0)?)
            .or_insert_with(HostOps::default)
            .labels
            .insert(at.key(1)?, at.required(0)?);
    }
    let mut excluded = Vec::new();
    for at in rows("exclusions") {
        excluded.push((
            at.position(1)?,
            Exclusion {
                service: at.key(0)?,
                node_id: at.key(1)?,
                reason: at.required(0)?,
            },
        ));
    }
    excluded.sort_by_key(|(position, _)| *position);
    state.excluded = excluded.into_iter().map(|(_, e)| e).collect();
    let mut holds = Vec::new();
    for at in rows("holds") {
        let target = at.key(2)?;
        let scope = match at.key(1)?.as_str() {
            "host" => HoldScope::Host(target),
            "role" => HoldScope::Role(target),
            "fleet" => HoldScope::Fleet,
            other => return Err(at.no(format!("the scope {other:?} is not one this build reads"))),
        };
        holds.push((
            at.position(3)?,
            HoldMark {
                service: at.key(0)?,
                scope,
                reason: at.required(0)?,
                by: at.required(1)?,
                at_unix: at.count(2)?,
            },
        ));
    }
    holds.sort_by_key(|(position, _)| *position);
    state.holds = holds.into_iter().map(|(_, h)| h).collect();
    for at in rows("assignments") {
        let entry = match at.required(0)?.as_str() {
            "include" => Entry::Include,
            "exclude" => Entry::Exclude,
            other => return Err(at.no(format!("the entry {other:?} is not one this build reads"))),
        };
        let assigned = state.assignments.entry(at.key(0)?).or_default();
        let map = match at.key(1)?.as_str() {
            "role" => &mut assigned.roles,
            "host" => &mut assigned.hosts,
            other => return Err(at.no(format!("the scope {other:?} is not one this build reads"))),
        };
        map.insert(at.key(2)?, entry);
    }
    for at in rows("min_versions") {
        let version = at
            .version(0)?
            .ok_or_else(|| at.no("the version is empty"))?;
        let assigned = state.assignments.entry(at.key(0)?).or_default();
        let map = match at.key(1)?.as_str() {
            "role" => &mut assigned.role_min_versions,
            "host" => &mut assigned.host_min_versions,
            other => return Err(at.no(format!("the scope {other:?} is not one this build reads"))),
        };
        map.insert(at.key(2)?, version);
    }
    for at in rows("uninstall_intents") {
        let generation = at.count(0)?;
        state
            .assignments
            .entry(at.key(0)?)
            .or_insert_with(ServiceAssignments::default)
            .uninstall
            .insert(at.key(1)?, generation);
    }
    let mut inventory = Vec::new();
    for at in rows("inventory") {
        let macs = match at.values.get(1) {
            Some(Val::Texts(macs)) => macs.clone(),
            _ => return Err(at.no("the data MACs are not a list")),
        };
        let used = match (at.text(9)?, at.values.get(10)) {
            (Some(request_id), Some(Val::Int(_))) => Some(RecordUse {
                request_id,
                at_unix: at.count(10)?,
            }),
            (None, Some(Val::Null)) => None,
            _ => return Err(at.no("a record's use is half there")),
        };
        let platform = match at.required(11)?.as_str() {
            "" => None,
            "linux-x86_64" => Some(Platform::LinuxX86_64),
            "linux-arm64" => Some(Platform::LinuxArm64),
            other => {
                return Err(at.no(format!("the platform {other:?} is not one a record states")));
            }
        };
        inventory.push((
            at.position(7)?,
            InventoryRecord {
                host_id: at.key(0)?,
                role: at.required(0)?,
                host_group: at.text(12)?.unwrap_or_default(),
                data_macs: macs,
                bmc_mac: at.required(2)?,
                serial: at.required(3)?,
                notes: at.required(4)?,
                by: at.required(5)?,
                at_unix: at.count(6)?,
                approve_until_unix: at.count(8)?,
                used,
                platform,
            },
        ));
    }
    inventory.sort_by_key(|(position, _)| *position);
    state.inventory = inventory.into_iter().map(|(_, r)| r).collect();
    for at in rows("halts") {
        let origin = match at.required(3)?.as_str() {
            "hook" => HaltOrigin::Hook,
            "updater" => HaltOrigin::Updater,
            "operator" => HaltOrigin::Operator,
            other => return Err(at.no(format!("the origin {other:?} is not one this build reads"))),
        };
        let halt = Halt {
            node_id: at.required(0)?,
            check: at.required(1)?,
            target: at.version(2)?.ok_or_else(|| at.no("the target is empty"))?,
            report: at.json(4)?,
            origin,
        };
        state.services.entry(at.key(0)?).or_default().halt = Some(halt);
    }
    for at in rows("halt_reports") {
        let kind = match at.key(1)?.as_str() {
            "acked" => "acked",
            "seen" => "seen",
            other => return Err(at.no(format!("the kind {other:?} is not one this build reads"))),
        };
        let ack = HaltAck {
            node_id: at.key(2)?,
            target: at.version(0)?.ok_or_else(|| at.no("the target is empty"))?,
            report: at.json(1)?,
        };
        ordered
            .entry((at.key(0)?, kind))
            .or_default()
            .push((at.position(2)?, ack));
    }
    for ((service, kind), mut list) in ordered {
        list.sort_by_key(|(position, _)| *position);
        let list: Vec<HaltAck> = list.into_iter().map(|(_, ack)| ack).collect();
        let marks: &mut ServiceOps = state.services.entry(service).or_default();
        if kind == "acked" {
            marks.halt_acked = list;
        } else {
            marks.halt_seen = list;
        }
    }
    for at in rows("rollouts") {
        let entered = at.count(1)?;
        let paused = match (at.text(2)?, at.text(3)?) {
            (Some(by), Some(reason)) => Some(Pause { by, reason }),
            _ => None,
        };
        state.rollouts.insert(
            at.key(0)?,
            Rollout {
                version: at.version(0)?,
                entered: u32::try_from(entered)
                    .map_err(|_| at.no("the stages entered are too many"))?,
                done: std::collections::BTreeSet::new(),
                paused,
            },
        );
    }
    for at in rows("rollout_done") {
        state
            .rollouts
            .entry(at.key(0)?)
            .or_default()
            .done
            .insert(at.key(1)?);
    }
    state.host_groups = groups::marks_of(tables)?;
    Ok(state)
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
