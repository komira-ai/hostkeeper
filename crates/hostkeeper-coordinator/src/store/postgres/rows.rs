//! The rows of the store's tables, as values in memory: what a commit compares and a load reads
//! (`docs/design/coordinator-storage.md`, section 4.1).
//!
//! A part of the state (the desired state, the operations state, the verdicts) is turned into
//! [`Tables`]: for each of its tables, its rows by primary key. A commit turns the state before
//! and after into rows and writes only the rows that differ ([`changes`]): a key gone is
//! deleted, a key new or changed is written over. A load reads every row of each table and turns
//! the rows back into the state, through the same checks a file's reader makes.

use std::collections::BTreeMap;
use std::error::Error;

use bytes::BytesMut;
use postgres::Row;
use postgres::types::{IsNull, ToSql, Type, to_sql_checked};

/// One value of a column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    /// SQL `NULL`.
    Null,
    /// `text`.
    Text(String),
    /// `bigint`.
    Int(i64),
    /// `smallint`.
    Small(i16),
    /// `boolean`.
    Bool(bool),
    /// `jsonb`.
    Json(serde_json::Value),
    /// `bytea`.
    Bytes(Vec<u8>),
    /// `text[]`.
    Texts(Vec<String>),
}

impl Val {
    /// The text, or `NULL` for none.
    pub(crate) fn text_or_null(text: Option<&str>) -> Val {
        text.map_or(Val::Null, |text| Val::Text(text.to_owned()))
    }

    /// The text of a `text` value; `None` for any other.
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Val::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The integer of a `bigint` value.
    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Val::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// The JSON of a `jsonb` value.
    pub(crate) fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            Val::Json(value) => Some(value),
            _ => None,
        }
    }
}

impl From<&KeyVal> for Val {
    fn from(key: &KeyVal) -> Val {
        match key {
            KeyVal::Text(text) => Val::Text(text.clone()),
            KeyVal::Int(n) => Val::Int(*n),
            KeyVal::Bytes(bytes) => Val::Bytes(bytes.clone()),
        }
    }
}

impl ToSql for Val {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            Val::Null => Ok(IsNull::Yes),
            Val::Text(text) => text.to_sql(ty, out),
            Val::Int(n) => n.to_sql(ty, out),
            Val::Small(n) => n.to_sql(ty, out),
            Val::Bool(b) => b.to_sql(ty, out),
            Val::Json(value) => value.to_sql(ty, out),
            Val::Bytes(bytes) => bytes.to_sql(ty, out),
            Val::Texts(texts) => texts.to_sql(ty, out),
        }
    }

    fn accepts(ty: &Type) -> bool {
        // Each value is written to the column its table names for it; the inner type checks
        // the column's type as it writes.
        let _ = ty;
        true
    }

    to_sql_checked!();
}

/// One value of a primary key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeyVal {
    /// `text`.
    Text(String),
    /// `bigint`.
    Int(i64),
    /// `bytea`.
    Bytes(Vec<u8>),
}

impl KeyVal {
    /// The text of a `text` key.
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            KeyVal::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// The type of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Text,
    Int,
    Small,
    Bool,
    Json,
    Bytes,
    Texts,
}

/// The type of a column of a primary key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyTy {
    Text,
    Int,
    Bytes,
}

/// A table: its name, the columns of its primary key, and its other columns, each in order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Table {
    pub(crate) name: &'static str,
    pub(crate) keys: &'static [(&'static str, KeyTy)],
    pub(crate) values: &'static [(&'static str, Ty)],
}

/// The rows of one table: the values of the columns past the key, by the key.
pub(crate) type Rows = BTreeMap<Vec<KeyVal>, Vec<Val>>;

/// The rows of a part's tables, by table name.
pub(crate) type Tables = BTreeMap<&'static str, Rows>;

/// One statement and its parameters, owned, so that it can be handed to the writer's thread.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stmt {
    pub(crate) sql: String,
    pub(crate) params: Vec<Val>,
}

impl Stmt {
    /// The parameters as the client takes them.
    pub(crate) fn refs(&self) -> Vec<&(dyn ToSql + Sync)> {
        self.params
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect()
    }
}

impl Table {
    fn key_names(&self) -> Vec<&'static str> {
        self.keys.iter().map(|(name, _)| *name).collect()
    }

    fn value_names(&self) -> Vec<&'static str> {
        self.values.iter().map(|(name, _)| *name).collect()
    }

    fn all_names(&self) -> Vec<&'static str> {
        let mut all = self.key_names();
        all.extend(self.value_names());
        all
    }

    /// `SELECT` of every column, in key order.
    pub(crate) fn select(&self) -> String {
        format!(
            "SELECT {} FROM {} ORDER BY {}",
            self.all_names().join(", "),
            self.name,
            self.key_names().join(", ")
        )
    }

    /// The statement that writes the row `key`, `values` over any row of that key.
    pub(crate) fn upsert(&self, key: &[KeyVal], values: &[Val]) -> Stmt {
        let all = self.all_names();
        let places: Vec<String> = (1..=all.len()).map(|n| format!("${n}")).collect();
        let rest = self.value_names();
        let on_conflict = if rest.is_empty() {
            "DO NOTHING".to_owned()
        } else {
            format!(
                "DO UPDATE SET {}",
                rest.iter()
                    .map(|c| format!("{c} = EXCLUDED.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Stmt {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) {on_conflict}",
                self.name,
                all.join(", "),
                places.join(", "),
                self.key_names().join(", ")
            ),
            params: key.iter().map(Val::from).chain(values.iter().cloned()).collect(),
        }
    }

    /// The statement that deletes the row `key`.
    pub(crate) fn delete(&self, key: &[KeyVal]) -> Stmt {
        let conditions: Vec<String> = self
            .key_names()
            .iter()
            .enumerate()
            .map(|(n, c)| format!("{c} = ${}", n + 1))
            .collect();
        Stmt {
            sql: format!(
                "DELETE FROM {} WHERE {}",
                self.name,
                conditions.join(" AND ")
            ),
            params: key.iter().map(Val::from).collect(),
        }
    }

    /// The rows `rows` read by [`Table::select`].
    ///
    /// # Errors
    /// A column's value is not of the type the table names for it.
    pub(crate) fn rows_of(&self, rows: &[Row]) -> Result<Rows, postgres::Error> {
        let mut out = Rows::new();
        for row in rows {
            let mut key = Vec::with_capacity(self.keys.len());
            for (i, (_, ty)) in self.keys.iter().enumerate() {
                key.push(match ty {
                    KeyTy::Text => KeyVal::Text(row.try_get(i)?),
                    KeyTy::Int => KeyVal::Int(row.try_get(i)?),
                    KeyTy::Bytes => KeyVal::Bytes(row.try_get(i)?),
                });
            }
            let mut values = Vec::with_capacity(self.values.len());
            for (i, (_, ty)) in self.values.iter().enumerate() {
                values.push(read(row, self.keys.len() + i, *ty)?);
            }
            out.insert(key, values);
        }
        Ok(out)
    }
}

/// The value of column `i` of `row`, of type `ty`.
fn read(row: &Row, i: usize, ty: Ty) -> Result<Val, postgres::Error> {
    fn or_null<T>(value: Option<T>, f: impl FnOnce(T) -> Val) -> Val {
        value.map_or(Val::Null, f)
    }
    Ok(match ty {
        Ty::Text => or_null(row.try_get::<_, Option<String>>(i)?, Val::Text),
        Ty::Int => or_null(row.try_get::<_, Option<i64>>(i)?, Val::Int),
        Ty::Small => or_null(row.try_get::<_, Option<i16>>(i)?, Val::Small),
        Ty::Bool => or_null(row.try_get::<_, Option<bool>>(i)?, Val::Bool),
        Ty::Json => or_null(row.try_get::<_, Option<serde_json::Value>>(i)?, Val::Json),
        Ty::Bytes => or_null(row.try_get::<_, Option<Vec<u8>>>(i)?, Val::Bytes),
        Ty::Texts => or_null(row.try_get::<_, Option<Vec<String>>>(i)?, Val::Texts),
    })
}

/// The statements that take the rows of `tables` from `before` to `after`: in the reverse order
/// of `tables`, every row deleted that is gone; then, in their order, every row written that is
/// new or changed. A row a later table refers to is so deleted after it and written before it,
/// and a unique value a deleted row held is free before a written row takes it.
pub(crate) fn changes(tables: &[Table], before: &Tables, after: &Tables) -> Vec<Stmt> {
    let empty = Rows::new();
    let mut writes = Vec::new();
    let mut deletes = Vec::new();
    for table in tables {
        let old = before.get(table.name).unwrap_or(&empty);
        let new = after.get(table.name).unwrap_or(&empty);
        for (key, values) in new {
            if old.get(key) != Some(values) {
                writes.push(table.upsert(key, values));
            }
        }
        let gone: Vec<Stmt> = old
            .keys()
            .filter(|key| !new.contains_key(*key))
            .map(|key| table.delete(key))
            .collect();
        deletes.push(gone);
    }
    let mut out: Vec<Stmt> = deletes.into_iter().rev().flatten().collect();
    out.extend(writes);
    out
}

/// `memory` with the change from `before` to `after` laid over it, row by row: a row the change
/// made takes the value it made, unless memory changed that row since `before`, whose newer
/// value stays. What a commit of unknown outcome that landed gives memory (section 4.4).
pub(crate) fn laid_over(memory: &Tables, before: &Tables, after: &Tables) -> Tables {
    let empty = Rows::new();
    let mut out = memory.clone();
    let names: std::collections::BTreeSet<&'static str> =
        before.keys().chain(after.keys()).copied().collect();
    for name in names {
        let old = before.get(name).unwrap_or(&empty);
        let new = after.get(name).unwrap_or(&empty);
        let rows = out.entry(name).or_default();
        let keys: std::collections::BTreeSet<&Vec<KeyVal>> = old.keys().chain(new.keys()).collect();
        for key in keys {
            if old.get(key) == new.get(key) || rows.get(key) != old.get(key) {
                continue;
            }
            match new.get(key) {
                Some(values) => rows.insert(key.clone(), values.clone()),
                None => rows.remove(key),
            };
        }
    }
    out
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
