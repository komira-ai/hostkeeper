//! The MDM gate's device records in the database (migration `0005`, the table `mdm_devices`):
//! the gate's [`DeviceStore`] beside its files, one row per enrollment id.
//!
//! A row keeps the record as the gate keeps it in a file, the bytes of
//! [`hostkeeper_mdm_gate::encode_record`], and a load reads each with the gate's own reader
//! ([`decode_record`]), so a row is as strict as a file: a record that is not whole, is of a newer
//! schema or is of another enrollment id than its key stops the gate's start, naming the row; a
//! field of a later release is skipped. A record moves from a row to a file byte for byte
//! (`export`).
//!
//! A save is one transaction on the writing session, fenced by the lease as every commit is: it
//! commits only while this coordinator holds the database's lock. A save that does not commit,
//! or whose commit's outcome is not known because the session was lost, is an error, and the
//! gate does not take the record; one of unknown outcome may have landed, which the gate reads at
//! its next start as it reads a file renamed into place whose directory sync failed. A load runs
//! on the read-only session and sees committed rows only.

use std::io;
use std::path::PathBuf;

use hostkeeper_mdm_gate::{DeviceRecord, DeviceStore, StoreError, decode_record, encode_record};

use super::Postgres;
use super::session::Session;

/// The statement that writes a record over the one of its enrollment id.
pub(super) const UPSERT: &str = "INSERT INTO mdm_devices (enrollment_id, record) VALUES ($1, $2) \
    ON CONFLICT (enrollment_id) DO UPDATE SET record = EXCLUDED.record";

/// The statement that reads every row, in the order of the enrollment ids' bytes.
const SELECT: &str =
    "SELECT enrollment_id, record FROM mdm_devices ORDER BY enrollment_id COLLATE \"C\"";

/// The gate's device records in the database.
pub(crate) struct PgDevices {
    store: Postgres,
}

impl std::fmt::Debug for PgDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PgDevices")
    }
}

/// Where the row of `key` is, as an error names it.
fn place(key: &str) -> PathBuf {
    PathBuf::from(format!("database:mdm_devices/{key}"))
}

/// The error of the table, or of the row of `key`, that could not be read or written.
fn failed(key: Option<&str>, why: &str) -> StoreError {
    StoreError::Io {
        path: key.map_or_else(|| PathBuf::from("database:mdm_devices"), place),
        source: io::Error::other(why.to_owned()),
    }
}

/// Every row `session` reads, each its enrollment id and its record's bytes.
fn rows(session: &mut Session) -> Result<Vec<(String, Vec<u8>)>, postgres::Error> {
    Ok(session
        .query(SELECT, &[])?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// The records of `rows`, each read by the gate's reader.
fn records_of(rows: &[(String, Vec<u8>)]) -> Result<Vec<DeviceRecord>, StoreError> {
    rows.iter()
        .map(|(key, bytes)| decode_record(&place(key), key, bytes))
        .collect()
}

/// Every record `session` holds, in the order of their enrollment ids: what an export writes.
///
/// # Errors
/// [`StoreError`]: the table cannot be read, or a row is not a record this release reads.
pub(super) fn read_all(session: &mut Session) -> Result<Vec<DeviceRecord>, StoreError> {
    let rows = rows(session).map_err(|e| failed(None, &super::session::describe(&e)))?;
    records_of(&rows)
}

impl PgDevices {
    /// The device records of `store`.
    pub(crate) fn over(store: &Postgres) -> PgDevices {
        PgDevices {
            store: store.clone(),
        }
    }
}

impl DeviceStore for PgDevices {
    fn load(&self) -> Result<Vec<DeviceRecord>, StoreError> {
        let rows = self.store.read(rows).map_err(|why| failed(None, &why))?;
        records_of(&rows)
    }

    fn save(&self, record: &DeviceRecord) -> Result<(), StoreError> {
        let key = record.enrollment_id.as_str().to_owned();
        let bytes = encode_record(record);
        let params = (key.clone(), bytes);
        self.store
            .write(move |tx| {
                let (key, bytes) = &params;
                tx.execute(UPSERT, &[key, bytes]).map(|_| ())
            })
            .map_err(|fail| failed(Some(&key), fail.why()))
    }
}
