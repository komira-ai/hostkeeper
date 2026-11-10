//! The registry of package repositories ([`crate::repositories`]) as the rows of the tables
//! that migration `0007` adds (`docs/design/coordinator-storage.md`, section 3.2), and back:
//! `repositories`, one row per repository holding its record as the `jsonb` the repositories
//! file holds, and `repository_generation`, one row, `id` 1, holding the registry's generation,
//! so that the generation survives the removal of the last repository.
//!
//! The tables are the last of the desired state's ([`super::state::DESIRED`]), written in the
//! same commit as the state it is set with. A registry at generation 0 with no repository has no
//! row. An older release never reads either table (`0007` is `may-run`). A load reads every row
//! through the file's reader and refuses a registry that does not read, naming the repository.

use std::collections::BTreeMap;

use super::rows::{KeyTy, KeyVal, Rows, Table, Ty, Val};
use super::state::{int, json_of, place};
use crate::desired::{DesiredError, StoreError};
use crate::repositories::{Kept, Registry};

/// The table of the repositories.
pub(crate) const TABLE: Table = Table {
    name: "repositories",
    keys: &[("name", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the registry's generation: the one row `id` = 1.
pub(crate) const GENERATION: Table = Table {
    name: "repository_generation",
    keys: &[("id", KeyTy::Int)],
    values: &[("generation", Ty::Int)],
};

/// The rows of `registry`: its repositories, and its generation unless it is 0.
///
/// # Errors
/// [`StoreError::Io`]: the generation is above `i64::MAX`, which the store refuses.
pub(crate) fn rows(registry: &Registry) -> Result<(Rows, Rows), StoreError> {
    let mut repositories = Rows::new();
    for (name, kept) in &registry.entries {
        repositories.insert(
            vec![KeyVal::Text(name.clone())],
            vec![Val::Json(kept.to_json())],
        );
    }
    let mut generation = Rows::new();
    if registry.generation > 0 {
        generation.insert(
            vec![KeyVal::Int(1)],
            vec![Val::Int(int(registry.generation)?)],
        );
    }
    Ok((repositories, generation))
}

/// The registry the rows hold, read as the repositories file's reader reads it.
///
/// # Errors
/// [`StoreError::Invalid`]: a row does not read, or the registry breaks a rule of
/// [`Registry::new`]; the error names the repository.
pub(crate) fn of(repositories: &Rows, generation: &Rows) -> Result<Registry, StoreError> {
    let refused = |why: String| StoreError::Invalid {
        path: place("repositories"),
        source: DesiredError::Malformed(why),
    };
    let generation = match generation
        .get(&vec![KeyVal::Int(1)])
        .and_then(|v| v.first())
    {
        None => 0,
        Some(Val::Int(n)) => {
            u64::try_from(*n).map_err(|_| refused("a negative generation".to_owned()))?
        }
        Some(_) => return Err(refused("a generation that is not a number".to_owned())),
    };
    let mut entries = BTreeMap::new();
    for (key, values) in repositories {
        let name = key.first().and_then(KeyVal::as_text).unwrap_or_default();
        let record = values.first().and_then(json_of).unwrap_or_default();
        let kept =
            Kept::from_json(&record).map_err(|why| refused(format!("the row {name}: {why}")))?;
        entries.insert(name.to_owned(), kept);
    }
    Registry::new(generation, entries).map_err(refused)
}

#[cfg(test)]
#[path = "repositories_tests.rs"]
mod tests;
