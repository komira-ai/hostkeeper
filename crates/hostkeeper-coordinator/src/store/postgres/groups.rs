//! The host groups ([`crate::host_groups`]) as the rows of the tables migration `0008` adds
//! (`docs/design/hostgroups.md`, section 8), and back.
//!
//! The desired state's part: `host_groups`, one row per group (its generation, its policy as the
//! `jsonb` the state file holds, who changed it last and when), `host_group_packages`, one row per
//! package of its list with its `position`, and `group_desired`, one row per desired version with
//! its write record. A load puts each group's JSON together again and reads it with the state
//! file's own reader, so a load is as strict as a start on the files.
//!
//! The operations state's part, one row per (group, package) in each table that holds something
//! of it: `group_min_versions`; `group_services`, its last good version, halt, halt reports and
//! force, each with its generation, as `jsonb` by their serde code; `group_rollouts` and
//! `group_rollout_done`, its plan position. A pair whose marks are all at their default has no row.

use std::collections::BTreeMap;

use hostkeeper_wire::version::Version;

use super::at::At;
use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use super::state::{int, json_of, place};
use crate::desired::{DesiredError, StoreError};
use crate::host_groups::{GroupMarks, GroupName, GroupsMarks, HostGroups};
use crate::ops::{HaltAck, OpsError};
use crate::rollouts::Pause;

/// The table of the groups.
pub(crate) const HOST_GROUPS: Table = Table {
    name: "host_groups",
    keys: &[("name", KeyTy::Text)],
    values: &[
        ("generation", Ty::Int),
        ("policy", Ty::Json),
        ("by_name", Ty::Text),
        ("at_unix", Ty::Int),
    ],
};

/// The table of each group's packages.
pub(crate) const PACKAGES: Table = Table {
    name: "host_group_packages",
    keys: &[("host_group", KeyTy::Text), ("service", KeyTy::Text)],
    values: &[("position", Ty::Int)],
};

/// The table of each group's desired versions.
pub(crate) const DESIRED: Table = Table {
    name: "group_desired",
    keys: &[("host_group", KeyTy::Text), ("service", KeyTy::Text)],
    values: &[
        ("version", Ty::Text),
        ("generation", Ty::Int),
        ("write_id", Ty::Text),
        ("replaced_version", Ty::Text),
    ],
};

/// The table of each (group, package)'s minimum version.
pub(crate) const MIN_VERSIONS: Table = Table {
    name: "group_min_versions",
    keys: &[("host_group", KeyTy::Text), ("service", KeyTy::Text)],
    values: &[("version", Ty::Text)],
};

/// The table of each (group, package)'s marks.
pub(crate) const SERVICES: Table = Table {
    name: "group_services",
    keys: &[("host_group", KeyTy::Text), ("service", KeyTy::Text)],
    values: &[
        ("last_good", Ty::Text),
        ("halt_generation", Ty::Int),
        ("force_generation", Ty::Int),
        ("force", Ty::Json),
        ("halt", Ty::Json),
        ("halt_acked", Ty::Json),
        ("halt_seen", Ty::Json),
    ],
};

/// The table of each (group, package)'s plan position.
pub(crate) const ROLLOUTS: Table = Table {
    name: "group_rollouts",
    keys: &[("host_group", KeyTy::Text), ("service", KeyTy::Text)],
    values: &[
        ("version", Ty::Text),
        ("entered", Ty::Int),
        ("paused_by", Ty::Text),
        ("paused_reason", Ty::Text),
    ],
};

/// The table of the hosts each (group, package)'s plan recorded done.
pub(crate) const ROLLOUT_DONE: Table = Table {
    name: "group_rollout_done",
    keys: &[
        ("host_group", KeyTy::Text),
        ("service", KeyTy::Text),
        ("node_id", KeyTy::Text),
    ],
    values: &[],
};

fn text(s: &str) -> KeyVal {
    KeyVal::Text(s.to_owned())
}

/// A value as `jsonb`; `NULL` for one that does not serialise, which the code's own types do.
fn json<T: serde::Serialize>(value: &T) -> Val {
    serde_json::to_value(value).map_or(Val::Null, Val::Json)
}

/// Puts the rows of `groups` into `tables`, under the tables' names.
///
/// # Errors
/// [`StoreError::Io`]: a counter is above `i64::MAX`, which the store refuses.
pub(crate) fn insert_groups(groups: &HostGroups, tables: &mut Tables) -> Result<(), StoreError> {
    let (mut rows, mut packages, mut desired) = (Rows::new(), Rows::new(), Rows::new());
    for (name, group) in &groups.0 {
        let name = name.as_str();
        rows.insert(
            vec![text(name)],
            vec![
                Val::Int(int(group.generation)?),
                json(&group.policy),
                Val::Text(group.by.clone()),
                Val::Int(int(group.at_unix)?),
            ],
        );
        for (position, package) in group.packages.iter().enumerate() {
            packages.insert(
                vec![text(name), text(package)],
                vec![Val::Int(position as i64)],
            );
        }
        for (package, one) in &group.desired {
            desired.insert(
                vec![text(name), text(package)],
                vec![
                    Val::Text(one.version.to_string()),
                    Val::Int(int(one.generation)?),
                    Val::text_or_null(one.write_id.as_deref()),
                    Val::text_or_null(one.replaced_version.as_ref().map(Version::as_str)),
                ],
            );
        }
    }
    tables.insert(HOST_GROUPS.name, rows);
    tables.insert(PACKAGES.name, packages);
    tables.insert(DESIRED.name, desired);
    Ok(())
}

/// The text of column `i` of a key: empty for one that is not text, which no key of these
/// tables is.
fn key_text(key: &[KeyVal], i: usize) -> String {
    key.get(i)
        .and_then(KeyVal::as_text)
        .unwrap_or_default()
        .to_owned()
}

/// The groups `tables` hold, read as the state file's reader reads them; a table they lack
/// reads as empty.
///
/// # Errors
/// [`StoreError::Invalid`]: a row holds what the reader refuses, the group named.
pub(crate) fn groups_of(tables: &Tables) -> Result<HostGroups, StoreError> {
    let refused = |why: String| StoreError::Invalid {
        path: place(HOST_GROUPS.name),
        source: DesiredError::Malformed(why),
    };
    let empty = Rows::new();
    let get = |table: &Table| tables.get(table.name).unwrap_or(&empty);
    let mut groups: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (key, values) in get(&HOST_GROUPS) {
        let names = ["generation", "policy", "by", "at_unix"];
        let group = groups.entry(key_text(key, 0)).or_default();
        for (name, value) in names.iter().zip(values) {
            group.insert((*name).to_owned(), value_json(value));
        }
        group.insert("packages".to_owned(), serde_json::Value::Array(Vec::new()));
    }
    let mut lists: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for (key, values) in get(&PACKAGES) {
        let position = values.first().and_then(Val::as_int).unwrap_or(-1);
        lists
            .entry(key_text(key, 0))
            .or_default()
            .push((position, key_text(key, 1)));
    }
    for (name, mut list) in lists {
        list.sort();
        let Some(group) = groups.get_mut(&name) else {
            return Err(refused(format!("the packages of {name}, a group not held")));
        };
        let packages = list.into_iter().map(|(_, p)| p.into()).collect();
        group.insert("packages".to_owned(), serde_json::Value::Array(packages));
    }
    let mut desired: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (key, values) in get(&DESIRED) {
        let name = key_text(key, 0);
        if !groups.contains_key(&name) {
            return Err(refused(format!(
                "a desired version of {name}, a group not held"
            )));
        }
        let mut one = serde_json::Map::new();
        let names = ["version", "generation", "write_id", "replaced_version"];
        for (field, value) in names.iter().zip(values) {
            if *value != Val::Null {
                one.insert((*field).to_owned(), value_json(value));
            }
        }
        desired
            .entry(name)
            .or_default()
            .insert(key_text(key, 1), one.into());
    }
    for (name, versions) in desired {
        groups
            .entry(name)
            .or_default()
            .insert("desired".to_owned(), versions.into());
    }
    HostGroups::read(serde_json::to_value(groups).unwrap_or_default()).map_err(refused)
}

/// A value as JSON: a counter as a number, else as [`json_of`] reads it (`NULL` as `null`).
fn value_json(value: &Val) -> serde_json::Value {
    match value {
        Val::Int(n) => (*n).into(),
        other => json_of(other).unwrap_or_default(),
    }
}

/// Puts the rows of `marks` into `tables`, under the tables' names.
///
/// # Errors
/// [`StoreError::Io`]: a counter is above `i64::MAX`, which the store refuses.
pub(crate) fn insert_marks(marks: &GroupsMarks, tables: &mut Tables) -> Result<(), StoreError> {
    let (mut minimums, mut services) = (Rows::new(), Rows::new());
    let (mut rollouts, mut done) = (Rows::new(), Rows::new());
    for (group, packages) in &marks.0 {
        for (package, one) in packages {
            let key = || vec![text(group.as_str()), text(package)];
            if let Some(version) = &one.min_version {
                minimums.insert(key(), vec![Val::Text(version.to_string())]);
            }
            let list = |acks: &[HaltAck]| {
                if acks.is_empty() {
                    Val::Null
                } else {
                    json(&acks)
                }
            };
            let row = vec![
                Val::text_or_null(one.last_good.as_ref().map(Version::as_str)),
                Val::Int(int(one.halt_generation)?),
                Val::Int(int(one.force_generation)?),
                one.force.as_ref().map_or(Val::Null, json),
                one.halt.as_ref().map_or(Val::Null, json),
                list(&one.halt_acked),
                list(&one.halt_seen),
            ];
            let none = [Val::Null, Val::Int(0), Val::Int(0)];
            if row[..3] != none || row[3..].iter().any(|v| *v != Val::Null) {
                services.insert(key(), row);
            }
            let rollout = &one.rollout;
            if !rollout.is_empty() {
                let (by, reason) = rollout.paused.as_ref().map_or((None, None), |p| {
                    (Some(p.by.as_str()), Some(p.reason.as_str()))
                });
                rollouts.insert(
                    key(),
                    vec![
                        Val::text_or_null(rollout.version.as_ref().map(Version::as_str)),
                        Val::Int(i64::from(rollout.entered)),
                        Val::text_or_null(by),
                        Val::text_or_null(reason),
                    ],
                );
                for node in &rollout.done {
                    let mut key = key();
                    key.push(text(node));
                    done.insert(key, vec![]);
                }
            }
        }
    }
    tables.insert(MIN_VERSIONS.name, minimums);
    tables.insert(SERVICES.name, services);
    tables.insert(ROLLOUTS.name, rollouts);
    tables.insert(ROLLOUT_DONE.name, done);
    Ok(())
}

/// The marks of the (group, package) of the row `at`, made at their default when none are held.
fn entry<'m>(marks: &'m mut GroupsMarks, at: &At<'_>) -> Result<&'m mut GroupMarks, OpsError> {
    let group: GroupName = at.key(0)?.parse().map_err(|e| at.no(e))?;
    Ok(marks
        .0
        .entry(group)
        .or_default()
        .entry(at.key(1)?)
        .or_default())
}

/// The marks `tables` hold: every value parsed again, a row that does not parse refused, naming
/// its table and its key; a table they lack reads as empty.
///
/// # Errors
/// [`OpsError::Invalid`]: a row holds what this build cannot read.
pub(crate) fn marks_of(tables: &Tables) -> Result<GroupsMarks, OpsError> {
    let empty = Rows::new();
    let rows = |table: &'static Table| {
        tables
            .get(table.name)
            .unwrap_or(&empty)
            .iter()
            .map(move |(key, values)| At {
                table: table.name,
                key,
                values,
            })
    };
    let mut marks = GroupsMarks::default();
    for at in rows(&MIN_VERSIONS) {
        let version = at
            .version(0)?
            .ok_or_else(|| at.no("the version is empty"))?;
        entry(&mut marks, &at)?.min_version = Some(version);
    }
    for at in rows(&SERVICES) {
        let one = entry(&mut marks, &at)?;
        one.last_good = at.version(0)?;
        one.halt_generation = at.count(1)?;
        one.force_generation = at.count(2)?;
        one.force = at.json(3)?;
        one.halt = at.json(4)?;
        one.halt_acked = at.json(5)?.unwrap_or_default();
        one.halt_seen = at.json(6)?.unwrap_or_default();
    }
    for at in rows(&ROLLOUTS) {
        let entered =
            u32::try_from(at.count(1)?).map_err(|_| at.no("the stages entered are too many"))?;
        let paused = match (at.text(2)?, at.text(3)?) {
            (Some(by), Some(reason)) => Some(Pause { by, reason }),
            _ => None,
        };
        let version = at.version(0)?;
        let rollout = &mut entry(&mut marks, &at)?.rollout;
        rollout.version = version;
        rollout.entered = entered;
        rollout.paused = paused;
    }
    for at in rows(&ROLLOUT_DONE) {
        let node = at.key(2)?;
        entry(&mut marks, &at)?.rollout.done.insert(node);
    }
    marks.check().map_err(|why| OpsError::Invalid {
        path: place(SERVICES.name),
        why,
    })?;
    Ok(marks)
}

#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
