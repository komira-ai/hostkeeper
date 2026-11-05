//! The operator actions of the JSON route `/v1/operator` ([`operate`]): exclusions, labels and
//! the clear of a halt, each validated before anything changes and written to the ops store
//! ([`crate::ops`]) with its audit entry.

use std::collections::BTreeMap;

use hostkeeper_wire::HostId;
use serde::Deserialize;

use crate::ops::{
    AuditEntry, Exclusion, HaltAck, HostOps, MAX_EXCLUDED, MAX_HOSTS, MAX_LABEL_BYTES, MAX_LABELS,
    MAX_REASON_BYTES, OpsError, OpsStore, Write,
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

fn service<'a>(services: &Services, name: &'a str) -> Result<&'a str, OperateError> {
    services
        .get(name)
        .map(|_| name)
        .ok_or(OperateError::Invalid(
            "service is not one this coordinator manages",
        ))
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

/// Does the operator action in `body`, as `by`, in `store`. The caller has already checked
/// who `by` is; the body is validated here before anything changes.
///
/// # Errors
/// [`OperateError::Invalid`]: the body is no action, or names an unknown service, a bad node
/// id, a bad reason or bad labels. [`OperateError::Store`]: the file could not be written.
pub fn operate(
    store: &OpsStore,
    services: &Services,
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
                service: service(services, &name)?.to_owned(),
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
            let (name, id) = (service(services, &name)?.to_owned(), node(&node_id)?);
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
            let name = service(services, &name)?.to_owned();
            let why = reason(&why)?;
            // Written first: a clear that cannot be written leaves the halt standing. The
            // reports of the halt and of every other host that reported while it stood are
            // acknowledged (one per host, the latest) so that a host, which keeps reporting
            // until it updates again, does not raise the halt again: a clear clears the service.
            // The halt is the one standing when the change is made, under the writers' lock:
            // one read before could have been cleared by another clear since, and a halt raised
            // after that one would be cleared with the old report's acknowledgement.
            store.decide_change::<OperateError>(Write::BeforeMemory, |state| {
                let marks = state.services.entry(name.clone()).or_default();
                let halt = marks
                    .halt
                    .take()
                    .ok_or(OperateError::Invalid("the service is not halted"))?;
                let mut entry = AuditEntry::new(by, "clear_halt");
                entry.service = Some(name);
                // An operator's halt names no host.
                entry.node_id = Some(halt.node_id.clone()).filter(|node| !node.is_empty());
                entry.reason = Some(why);
                let seen = std::mem::take(&mut marks.halt_seen);
                let reports = std::iter::once(HaltAck::from(halt)).chain(seen);
                for report in reports {
                    marks.halt_acked.retain(|ack| ack.node_id != report.node_id);
                    marks.halt_acked.push(report);
                }
                marks.halt_generation = marks.halt_generation.saturating_add(1);
                Ok(entry)
            })?
        }
    })
}
