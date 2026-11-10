//! The operator actions of the JSON route `/v1/operator` ([`operate`]): exclusions, labels and
//! the clear of a halt, each validated before anything changes and written to the ops store
//! ([`crate::ops`]) with its audit entry.

use std::collections::BTreeMap;

use hostkeeper_wire::HostId;
use serde::Deserialize;

use crate::group_reach::listing;
use crate::host_groups::HostGroups;
use crate::ops::{
    AuditEntry, Exclusion, Halt, HaltAck, HostOps, MAX_EXCLUDED, MAX_HOSTS, MAX_LABEL_BYTES,
    MAX_LABELS, MAX_REASON_BYTES, OpsError, OpsStore, ServiceOps, Write,
};
use crate::service::Unauthorized;
use crate::services::Services;

/// An operator action: `{"action": "<name>", ...}`, the name from a closed set.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// Takes a host out of a service: its beats are not counted and it is not ordered.
    Exclude {
        service: String,
        node_id: String,
        reason: String,
    },
    /// Puts it back.
    Include { service: String, node_id: String },
    /// Replaces the labels of a host (none removes them all).
    SetLabels {
        node_id: String,
        #[serde(deserialize_with = "labels_once")]
        labels: BTreeMap<String, String>,
    },
    /// Lifts the halt of a service: orders resume, and the report that raised it is not
    /// raised again.
    ClearHalt { service: String, reason: String },
}

/// A map of labels in which no key is given twice.
fn labels_once<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Once;

    impl<'de> serde::de::Visitor<'de> for Once {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object of labels")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut labels = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if labels.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("a label key is given twice"));
                }
            }
            Ok(labels)
        }
    }

    deserializer.deserialize_map(Once)
}

/// Why an operator action was not done.
#[derive(Debug, thiserror::Error)]
pub enum OperateError {
    /// The caller has no valid operator token.
    #[error(transparent)]
    Unauthorized(#[from] Unauthorized),
    /// The body is no action this coordinator accepts; the text says which part.
    #[error("{0}")]
    Invalid(&'static str),
    /// The file already holds the most of something it keeps; the constant that sets the
    /// most is named.
    #[error(
        "the ops file holds at most {max} {what}: remove some, or raise the constant in ops.rs"
    )]
    Capped {
        /// What there is too much of, with the constant.
        what: &'static str,
        /// The most.
        max: usize,
    },
    /// The ops file could not be written.
    #[error(transparent)]
    Store(#[from] OpsError),
}

fn node(text: &str) -> Result<String, OperateError> {
    text.parse::<HostId>()
        .map(|id| id.to_string())
        .map_err(|_| OperateError::Invalid("node_id is not a node id"))
}

/// `name` if the coordinator manages it: the rollout file defines it, or a host group lists it.
fn service<'a>(
    services: &Services,
    groups: &HostGroups,
    name: &'a str,
) -> Result<&'a str, OperateError> {
    (services.get(name).is_some() || !listing(groups, name).is_empty())
        .then_some(name)
        .ok_or(OperateError::Invalid(
            "service is not one this coordinator manages",
        ))
}

/// Lifts the halt standing in `marks`, as a clear does: the halt's report and every report kept
/// while it stood are acknowledged, one per host, so that a host that keeps reporting until it
/// updates again does not raise the halt again, and the generation counts. Answers the halt that
/// stood; none when there was none.
fn clear_standing(marks: &mut ServiceOps) -> Option<Halt> {
    let halt = marks.halt.take()?;
    let seen = std::mem::take(&mut marks.halt_seen);
    let reports = std::iter::once(HaltAck::from(halt.clone())).chain(seen);
    for report in reports {
        marks.halt_acked.retain(|ack| ack.node_id != report.node_id);
        marks.halt_acked.push(report);
    }
    marks.halt_generation = marks.halt_generation.saturating_add(1);
    Some(halt)
}

/// The `clear_halt` entry of the halt `halt` of `service` that `by` cleared, with `reason`, in
/// `group`.
fn cleared(
    by: &str,
    service: &str,
    group: Option<&crate::host_groups::GroupName>,
    halt: &Halt,
    reason: &str,
) -> AuditEntry {
    let mut entry = AuditEntry::new(by, "clear_halt");
    entry.service = Some(service.to_owned());
    // An operator's halt names no host.
    entry.node_id = Some(halt.node_id.clone()).filter(|node| !node.is_empty());
    entry.host_group = group.map(ToString::to_string);
    entry.reason = Some(reason.to_owned());
    entry
}

pub(crate) fn reason(text: &str) -> Result<String, OperateError> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_REASON_BYTES || text.chars().any(char::is_control) {
        return Err(OperateError::Invalid(
            "reason must be 1 to 256 bytes with no control character",
        ));
    }
    Ok(text.to_owned())
}

/// Writes `exclusion`, audited as `entry`: it replaces an exclusion of the same host and
/// service, and is refused with `capped` when the file holds [`MAX_EXCLUDED`] others.
///
/// # Errors
/// `capped`, or the store's error as `E`.
pub(crate) fn exclude<E: From<OpsError>>(
    store: &OpsStore,
    exclusion: Exclusion,
    entry: AuditEntry,
    capped: E,
) -> Result<AuditEntry, E> {
    store.try_change(Write::BeforeMemory, entry, |state| {
        let before = state.excluded.len();
        state.excluded.retain(|e| {
            (e.service.as_str(), e.node_id.as_str())
                != (exclusion.service.as_str(), exclusion.node_id.as_str())
        });
        let known = state.excluded.len() < before;
        if !known && before >= MAX_EXCLUDED {
            return Err(capped);
        }
        state.excluded.push(exclusion);
        Ok(())
    })
}

pub(crate) fn label(text: &str) -> bool {
    (1..=MAX_LABEL_BYTES).contains(&text.len())
        && text
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Does the operator action in `body`, as `by`, in `store`, over the services `services` and the
/// host groups `groups` of the coordinator (none: a coordinator without groups). The caller has
/// already checked who `by` is; the body is validated here before anything changes. A service is
/// managed when the rollout file defines it or a group lists it, and a clear of a halt, with
/// groups, clears the halt of every group that lists the service ([`clear_in_groups`]).
///
/// # Errors
/// [`OperateError::Invalid`]: the body is no action, or names an unknown service, a bad node
/// id, a bad reason or bad labels. [`OperateError::Store`]: the file could not be written.
pub(crate) fn operate(
    store: &OpsStore,
    services: &Services,
    groups: &HostGroups,
    by: &str,
    body: &[u8],
) -> Result<AuditEntry, OperateError> {
    let action: Action = serde_json::from_slice(body)
        .map_err(|_| OperateError::Invalid("the body is not an operator action"))?;
    Ok(match action {
        Action::Exclude {
            service: name,
            node_id,
            reason: why,
        } => {
            let exclusion = Exclusion {
                service: service(services, groups, &name)?.to_owned(),
                node_id: node(&node_id)?,
                reason: reason(&why)?,
            };
            let mut entry = AuditEntry::new(by, "exclude");
            entry.service = Some(exclusion.service.clone());
            entry.node_id = Some(exclusion.node_id.clone());
            entry.reason = Some(exclusion.reason.clone());
            let capped = OperateError::Capped {
                what: "exclusions (MAX_EXCLUDED)",
                max: MAX_EXCLUDED,
            };
            exclude(store, exclusion, entry, capped)?
        }
        Action::Include {
            service: name,
            node_id,
        } => {
            let (name, id) = (
                service(services, groups, &name)?.to_owned(),
                node(&node_id)?,
            );
            let mut entry = AuditEntry::new(by, "include");
            entry.service = Some(name.clone());
            entry.node_id = Some(id.clone());
            store.change(Write::BeforeMemory, entry, |state| {
                state.excluded.retain(|e| {
                    (e.service.as_str(), e.node_id.as_str()) != (name.as_str(), id.as_str())
                });
            })?
        }
        Action::SetLabels { node_id, labels } => {
            let id = node(&node_id)?;
            if labels.len() > MAX_LABELS || !labels.iter().all(|(k, v)| label(k) && label(v)) {
                return Err(OperateError::Invalid(
                    "labels are at most 8, each key and value 1 to 32 bytes of [a-z0-9-]",
                ));
            }
            let mut entry = AuditEntry::new(by, "set_labels");
            entry.node_id = Some(id.clone());
            store.try_change(Write::BeforeMemory, entry, |state| {
                if labels.is_empty() {
                    state.hosts.remove(&id);
                } else {
                    let grows = !state.hosts.contains_key(&id);
                    if grows && state.hosts.len() >= MAX_HOSTS {
                        return Err(OperateError::Capped {
                            what: "hosts with labels (MAX_HOSTS)",
                            max: MAX_HOSTS,
                        });
                    }
                    state.hosts.insert(id, HostOps { labels });
                }
                Ok(())
            })?
        }
        Action::ClearHalt {
            service: name,
            reason: why,
        } => {
            let name = service(services, groups, &name)?.to_owned();
            let why = reason(&why)?;
            if groups.is_empty() {
                // Written first: a clear that cannot be written leaves the halt standing. The
                // halt is the one standing when the change is made, under the writers' lock: one
                // read before could have been cleared by another clear since, and a halt raised
                // after that one would be cleared with the old report's acknowledgement.
                store.decide_change::<OperateError>(Write::BeforeMemory, |state| {
                    let marks = state.services.entry(name.clone()).or_default();
                    let halt = clear_standing(marks)
                        .ok_or(OperateError::Invalid("the service is not halted"))?;
                    Ok(cleared(by, &name, None, &halt, &why))
                })?
            } else {
                clear_in_groups(store, groups, by, &name, &why)?
            }
        }
    })
}

/// Clears the halt of `service` in every group of `groups` that lists it and has one standing, in
/// one change, as [`clear_standing`] does, one entry for each group naming it. Answers the first.
///
/// # Errors
/// [`OperateError::Invalid`] when no such group is halted; the store's error.
fn clear_in_groups(
    store: &OpsStore,
    groups: &HostGroups,
    by: &str,
    service: &str,
    reason: &str,
) -> Result<AuditEntry, OperateError> {
    let mut entries = store.decide_changes::<OperateError>(Write::BeforeMemory, |state| {
        let mut entries = Vec::new();
        for group in listing(groups, service) {
            let halt = state.edit_marks_in(Some(&group), service, clear_standing);
            entries.extend(halt.map(|halt| cleared(by, service, Some(&group), &halt, reason)));
        }
        if entries.is_empty() {
            return Err(OperateError::Invalid("the service is not halted"));
        }
        Ok(entries)
    })?;
    Ok(entries.swap_remove(0))
}
