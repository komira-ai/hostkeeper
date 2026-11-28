//! The hardware event log as rows of `hardware_events` (migration `0033`;
//! `docs/design/host-health-alerts.md`, section 2.3): one row per record, keyed by the host and
//! the record's `event_id`. A write inserts each record on that key in one transaction, and a row
//! the table holds already is left as it is (`ON CONFLICT DO NOTHING`), so a record two
//! coordinators receive is one row. A page is read on the read-only session, newest first, the
//! keys compared byte by byte (`COLLATE "C"`), as [`crate::hardware_log::Query::page_of`] orders
//! them.

use postgres::Row;
use postgres::types::ToSql;

use super::Postgres;
use super::rows::Val;
use crate::hardware_log::{HardwareLog, Query, Record};

/// The columns of a row, in the order a page reads them.
const COLUMNS: &str = "host_id, event_id, machine_serial, time_unix, observed_unix, boot_id, \
                       of_previous_boot, component, subject, severity, state, source, code, \
                       message, raw, about, by_name";

/// The hardware event log of a coordinator over a database.
#[derive(Clone)]
pub(crate) struct PgHardwareLog {
    store: Postgres,
}

impl std::fmt::Debug for PgHardwareLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgHardwareLog").finish_non_exhaustive()
    }
}

impl PgHardwareLog {
    /// The hardware event log of `store`.
    pub(crate) fn new(store: Postgres) -> PgHardwareLog {
        PgHardwareLog { store }
    }
}

/// `n` as a `bigint`.
fn int(n: u64) -> Result<i64, String> {
    i64::try_from(n).map_err(|_| format!("{n} is above the largest time the database keeps"))
}

/// The `bigint` `n` as seconds: 0 for one below (the table refuses those).
fn secs(n: i64) -> u64 {
    u64::try_from(n).unwrap_or_default()
}

/// The record of a row read in [`COLUMNS`]' order.
fn record_of(row: &Row) -> Record {
    Record {
        host_id: row.get(0),
        event_id: row.get(1),
        machine_serial: row.get(2),
        time_unix: secs(row.get(3)),
        observed_unix: secs(row.get(4)),
        boot_id: row.get(5),
        of_previous_boot: row.get(6),
        component: row.get(7),
        subject: row.get(8),
        severity: row.get(9),
        state: row.get(10),
        source: row.get(11),
        code: row.get(12),
        message: row.get(13),
        raw: row.get(14),
        about: row.get(15),
        by: row.get(16),
    }
}

impl HardwareLog for PgHardwareLog {
    fn write(&self, records: &[Record]) -> Result<(), String> {
        let rows = records
            .iter()
            .map(|r| Ok((r.clone(), int(r.time_unix)?, int(r.observed_unix)?)))
            .collect::<Result<Vec<_>, String>>()?;
        self.store
            .write(move |tx| {
                for (r, time, observed) in &rows {
                    tx.execute(
                        &format!(
                            "INSERT INTO hardware_events ({COLUMNS}) VALUES \
                             ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                              $16, $17) ON CONFLICT (host_id, event_id) DO NOTHING"
                        ),
                        &[
                            &r.host_id,
                            &r.event_id,
                            &r.machine_serial,
                            time,
                            observed,
                            &r.boot_id,
                            &r.of_previous_boot,
                            &r.component,
                            &r.subject,
                            &r.severity,
                            &r.state,
                            &r.source,
                            &r.code,
                            &r.message,
                            &r.raw,
                            &r.about,
                            &r.by,
                        ],
                    )?;
                }
                Ok(())
            })
            .map_err(|fail| fail.why().to_owned())
    }

    fn page(&self, query: &Query) -> Result<Vec<Record>, String> {
        let (after_time, after_host, after_event) = match &query.after {
            Some(place) => (
                Val::Int(int(place.time_unix)?),
                Val::Text(place.host_id.clone()),
                Val::Text(place.event_id.clone()),
            ),
            None => (Val::Null, Val::Null, Val::Null),
        };
        let params: Vec<Val> = vec![
            Val::text_or_null(query.host_id.as_deref()),
            Val::text_or_null(query.serial.as_deref()),
            Val::Int(int(query.since_unix)?),
            Val::text_or_null(query.component.as_deref()),
            Val::Texts(query.severities.clone()),
            after_time,
            after_host,
            after_event,
            Val::Int(i64::try_from(query.limit).unwrap_or(i64::MAX)),
        ];
        let sql = format!(
            "SELECT {COLUMNS} FROM hardware_events \
             WHERE ($1::text IS NULL OR host_id = $1::text) \
             AND ($2::text IS NULL OR machine_serial = $2::text) \
             AND time_unix >= $3::bigint \
             AND ($4::text IS NULL OR component = $4::text) \
             AND (cardinality($5::text[]) = 0 OR severity = ANY($5::text[])) \
             AND ($6::bigint IS NULL OR (time_unix, host_id COLLATE \"C\", event_id COLLATE \"C\") \
                  < ($6::bigint, $7::text COLLATE \"C\", $8::text COLLATE \"C\")) \
             ORDER BY time_unix DESC, host_id COLLATE \"C\" DESC, event_id COLLATE \"C\" DESC \
             LIMIT $9::bigint"
        );
        self.store.read(move |session| {
            let refs: Vec<&(dyn ToSql + Sync)> =
                params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
            Ok(session.query(&sql, &refs)?.iter().map(record_of).collect())
        })
    }
}
