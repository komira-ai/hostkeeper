//! The operator's users and API tokens ([`crate::accounts`]) as the rows of the tables that
//! migration `0009` adds: `users` and `api_tokens`, one row per user and per token, keyed by
//! name, holding the record as the `jsonb` the accounts file holds for it, without the name.
//!
//! A database that holds no row holds no user and no token. An older release never reads either
//! table (`0009` is `may-run`). A load reads every row through the file's reader, so it is as
//! strict as a start on the files, and refuses accounts that break a rule of
//! [`Accounts::new`], naming the table.

use super::rows::{KeyTy, KeyVal, Rows, Table, Tables, Ty, Val};
use super::state::{json_of, place};
use crate::accounts::{Accounts, AccountsError};

/// The table of the users.
pub(crate) const USERS: Table = Table {
    name: "users",
    keys: &[("name", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The table of the API tokens.
pub(crate) const API_TOKENS: Table = Table {
    name: "api_tokens",
    keys: &[("name", KeyTy::Text)],
    values: &[("record", Ty::Json)],
};

/// The accounts' tables.
pub(crate) const TABLES: [Table; 2] = [USERS, API_TOKENS];

/// The rows of `accounts`, under the tables' names.
pub(crate) fn rows(accounts: &Accounts) -> Tables {
    let record = |value: Result<serde_json::Value, serde_json::Error>| {
        vec![Val::Json(value.unwrap_or_default())]
    };
    let mut users = Rows::new();
    for user in accounts.users() {
        users.insert(
            vec![KeyVal::Text(user.name.clone())],
            record(serde_json::to_value(user)),
        );
    }
    let mut tokens = Rows::new();
    for token in accounts.tokens() {
        tokens.insert(
            vec![KeyVal::Text(token.name.clone())],
            record(serde_json::to_value(token)),
        );
    }
    Tables::from([(USERS.name, users), (API_TOKENS.name, tokens)])
}

/// The accounts `tables` hold; a table they lack reads as empty.
///
/// # Errors
/// [`AccountsError::Invalid`]: a row does not read, or the accounts break a rule of
/// [`Accounts::new`].
pub(crate) fn of_tables(tables: &Tables) -> Result<Accounts, AccountsError> {
    let empty = Rows::new();
    let records = |table: &Table| {
        tables
            .get(table.name)
            .unwrap_or(&empty)
            .iter()
            .map(|(key, values)| {
                let name = key.first().and_then(KeyVal::as_text).unwrap_or_default();
                let record = values.first().and_then(json_of).unwrap_or_default();
                (name.to_owned(), record)
            })
            .collect::<Vec<_>>()
    };
    Accounts::of_records(&place("accounts"), records(&USERS), records(&API_TOKENS))
}
