//! The reader of one row of the operations state's tables at load ([`At`]): its columns read by
//! their kind, a value that is not what the column holds refused, naming the table and the key.

use hostkeeper_wire::version::Version;
use serde::de::DeserializeOwned;

use super::rows::{KeyVal, Val};
use super::state::place;
use crate::ops::OpsError;

/// How one row of the operations state's tables is refused at load.
pub(super) fn refused(table: &str, key: &[KeyVal], why: impl std::fmt::Display) -> OpsError {
    let key: Vec<String> = key
        .iter()
        .map(|k| match k {
            KeyVal::Text(text) => text.clone(),
            KeyVal::Int(n) => n.to_string(),
        })
        .collect();
    OpsError::Invalid {
        path: place(table),
        why: format!("the row {}: {why}", key.join("/")),
    }
}

/// The reader of one row of the operations state: its table, its key and its values.
pub(super) struct At<'a> {
    pub(super) table: &'static str,
    pub(super) key: &'a [KeyVal],
    pub(super) values: &'a [Val],
}

impl At<'_> {
    pub(super) fn no(&self, why: impl std::fmt::Display) -> OpsError {
        refused(self.table, self.key, why)
    }

    pub(super) fn key(&self, i: usize) -> Result<String, OpsError> {
        self.key
            .get(i)
            .and_then(KeyVal::as_text)
            .map(str::to_owned)
            .ok_or_else(|| self.no("a key column is not text"))
    }

    pub(super) fn text(&self, i: usize) -> Result<Option<String>, OpsError> {
        match self.values.get(i) {
            Some(Val::Text(text)) => Ok(Some(text.clone())),
            Some(Val::Null) | None => Ok(None),
            Some(_) => Err(self.no("a column is not text")),
        }
    }

    pub(super) fn required(&self, i: usize) -> Result<String, OpsError> {
        self.text(i)?
            .ok_or_else(|| self.no("a column that is required is empty"))
    }

    pub(super) fn version(&self, i: usize) -> Result<Option<Version>, OpsError> {
        self.text(i)?
            .map(|text| Version::parse(&text).map_err(|e| self.no(e)))
            .transpose()
    }

    pub(super) fn count(&self, i: usize) -> Result<u64, OpsError> {
        match self.values.get(i) {
            Some(Val::Int(n)) => u64::try_from(*n).map_err(|_| self.no("a counter is negative")),
            _ => Err(self.no("a counter is not a number")),
        }
    }

    pub(super) fn position(&self, i: usize) -> Result<i64, OpsError> {
        self.values
            .get(i)
            .and_then(Val::as_int)
            .ok_or_else(|| self.no("a position is not a number"))
    }

    pub(super) fn json<T: DeserializeOwned>(&self, i: usize) -> Result<Option<T>, OpsError> {
        match self.values.get(i) {
            Some(Val::Json(value)) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|e| self.no(e)),
            Some(Val::Null) | None => Ok(None),
            Some(_) => Err(self.no("a column is not JSON")),
        }
    }
}
