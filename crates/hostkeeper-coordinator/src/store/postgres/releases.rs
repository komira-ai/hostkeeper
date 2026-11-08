//! The releases of the desired state ([`crate::releases`]) as the rows of the table `releases`
//! that migration `0004` adds (`docs/design/coordinator-storage.md`, section 3.2), and back: one
//! row per service and version, its targets as the `jsonb` the releases file holds and the
//! digest of its release index, `NULL` for none.
//!
//! The table is the last of the desired state's ([`super::state::DESIRED`]), written in the same
//! commit as the state it is set with. A release written once is never changed, so a commit
//! writes only the row of a new release, and deletes only one dropped at the cap. An older
//! release never reads the table (`0004` is `may-run`): it reads the version as set with neither
//! a package digest nor an index. A load reads every row through the releases file's reader and
//! refuses one that does not read, naming its service and version.

use std::sync::Arc;

use serde_json::json;

use super::rows::{KeyTy, KeyVal, Rows, Table, Ty, Val};
use super::state::place;
use crate::desired::{DesiredError, StoreError};
use crate::releases::{Books, Release};

/// The table of the releases.
pub(crate) const TABLE: Table = Table {
    name: "releases",
    keys: &[("service", KeyTy::Text), ("version", KeyTy::Text)],
    values: &[("targets", Ty::Json), ("release_index_digest", Ty::Text)],
};

/// The rows of `books`.
pub(crate) fn rows(books: &Books) -> Rows {
    let mut rows = Rows::new();
    for (service, book) in books {
        for (version, release) in book {
            let stored = release.to_json();
            let digest = stored
                .get("release_index_digest")
                .and_then(serde_json::Value::as_str);
            rows.insert(
                vec![KeyVal::Text(service.clone()), KeyVal::Text(version.clone())],
                vec![
                    Val::Json(stored["targets"].clone()),
                    Val::text_or_null(digest),
                ],
            );
        }
    }
    rows
}

/// The releases the rows `rows` hold, read as the releases file's reader reads a release.
///
/// # Errors
/// [`StoreError::Invalid`]: a row does not read; the error names its service and version.
pub(crate) fn of(rows: &Rows) -> Result<Books, StoreError> {
    let mut books = Books::new();
    for (key, values) in rows {
        let text = |i: usize| key.get(i).and_then(KeyVal::as_text).unwrap_or_default();
        let (service, version) = (text(0), text(1));
        let mut stored = json!({});
        if let Some(Val::Json(targets)) = values.first() {
            stored["targets"] = targets.clone();
        }
        if let Some(Val::Text(digest)) = values.get(1) {
            stored["release_index_digest"] = digest.as_str().into();
        }
        let release = Release::from_json(&stored).map_err(|why| StoreError::Invalid {
            path: place("releases"),
            source: DesiredError::Malformed(format!("the row {service} {version}: {why}")),
        })?;
        books
            .entry(service.to_owned())
            .or_default()
            .insert(version.to_owned(), Arc::new(release));
    }
    Ok(books)
}
