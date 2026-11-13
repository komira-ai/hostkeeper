//! The desired-state file's text: the single state of earlier builds, or the map of services,
//! each entry its state's keys and its registry (`digests`), with the host groups beside it
//! (`host_groups`, only while one is held besides the built-in `all` as the migration makes it), and back. A key the state does not have is refused,
//! so an earlier build refuses what it cannot read rather than lose it.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{Desired, DesiredError, Held};
use crate::host_groups::HostGroups;
use crate::registry::Registry;

/// One service's state as the file writes it: the desired state, then its registry when it holds
/// any.
#[derive(Serialize)]
struct EntryOut<'a> {
    #[serde(flatten)]
    desired: &'a Desired,
    #[serde(skip_serializing_if = "Option::is_none")]
    digests: Option<&'a Registry>,
}

/// The state file in its map form, as written.
#[derive(Serialize)]
struct MapFileOut<'a> {
    services: BTreeMap<&'a str, EntryOut<'a>>,
    #[serde(skip_serializing_if = "HostGroups::is_bare")]
    host_groups: &'a HostGroups,
}

/// The desired state and the registry of one service's entry in the file: the state's own keys,
/// read strictly, and `digests`.
fn parse_entry(value: serde_json::Value) -> Result<(Desired, Registry), DesiredError> {
    let malformed = |e: serde_json::Error| DesiredError::Malformed(e.to_string());
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(value).map_err(malformed)?;
    let digests = match fields.remove("digests") {
        Some(value) => Registry::read(value)?,
        None => Registry::default(),
    };
    let desired: Desired = serde_json::from_value(fields.into()).map_err(malformed)?;
    desired.checked()?;
    Ok((desired, digests))
}

/// The services in `bytes`: a map file, or the single state of earlier builds as the
/// legacy service's (`legacy`; with none set that file is refused).
pub(crate) fn parse_file(bytes: &[u8], legacy: Option<&str>) -> Result<Held, DesiredError> {
    let malformed = |e: serde_json::Error| DesiredError::Malformed(e.to_string());
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(malformed)?;
    let mut held = Held::default();
    let Some(services) = value.get("services").cloned() else {
        let name = legacy.ok_or(DesiredError::NoLegacyService)?;
        let (desired, digests) = parse_entry(value)?;
        held.insert(name, desired, digests);
        return Ok(held);
    };
    if let Some(other) = value.as_object().and_then(|map| {
        map.keys()
            .find(|key| !matches!(key.as_str(), "services" | "host_groups"))
    }) {
        return Err(DesiredError::Malformed(format!("unknown field `{other}`")));
    }
    if let Some(groups) = value.get("host_groups").cloned() {
        held.host_groups = HostGroups::read(groups).map_err(DesiredError::Malformed)?;
    }
    let services: BTreeMap<String, serde_json::Value> =
        serde_json::from_value(services).map_err(malformed)?;
    for (name, entry) in services {
        let (desired, digests) = parse_entry(entry)?;
        held.insert(&name, desired, digests);
    }
    Ok(held)
}

/// The text of the file holding `held`: the single state of earlier builds while `legacy` is the
/// only service and no host group is held, which earlier builds read and whose reader refuses the
/// map form; the map otherwise.
///
/// # Errors
/// The state could not be serialised.
pub(crate) fn encode(held: &Held, legacy: Option<&str>) -> serde_json::Result<Vec<u8>> {
    let encoded = match held.desired.iter().collect::<Vec<_>>().as_slice() {
        [(name, only)] if Some(name.as_str()) == legacy && held.host_groups.is_bare() => {
            serde_json::to_vec_pretty(&held.entry(name, only))
        }
        _ => serde_json::to_vec_pretty(&MapFileOut {
            services: held
                .desired
                .iter()
                .map(|(name, desired)| (name.as_str(), held.entry(name, desired)))
                .collect(),
            host_groups: &held.host_groups,
        }),
    };
    let mut text = encoded?;
    text.push(b'\n');
    Ok(text)
}

impl Held {
    fn insert(&mut self, name: &str, desired: Desired, digests: Registry) {
        self.desired.insert(name.to_owned(), desired);
        if !digests.is_empty() {
            self.digests.insert(name.to_owned(), digests);
        }
    }

    /// The entry of `name`, whose state is `desired`, as the file writes it.
    fn entry<'a>(&'a self, name: &str, desired: &'a Desired) -> EntryOut<'a> {
        EntryOut {
            desired,
            digests: self.digests.get(name).filter(|r| !r.is_empty()),
        }
    }
}
