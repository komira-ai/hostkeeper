//! Each service's write record ([`crate::write_record`]) as the four columns migration `0003` adds
//! to `desired` (`docs/design/coordinator-storage.md`, section 3.2): `generation`, `write_id`,
//! `replaced_version` and `replaced_min_up`, after the columns of `0001`, and back.
//!
//! A service never written has the default record (generation 0, nothing else), which is what a
//! row written before `0003` reads as once it has run: a load reads it as no record, and a commit writes
//! it as those defaults. A load parses every value again, as a start on any other table does,
//! and refuses a row whose record does not parse, naming the service.

use std::collections::BTreeMap;

use hostkeeper_wire::version::Version;

use super::rows::{KeyVal, Rows, Val};
use super::state::{int, place};
use crate::desired::{DesiredError, MAX_COUNT, RoleLimits, StoreError, is_role_name};
use crate::write_record::{Replaced, WriteId, WriteRecord};

/// Where the columns of `0003` start in a row of `desired`: after the five of `0001`.
const FIRST: usize = 5;

/// The columns of `record` (the default record for none), in their order.
///
/// # Errors
/// [`StoreError::Io`]: the generation is above `i64::MAX`, which the store refuses.
pub(crate) fn columns(record: Option<&WriteRecord>) -> Result<[Val; 4], StoreError> {
    let record = record.cloned().unwrap_or_default();
    let (version, min_up) = match record.replaced {
        Some(Replaced { version, min_up }) => {
            let map = min_up
                .iter()
                .map(|(role, n)| (role.to_owned(), n.into()))
                .collect::<serde_json::Map<_, _>>();
            (
                Val::Text(version.as_str().to_owned()),
                Val::Json(map.into()),
            )
        }
        None => (Val::Null, Val::Null),
    };
    Ok([
        Val::Int(int(record.generation)?),
        Val::text_or_null(record.write_id.as_ref().map(WriteId::as_str)),
        version,
        min_up,
    ])
}

/// The record of each service the rows of `desired` hold; a service at the default record is not
/// listed.
///
/// # Errors
/// [`StoreError::Invalid`]: a row's record does not parse; the error names its service.
pub(crate) fn records_of(rows: &Rows) -> Result<BTreeMap<String, WriteRecord>, StoreError> {
    let mut out = BTreeMap::new();
    for (key, values) in rows {
        let service = key.first().and_then(KeyVal::as_text).unwrap_or_default();
        let refused = |why: &str| StoreError::Invalid {
            path: place("desired"),
            source: DesiredError::Malformed(format!("the row {service}: {why}")),
        };
        let column = |i: usize| values.get(FIRST + i).unwrap_or(&Val::Null);
        let generation = match column(0) {
            Val::Int(n) => u64::try_from(*n).map_err(|_| refused("a negative generation"))?,
            _ => return Err(refused("no generation")),
        };
        let write_id = match column(1) {
            Val::Null => None,
            Val::Text(text) => {
                Some(WriteId::parse(text).map_err(|e| refused(&format!("its write id: {e}")))?)
            }
            _ => return Err(refused("a write id that is not text")),
        };
        let replaced = match (column(2), column(3)) {
            (Val::Null, Val::Null) => None,
            (Val::Text(version), Val::Json(min_up)) => Some(Replaced {
                version: Version::parse(version)
                    .map_err(|e| refused(&format!("its replaced version: {e}")))?,
                min_up: min_up_of(min_up).map_err(|why| refused(&why))?,
            }),
            _ => {
                return Err(refused(
                    "a replaced version without its minimums, or minimums alone",
                ));
            }
        };
        let record = WriteRecord {
            generation,
            replaced,
            write_id,
        };
        if record != WriteRecord::default() {
            out.insert(service.to_owned(), record);
        }
    }
    Ok(out)
}

/// The replaced minimums of `value`, checked as a desired state's are: every key a role name and
/// every count within [`MAX_COUNT`].
fn min_up_of(value: &serde_json::Value) -> Result<RoleLimits, String> {
    let limits: RoleLimits =
        serde_json::from_value(value.clone()).map_err(|e| format!("its replaced minimums: {e}"))?;
    if let Some((role, _)) = limits.iter().find(|(role, _)| !is_role_name(role)) {
        return Err(format!(
            "its replaced minimums: {role:?} is not a role name"
        ));
    }
    if limits.iter().any(|(_, n)| n > MAX_COUNT) {
        return Err(format!(
            "its replaced minimums: a count is over {MAX_COUNT}"
        ));
    }
    Ok(limits)
}

#[cfg(test)]
#[path = "generations_tests.rs"]
mod tests;
