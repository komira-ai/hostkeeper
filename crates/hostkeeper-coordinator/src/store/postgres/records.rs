//! The enrollment store's records in the database ([`Records`], section 3.6): one table per kind
//! of record, one column per field of its message, so an export writes the same messages back.
//!
//! Writes go through the writing session, each commit one transaction. Reads that are not
//! decisions go to the read-only session. The heartbeat's reads (a host's row, its deny row, the
//! host ids) are served from an in-memory copy of the hosts and the deny rows, loaded at start and
//! changed only after a commit returns Ok, so a heartbeat in the steady state runs no query. After
//! a commit of unknown outcome the copy is loaded again from the tables before it answers again.
//!
//! Under Postgres the store's lock is its in-process mutex alone ([`RecordsLock::in_process`]):
//! the advisory lock keeps other processes off.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use hostkeeper_enrollment::enroll::records::{
    Record, RecordKey, Records, RecordsLock, Snapshot, pb,
};
use hostkeeper_enrollment::enroll::{FIRST_SERIAL, MacNonceHash, StoreError};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use postgres::Row;
use prost::Message as _;

use super::rows::{Stmt, Val};
use super::session::Session;
use super::writer::WriteFail;
use super::{Postgres, run_all};

/// The columns of `enrollment_requests`, in the order its rows are read and written.
const REQUEST_COLUMNS: &str = "request_id, state, expired_unix, expired_after_secs, posted_unix, \
    round, serial, renewal_of, image, csr_der, spki_der, mac_nonce_presented, machine_id, \
    dmi_serial, dmi_product_uuid, addresses, mac_serial, mac_platform_uuid, bmc_mac, \
    approved_host_id, approved_role, approved_services, approved_replace, approver, \
    approved_unix, approved_reason, approved_mac_nonce";

/// The columns of `enrollment_hosts`.
const HOST_COLUMNS: &str = "host_id, key_hash, machine_id, role, services, duplicate, first_heartbeat_unix, \
    first_serving_unix";

/// What a read of a host takes: the columns of `enrollment_hosts` and the running record
/// (`enrollment_host_running`, migration `0006`) of the same host, which a host without one has
/// `NULL` for ([`host_of`] reads the columns of [`HOST_READ`]).
const HOST_FROM: &str = "enrollment_hosts LEFT JOIN enrollment_host_running USING (host_id)";

/// The columns [`HOST_FROM`] gives, in the order [`host_of`] reads them.
const HOST_READ: &str = "host_id, key_hash, machine_id, role, services, duplicate, \
    first_heartbeat_unix, first_serving_unix, running";

/// The columns of `enrollment_issued`.
const ISSUED_COLUMNS: &str = "request_id, round, serial, host_id, not_after, certificate_der";

/// The columns of `enrollment_deny`.
const DENY_COLUMNS: &str = "host_id, state, cleared_by";

/// The columns of `mac_nonces`.
const NONCE_COLUMNS: &str = "nonce_sha256, serial, issued_unix, consumed_by, request_id";

/// The value every record kept here carries in its `schema`, which only a file needs.
const SCHEMA: u32 = 1;

/// The heartbeat's copy of the hosts and the deny rows.
#[derive(Debug, Default)]
struct Copy {
    hosts: BTreeMap<String, pb::Host>,
    deny: BTreeMap<String, pb::Deny>,
    /// Whether a commit of unknown outcome may have changed the tables since the copy was made.
    stale: bool,
}

/// The enrollment store's records in the database.
pub(crate) struct PgRecords {
    store: Postgres,
    copy: Mutex<Copy>,
}

impl fmt::Debug for PgRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PgRecords")
    }
}

/// The error of a record that could not be read or written, at `place`.
fn failed(place: PathBuf, why: &str) -> StoreError {
    StoreError::Io {
        path: place,
        source: io::Error::other(why.to_owned()),
    }
}

fn at(table: &str) -> PathBuf {
    PathBuf::from(format!("database:{table}"))
}

/// A counter of a message as a `bigint`.
fn int(n: u64, table: &str) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Corrupt {
        path: at(table),
        reason: format!("{n} is above the largest counter the database keeps"),
    })
}

/// A count of the database as the message's counter.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or_default()
}

/// Empty bytes as `NULL`, the way a column of an optional key keeps "none".
fn bytes_or_null(bytes: &[u8]) -> Val {
    if bytes.is_empty() {
        Val::Null
    } else {
        Val::Bytes(bytes.to_vec())
    }
}

fn state_of(state: i32, table: &str) -> Result<i16, StoreError> {
    i16::try_from(state).map_err(|_| StoreError::UnknownState {
        path: at(table),
        value: state,
    })
}

/// The statement that writes `record` over the record of its key.
fn upsert(record: &Record) -> Result<Stmt, StoreError> {
    let text = |s: &str| Val::Text(s.to_owned());
    let (sql, params) = match record {
        Record::Request(r) => {
            let claims = r.claims.clone().unwrap_or_default();
            let approval = r.approved.as_ref();
            let approved = |f: &dyn Fn(&pb::Approval) -> Val| approval.map_or(Val::Null, f);
            (
                format!(
                    "INSERT INTO enrollment_requests ({REQUEST_COLUMNS}) VALUES ($1, $2, $3, $4, \
                     $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, \
                     $21, $22, $23, $24, $25, $26, $27) ON CONFLICT (request_id) DO UPDATE SET \
                     ({REQUEST_COLUMNS}) = ROW(EXCLUDED.*)"
                ),
                vec![
                    Val::Bytes(r.request_id.clone()),
                    Val::Small(state_of(r.state, "enrollment_requests")?),
                    Val::Int(int(r.expired_unix, "enrollment_requests")?),
                    Val::Int(int(r.expired_after_secs, "enrollment_requests")?),
                    Val::Int(int(r.posted_unix, "enrollment_requests")?),
                    Val::Int(i64::from(r.round)),
                    Val::Int(int(r.serial, "enrollment_requests")?),
                    text(&r.renewal_of),
                    text(&r.image),
                    Val::Bytes(r.csr_der.clone()),
                    Val::Bytes(r.spki_der.clone()),
                    Val::Bool(r.mac_nonce_presented),
                    text(&claims.machine_id),
                    text(&claims.dmi_serial),
                    text(&claims.dmi_product_uuid),
                    Val::Texts(claims.addresses.clone()),
                    text(&claims.mac_serial),
                    text(&claims.mac_platform_uuid),
                    text(&claims.bmc_mac),
                    approved(&|a| text(&a.host_id)),
                    approved(&|a| text(&a.role)),
                    approved(&|a| Val::Texts(a.services.clone())),
                    approved(&|a| Val::Bool(a.replace)),
                    approved(&|a| text(&a.approver)),
                    match approval {
                        Some(a) => Val::Int(int(a.approved_unix, "enrollment_requests")?),
                        None => Val::Null,
                    },
                    approved(&|a| text(&a.reason)),
                    approved(&|a| Val::Bytes(a.mac_nonce_sha256.clone())),
                ],
            )
        }
        Record::Issued(i) => (
            "INSERT INTO enrollment_issued (request_id, round, serial, host_id, not_after, \
             certificate_der) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (request_id) DO UPDATE \
             SET (round, serial, host_id, not_after, certificate_der) = (EXCLUDED.round, \
             EXCLUDED.serial, EXCLUDED.host_id, EXCLUDED.not_after, EXCLUDED.certificate_der)"
                .to_owned(),
            vec![
                Val::Bytes(i.request_id.clone()),
                Val::Int(i64::from(i.round)),
                Val::Int(int(i.serial, "enrollment_issued")?),
                text(&i.host_id),
                Val::Int(int(i.not_after, "enrollment_issued")?),
                Val::Bytes(i.certificate_der.clone()),
            ],
        ),
        Record::Host(h) => (
            format!(
                "INSERT INTO enrollment_hosts ({HOST_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, \
                 $8) ON CONFLICT (host_id) DO UPDATE SET ({HOST_COLUMNS}) = ROW(EXCLUDED.*)"
            ),
            vec![
                text(&h.host_id),
                Val::Bytes(h.key_hash.clone()),
                text(&h.machine_id),
                text(&h.role),
                Val::Texts(h.services.clone()),
                Val::Bool(h.duplicate),
                match h.first_heartbeat_unix {
                    Some(at) => Val::Int(int(at, "enrollment_hosts")?),
                    None => Val::Null,
                },
                match h.first_serving_unix {
                    Some(at) => Val::Int(int(at, "enrollment_hosts")?),
                    None => Val::Null,
                },
            ],
        ),
        Record::Deny(d) => (
            "INSERT INTO enrollment_deny (host_id, state, cleared_by) VALUES ($1, $2, $3) \
             ON CONFLICT (host_id) DO UPDATE SET (state, cleared_by) = \
             (EXCLUDED.state, EXCLUDED.cleared_by)"
                .to_owned(),
            vec![
                text(&d.host_id),
                Val::Small(state_of(d.state, "enrollment_deny")?),
                bytes_or_null(&d.cleared_by),
            ],
        ),
        Record::MacNonce(n) => (
            "INSERT INTO mac_nonces (nonce_sha256, serial, issued_unix, consumed_by, request_id) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (nonce_sha256) DO UPDATE SET \
             (serial, issued_unix, consumed_by, request_id) = (EXCLUDED.serial, \
             EXCLUDED.issued_unix, EXCLUDED.consumed_by, EXCLUDED.request_id)"
                .to_owned(),
            vec![
                Val::Bytes(n.nonce_sha256.clone()),
                text(&n.serial),
                Val::Int(int(n.issued_unix, "mac_nonces")?),
                bytes_or_null(&n.consumed_by),
                bytes_or_null(&n.request_id),
            ],
        ),
    };
    Ok(Stmt { sql, params })
}

/// The request of a row of `enrollment_requests`, its columns in [`REQUEST_COLUMNS`]' order.
fn request_of(row: &Row) -> pb::Request {
    let approved = row
        .get::<_, Option<String>>(19)
        .map(|host_id| pb::Approval {
            host_id,
            role: row.get::<_, Option<String>>(20).unwrap_or_default(),
            services: row.get::<_, Option<Vec<String>>>(21).unwrap_or_default(),
            replace: row.get::<_, Option<bool>>(22).unwrap_or_default(),
            approver: row.get::<_, Option<String>>(23).unwrap_or_default(),
            approved_unix: unsigned(row.get::<_, Option<i64>>(24).unwrap_or_default()),
            reason: row.get::<_, Option<String>>(25).unwrap_or_default(),
            mac_nonce_sha256: row.get::<_, Option<Vec<u8>>>(26).unwrap_or_default(),
        });
    pb::Request {
        schema: SCHEMA,
        request_id: row.get(0),
        state: i32::from(row.get::<_, i16>(1)),
        expired_unix: unsigned(row.get(2)),
        expired_after_secs: unsigned(row.get(3)),
        posted_unix: unsigned(row.get(4)),
        round: u32::try_from(row.get::<_, i64>(5)).unwrap_or_default(),
        serial: unsigned(row.get(6)),
        renewal_of: row.get(7),
        image: row.get(8),
        csr_der: row.get(9),
        spki_der: row.get(10),
        mac_nonce_presented: row.get(11),
        claims: Some(pb::Claims {
            machine_id: row.get(12),
            dmi_serial: row.get(13),
            dmi_product_uuid: row.get(14),
            addresses: row.get(15),
            mac_serial: row.get(16),
            mac_platform_uuid: row.get(17),
            bmc_mac: row.get(18),
        }),
        approved,
    }
}

/// The host of a row of [`HOST_FROM`], its columns in [`HOST_READ`]' order.
///
/// # Errors
/// [`StoreError::Corrupt`] when the running record does not decode: a host is not read with
/// its running record dropped, which the next heartbeat would write over without a trace.
fn host_of(row: &Row) -> Result<pb::Host, StoreError> {
    let host_id: String = row.get(0);
    let running = match row.get::<_, Option<Vec<u8>>>(8) {
        Some(bytes) => {
            Some(
                pb::Running::decode(bytes.as_slice()).map_err(|e| StoreError::Corrupt {
                    path: at(&format!("enrollment_host_running/{host_id}")),
                    reason: format!("the running record does not decode: {e}"),
                })?,
            )
        }
        None => None,
    };
    Ok(pb::Host {
        schema: SCHEMA,
        host_id,
        key_hash: row.get(1),
        machine_id: row.get(2),
        role: row.get(3),
        services: row.get(4),
        duplicate: row.get(5),
        first_heartbeat_unix: row.get::<_, Option<i64>>(6).map(unsigned),
        first_serving_unix: row.get::<_, Option<i64>>(7).map(unsigned),
        running,
    })
}

/// The statement that writes the running record of `host` over the one it has, or removes it
/// when the host has none, so a host written without one (an approval) holds none.
fn running_stmt(host: &pb::Host) -> Stmt {
    match &host.running {
        Some(running) => Stmt {
            sql: "INSERT INTO enrollment_host_running (host_id, running) VALUES ($1, $2) \
                  ON CONFLICT (host_id) DO UPDATE SET running = EXCLUDED.running"
                .to_owned(),
            params: vec![
                Val::Text(host.host_id.clone()),
                Val::Bytes(running.encode_to_vec()),
            ],
        },
        None => Stmt {
            sql: "DELETE FROM enrollment_host_running WHERE host_id = $1".to_owned(),
            params: vec![Val::Text(host.host_id.clone())],
        },
    }
}

/// The statements that write `record`: its row, and for a host its running record.
fn statements(record: &Record) -> Result<Vec<Stmt>, StoreError> {
    let mut out = vec![upsert(record)?];
    if let Record::Host(host) = record {
        out.push(running_stmt(host));
    }
    Ok(out)
}

fn deny_of(row: &Row) -> pb::Deny {
    pb::Deny {
        schema: SCHEMA,
        host_id: row.get(0),
        state: i32::from(row.get::<_, i16>(1)),
        cleared_by: row.get::<_, Option<Vec<u8>>>(2).unwrap_or_default(),
    }
}

/// The issued certificate of a row of `enrollment_issued`, its columns in [`ISSUED_COLUMNS`]'
/// order.
fn issued_of(row: &Row) -> pb::Issued {
    pb::Issued {
        schema: SCHEMA,
        request_id: row.get(0),
        round: u32::try_from(row.get::<_, i64>(1)).unwrap_or_default(),
        serial: unsigned(row.get(2)),
        host_id: row.get(3),
        not_after: unsigned(row.get(4)),
        certificate_der: row.get(5),
    }
}

/// The Mac nonce of a row of `mac_nonces`, its columns in [`NONCE_COLUMNS`]' order.
fn nonce_of(row: &Row) -> pb::MacNonce {
    pb::MacNonce {
        schema: SCHEMA,
        nonce_sha256: row.get(0),
        serial: row.get(1),
        issued_unix: unsigned(row.get(2)),
        consumed_by: row.get::<_, Option<Vec<u8>>>(3).unwrap_or_default(),
        request_id: row.get::<_, Option<Vec<u8>>>(4).unwrap_or_default(),
    }
}

/// Every record the tables of `session` hold, each list in the order of its key, and the next
/// serial when one was taken: what an import reads back and an export writes
/// (`docs/design/coordinator-storage.md`, section 11). A counter still at the first serial is
/// one no serial was taken from.
///
/// # Errors
/// The client's error.
///
/// The outer error is the client's; the inner one a running record that does not decode.
pub(super) fn read_all(
    session: &mut Session,
) -> Result<Result<Snapshot, StoreError>, postgres::Error> {
    let mut all = |columns: &str, table: &str, key: &str| {
        session.query(
            &format!("SELECT {columns} FROM {table} ORDER BY {key}"),
            &[],
        )
    };
    let requests = all(REQUEST_COLUMNS, "enrollment_requests", "request_id")?;
    let issued = all(ISSUED_COLUMNS, "enrollment_issued", "request_id")?;
    // A host id's order is its bytes', as the files list them, whatever the database's collation.
    let hosts = all(HOST_READ, HOST_FROM, "host_id COLLATE \"C\"")?;
    let deny = all(DENY_COLUMNS, "enrollment_deny", "host_id COLLATE \"C\"")?;
    let nonces = all(NONCE_COLUMNS, "mac_nonces", "nonce_sha256")?;
    let next: i64 = session
        .query_one("SELECT next FROM enrollment_serial", &[])?
        .get(0);
    let hosts: Result<Vec<pb::Host>, StoreError> = hosts.iter().map(host_of).collect();
    Ok(hosts.map(|hosts| Snapshot {
        requests: requests.iter().map(request_of).collect(),
        issued: issued.iter().map(issued_of).collect(),
        hosts,
        deny: deny.iter().map(deny_of).collect(),
        mac_nonces: nonces.iter().map(nonce_of).collect(),
        serial: Some(unsigned(next)).filter(|next| *next != FIRST_SERIAL),
    }))
}

/// The statements that write every record of `snapshot` into tables that hold none, and set the
/// serial counter to its next serial when one was taken.
///
/// # Errors
/// [`StoreError`]: a record's key does not parse, or a counter is above the largest the database
/// keeps.
pub(super) fn writes(snapshot: &Snapshot) -> Result<Vec<Stmt>, StoreError> {
    let records = snapshot
        .requests
        .iter()
        .cloned()
        .map(Record::Request)
        .chain(snapshot.issued.iter().cloned().map(Record::Issued))
        .chain(snapshot.hosts.iter().cloned().map(Record::Host))
        .chain(snapshot.deny.iter().cloned().map(Record::Deny))
        .chain(snapshot.mac_nonces.iter().cloned().map(Record::MacNonce));
    let mut out = Vec::new();
    for record in records {
        record.key()?;
        out.extend(statements(&record)?);
    }
    if let Some(next) = snapshot.serial {
        out.push(Stmt {
            sql: "UPDATE enrollment_serial SET next = $1".to_owned(),
            params: vec![Val::Int(int(next, "enrollment_serial")?)],
        });
    }
    Ok(out)
}

/// The copy of the hosts and the deny rows, read from the tables.
fn copy_of(store: &Postgres) -> Result<Copy, StoreError> {
    store
        .read(|session| {
            let hosts: Result<BTreeMap<String, pb::Host>, StoreError> = session
                .query(&format!("SELECT {HOST_READ} FROM {HOST_FROM}"), &[])?
                .iter()
                .map(|row| host_of(row).map(|host| (host.host_id.clone(), host)))
                .collect();
            let deny = session
                .query(&format!("SELECT {DENY_COLUMNS} FROM enrollment_deny"), &[])?
                .iter()
                .map(|row| {
                    let deny = deny_of(row);
                    (deny.host_id.clone(), deny)
                })
                .collect();
            Ok(hosts.map(|hosts| Copy {
                hosts,
                deny,
                stale: false,
            }))
        })
        .map_err(|why| failed(at("enrollment_hosts"), &why))?
}

impl PgRecords {
    /// The records of `store`, the heartbeat's copy loaded from its tables.
    ///
    /// # Errors
    /// [`StoreError`]: the copy cannot be read.
    pub(crate) fn over(store: &Postgres) -> Result<PgRecords, StoreError> {
        Ok(PgRecords {
            copy: Mutex::new(copy_of(store)?),
            store: store.clone(),
        })
    }

    /// The copy, loaded again first when a commit of unknown outcome may have changed the tables
    /// and that outcome is settled. While the writing session is lost or the outcome is not yet
    /// known, the copy is served as it is and stays marked: a heartbeat never waits for a
    /// reconnect, and never takes rows read before the commit it waits on was settled.
    fn copy(&self) -> Result<MutexGuard<'_, Copy>, StoreError> {
        let mut copy = self.copy.lock().unwrap_or_else(PoisonError::into_inner);
        if copy.stale && self.store.settled_up() {
            *copy = copy_of(&self.store)?;
        }
        Ok(copy)
    }

    /// Runs the statements of `records`, and `then` in the same transaction; on Ok the copy takes
    /// the hosts and the deny rows among them.
    fn write<R: Send + 'static>(
        &self,
        records: &[Record],
        then: impl FnOnce(&mut super::writer::Tx<'_>) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, StoreError> {
        let mut statements = Vec::with_capacity(records.len());
        for record in records {
            record.key()?;
            statements.extend(self::statements(record)?);
        }
        let place = records
            .first()
            .and_then(|record| record.key().ok())
            .map_or_else(|| self.place(&RecordKey::Serial), |key| self.place(&key));
        let answer = self.store.write(move |tx| {
            run_all(tx, &statements)?;
            then(tx)
        });
        let mut copy = self.copy.lock().unwrap_or_else(PoisonError::into_inner);
        match answer {
            Ok(answer) => {
                for record in records {
                    match record {
                        Record::Host(host) => {
                            copy.hosts.insert(host.host_id.clone(), host.clone());
                        }
                        Record::Deny(deny) => {
                            copy.deny.insert(deny.host_id.clone(), deny.clone());
                        }
                        _ => {}
                    }
                }
                Ok(answer)
            }
            Err(fail) => {
                if matches!(fail, WriteFail::Unknown { .. }) {
                    copy.stale = true;
                }
                Err(failed(place, fail.why()))
            }
        }
    }

    /// Reads one row of `sql` by its key `key`, made by `of`.
    fn one<T: Send + 'static>(
        &self,
        place: PathBuf,
        sql: String,
        key: Vec<u8>,
        of: fn(&Row) -> T,
    ) -> Result<Option<T>, StoreError> {
        self.store
            .read(move |session| Ok(session.query_opt(&sql, &[&key])?.as_ref().map(of)))
            .map_err(|why| failed(place, &why))
    }
}

impl Records for PgRecords {
    fn lock(&self) -> Result<RecordsLock, StoreError> {
        Ok(RecordsLock::in_process())
    }

    fn place(&self, key: &RecordKey) -> PathBuf {
        match key {
            RecordKey::Request(id) => at(&format!("enrollment_requests/{id}")),
            RecordKey::Issued(id) => at(&format!("enrollment_issued/{id}")),
            RecordKey::Host(host) => at(&format!("enrollment_hosts/{host}")),
            RecordKey::Deny(host) => at(&format!("enrollment_deny/{host}")),
            RecordKey::MacNonce(hash) => at(&format!(
                "mac_nonces/{}",
                hash.as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )),
            RecordKey::Serial => at("enrollment_serial"),
        }
    }

    fn request(&self, id: &KeyHash) -> Result<Option<pb::Request>, StoreError> {
        self.one(
            self.place(&RecordKey::Request(*id)),
            format!("SELECT {REQUEST_COLUMNS} FROM enrollment_requests WHERE request_id = $1"),
            id.as_bytes().to_vec(),
            request_of,
        )
    }

    fn requests(&self) -> Result<Vec<(KeyHash, pb::Request)>, StoreError> {
        let rows = self
            .store
            .read(|session| {
                Ok(session
                    .query(
                        &format!(
                            "SELECT {REQUEST_COLUMNS} FROM enrollment_requests ORDER BY request_id"
                        ),
                        &[],
                    )?
                    .iter()
                    .map(request_of)
                    .collect::<Vec<_>>())
            })
            .map_err(|why| failed(at("enrollment_requests"), &why))?;
        rows.into_iter()
            .map(|request| {
                KeyHash::from_bytes(&request.request_id)
                    .map(|id| (id, request))
                    .ok_or(StoreError::Unkeyed("request"))
            })
            .collect()
    }

    fn issued(&self, id: &KeyHash) -> Result<Option<pb::Issued>, StoreError> {
        self.one(
            self.place(&RecordKey::Issued(*id)),
            format!("SELECT {ISSUED_COLUMNS} FROM enrollment_issued WHERE request_id = $1"),
            id.as_bytes().to_vec(),
            issued_of,
        )
    }

    fn host(&self, host: &HostId) -> Result<Option<pb::Host>, StoreError> {
        Ok(self.copy()?.hosts.get(host.as_str()).cloned())
    }

    fn host_ids(&self) -> Result<Vec<HostId>, StoreError> {
        self.copy()?
            .hosts
            .keys()
            .map(|id| HostId::parse(id).map_err(|_| StoreError::Unkeyed("host")))
            .collect()
    }

    fn deny(&self, host: &HostId) -> Result<Option<pb::Deny>, StoreError> {
        Ok(self.copy()?.deny.get(host.as_str()).cloned())
    }

    fn mac_nonce(&self, hash: &MacNonceHash) -> Result<Option<pb::MacNonce>, StoreError> {
        self.one(
            self.place(&RecordKey::MacNonce(*hash)),
            format!("SELECT {NONCE_COLUMNS} FROM mac_nonces WHERE nonce_sha256 = $1"),
            hash.as_bytes().to_vec(),
            nonce_of,
        )
    }

    fn commit(&self, records: &[Record]) -> Result<(), StoreError> {
        self.write(records, |_| Ok(()))
    }

    fn take_serial(&self, reserving: Option<&pb::Request>) -> Result<u64, StoreError> {
        // The request is written whole, then given the serial taken, in the same transaction.
        let reserved = match reserving {
            Some(request) => {
                let record = Record::Request(request.clone());
                record.key()?;
                Some((upsert(&record)?, request.request_id.clone()))
            }
            None => None,
        };
        let next = self.write(&[], move |tx| {
            let taken: i64 = tx
                .query_one(
                    "UPDATE enrollment_serial SET next = next + 1 RETURNING next - 1",
                    &[],
                )?
                .get(0);
            if let Some((statement, id)) = reserved {
                run_all(tx, &[statement])?;
                tx.execute(
                    "UPDATE enrollment_requests SET serial = $1 WHERE request_id = $2",
                    &[&taken, &id],
                )?;
            }
            Ok(taken)
        })?;
        Ok(unsigned(next))
    }

    fn set_first_heartbeat(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.commit(&[Record::Host(pb::Host {
            first_heartbeat_unix: Some(at_unix),
            ..host.clone()
        })])
    }

    fn set_first_serving(&self, host: &pb::Host, at_unix: u64) -> Result<(), StoreError> {
        self.commit(&[Record::Host(pb::Host {
            first_serving_unix: Some(at_unix),
            ..host.clone()
        })])
    }

    fn set_running(&self, host: &pb::Host, running: pb::Running) -> Result<(), StoreError> {
        self.commit(&[Record::Host(pb::Host {
            running: Some(running),
            ..host.clone()
        })])
    }

    fn set_duplicate(&self, host: &pb::Host) -> Result<(), StoreError> {
        self.commit(&[Record::Host(pb::Host {
            duplicate: true,
            ..host.clone()
        })])
    }
}
