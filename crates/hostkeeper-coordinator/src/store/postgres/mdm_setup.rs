//! The MDM setup tables of migration `0012` behind [`SetupStore`]: `mdm_secrets` and
//! `mdm_settings` (`docs/design/coordinator-mdm.md`, 4.9).
//!
//! A change is one transaction on the writing session, fenced by the lease as every commit is. A
//! row added whose version is not the next one answers before its first write, so the transaction
//! that commits has written nothing. A read runs on the read-only session and sees committed rows
//! only. The sealed bytes are a `bytea` column, never read into a log line or an error.

use std::collections::BTreeMap;

use postgres::Row;

use super::Postgres;
use crate::store::mdm_setup::{Sealed, SecretRow, SetupError, SetupStore};

const SECRET: &str =
    "kind, version, sealed, fingerprint, not_after_unix, created_unix, superseded_unix";

/// The setup tables of a [`Postgres`] store.
pub(crate) struct PgSetup {
    store: Postgres,
}

impl PgSetup {
    /// The setup tables of `store`.
    pub(crate) fn over(store: &Postgres) -> PgSetup {
        PgSetup {
            store: store.clone(),
        }
    }
}

fn secret_of(row: &Row) -> Result<SecretRow, SetupError> {
    let version: i64 = row.get(1);
    Ok(SecretRow {
        kind: row.get(0),
        version: u32::try_from(version).map_err(|_| {
            SetupError::Store(format!(
                "mdm_secrets holds a version out of range: {version}"
            ))
        })?,
        sealed: Sealed(row.get(2)),
        fingerprint: row.get(3),
        not_after_unix: row.get(4),
        created_unix: row.get(5),
        superseded_unix: row.get(6),
    })
}

impl SetupStore for PgSetup {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        let row = row.clone();
        let kind = row.kind.clone();
        let refused = self
            .store
            .write(move |tx| {
                let newest = tx.query(
                    "SELECT version FROM mdm_secrets WHERE kind = $1 \
                     ORDER BY version DESC LIMIT 1 FOR UPDATE",
                    &[&row.kind],
                )?;
                let expected = newest
                    .first()
                    .map_or(1, |newest| newest.get::<_, i64>(0) + 1);
                let version = i64::from(row.version);
                if version != expected {
                    return Ok(Some(expected));
                }
                tx.execute(
                    "UPDATE mdm_secrets SET superseded_unix = $2 \
                     WHERE kind = $1 AND superseded_unix IS NULL",
                    &[&row.kind, &row.created_unix],
                )?;
                tx.execute(
                    &format!(
                        "INSERT INTO mdm_secrets ({SECRET}) VALUES ($1, $2, $3, $4, $5, $6, NULL)"
                    ),
                    &[
                        &row.kind,
                        &version,
                        &row.sealed.0,
                        &row.fingerprint,
                        &row.not_after_unix,
                        &row.created_unix,
                    ],
                )?;
                Ok(None)
            })
            .map_err(|fail| SetupError::Store(fail.why().to_owned()))?;
        match refused {
            None => Ok(()),
            Some(expected) => Err(SetupError::Version {
                kind,
                expected: u32::try_from(expected).unwrap_or(u32::MAX),
            }),
        }
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        let rows = self
            .store
            .read(|s| {
                s.query(
                    &format!(
                        "SELECT {SECRET} FROM mdm_secrets ORDER BY kind COLLATE \"C\", version"
                    ),
                    &[],
                )
            })
            .map_err(SetupError::Store)?;
        rows.iter().map(secret_of).collect()
    }

    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.store
            .write(move |tx| {
                tx.execute(
                    "DELETE FROM mdm_secrets WHERE superseded_unix IS NOT NULL \
                     AND superseded_unix <= $1",
                    &[&superseded_unix],
                )
            })
            .map_err(|fail| SetupError::Store(fail.why().to_owned()))
    }

    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        let rows = self
            .store
            .read(|s| s.query("SELECT name, value FROM mdm_settings", &[]))
            .map_err(SetupError::Store)?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        let (name, value) = (name.to_owned(), value.to_owned());
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO mdm_settings (name, value) VALUES ($1, $2) \
                     ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value",
                    &[&name, &value],
                )
            })
            .map(|_| ())
            .map_err(|fail| SetupError::Store(fail.why().to_owned()))
    }
}
