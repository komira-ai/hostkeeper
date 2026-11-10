//! The database's side of `import` and `export` (`docs/design/coordinator-storage.md`, section
//! 11): the whole state written into a database that holds none, in one transaction that reads
//! it back and compares it before it commits; and the whole state read out of a database under
//! the coordinator's lock, its schema first brought up to this build as a start brings it.
//!
//! Both run on one session of their own, on the caller's thread (neither runs in a runtime), and
//! take the coordinator's advisory lock without waiting, so a coordinator serving on the database
//! refuses them. Neither raises the epoch: neither is a coordinator that commits as one.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use hostkeeper_enrollment::enroll::FIRST_SERIAL;
use hostkeeper_enrollment::enroll::records::Snapshot;
use hostkeeper_mdm_gate::DeviceRecord;

use super::rows::{Stmt, Tables, Val};
use super::session::{Session, describe};
use super::writer::try_lock;
use super::{DatabaseError, Dsn, Timeouts, devices, load_all, logged_of, migrate, records};
use super::{accounts, state, statements};
use crate::accounts::Accounts;
use crate::audit_file::AuditLine;
use crate::desired;
use crate::ops::OpsState;
use crate::store::{Commit, Loaded};
use crate::verdict_file;

/// The bounded waits of an import or an export: an operator's one-off run, not a heartbeat under
/// the fleet lock, so a statement may run as long as a whole audit history takes to read.
const MOVE_TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(5),
    statement: Duration::from_secs(300),
    lock: Duration::from_secs(3),
    tcp: Duration::from_secs(5),
};

/// Every table that holds state, in the order an export counts them; `imports` is the import's
/// own record, and the bookkeeping (`schema_version`, `lease`, `enrollment_serial`) is not here.
pub(crate) const STATE_TABLES: [&str; 39] = [
    "desired",
    "desired_digests",
    "releases",
    "repositories",
    "repository_generation",
    "host_groups",
    "host_group_packages",
    "group_desired",
    "services",
    "bad_versions",
    "failure_acks",
    "host_labels",
    "exclusions",
    "holds",
    "assignments",
    "min_versions",
    "uninstall_intents",
    "inventory",
    "halts",
    "halt_reports",
    "rollouts",
    "rollout_done",
    "group_min_versions",
    "group_services",
    "group_rollouts",
    "group_rollout_done",
    "audit",
    "spent_tokens",
    "verdict_standing",
    "verdict_counted",
    "enrollment_requests",
    "enrollment_issued",
    "enrollment_hosts",
    "enrollment_host_running",
    "enrollment_deny",
    "mac_nonces",
    "mdm_devices",
    "users",
    "api_tokens",
];

/// The rows of each state table, in [`STATE_TABLES`]' order.
pub(crate) type Counts = Vec<(&'static str, i64)>;

/// The action of the audit row an import appends, by `coordinator`.
pub(crate) const ACTION_IMPORTED: &str = "state_imported";

/// Everything the coordinator keeps durable, whole: what an import writes and an export reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Moved {
    /// The desired state, the operations state, the verdicts and the accounts, with the spent ids not yet
    /// past; the ring of the operations state is not part of it, which the audit history holds.
    pub(crate) loaded: Loaded,
    /// The whole audit history, oldest first.
    pub(crate) audit: Vec<AuditLine>,
    /// The enrollment store's records.
    pub(crate) records: Snapshot,
    /// The MDM gate's device records, in the order of their enrollment ids.
    pub(crate) devices: Vec<DeviceRecord>,
    /// The time of the oldest audit line the files still held when older ones were deleted.
    pub(crate) pruned_before: Option<u64>,
    /// How many lines of the audit files this build could not read, and so are not in `audit`.
    pub(crate) unread: usize,
}

/// Why an import or an export was refused.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MoveError {
    /// The database refused, or could not be reached.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The database holds state already.
    #[error(
        "the database holds state ({rows} rows in {table}): import writes only into a database that holds none; drop it and create it again before another import"
    )]
    HoldsState {
        /// The first table found holding rows.
        table: &'static str,
        /// Its rows.
        rows: i64,
    },
    /// What the transaction read back is not what the files hold.
    #[error(
        "what the database read back differs from the files in {part}: the import was rolled back, and the database holds none of it"
    )]
    Differs {
        /// The first table, or part, that differs.
        part: &'static str,
    },
}

/// A failed statement as the database's refusal.
fn refused(e: &postgres::Error) -> DatabaseError {
    DatabaseError::Refused(describe(e))
}

/// A session of `dsn`, which holds the coordinator's lock when `lock` is asked.
fn connect(dsn: &Dsn, lock: bool) -> Result<Session, DatabaseError> {
    let config = dsn.config(MOVE_TIMEOUTS);
    let mut session = Session::connect(&config, Arc::new(AtomicU64::new(0)), false)?;
    if lock && !try_lock(&mut session).map_err(|e| refused(&e))? {
        return Err(DatabaseError::Locked);
    }
    Ok(session)
}

/// Brings the schema of `session`, which holds the lock, up to this build, as a start does.
fn migrated(session: &mut Session) -> Result<(), DatabaseError> {
    let set =
        migrate::checked(&migrate::SHIPPED).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    migrate::apply(session, &set).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    Ok(())
}

/// The first table of the database that holds state, with its rows: a state table, an import's
/// record, or a serial counter that a serial was taken from. None for a database that holds
/// none, or that no migration has run on yet.
fn holding(session: &mut Session) -> Result<Option<(&'static str, i64)>, DatabaseError> {
    let made: bool = session
        .query_one("SELECT to_regclass('imports') IS NOT NULL", &[])
        .map_err(|e| refused(&e))?
        .get(0);
    if !made {
        return Ok(None);
    }
    let counts = STATE_TABLES
        .iter()
        .chain(&["imports"])
        .map(|table| (*table, format!("SELECT count(*) FROM {table}")))
        .chain([(
            "enrollment_serial",
            format!("SELECT count(*) FROM enrollment_serial WHERE next <> {FIRST_SERIAL}"),
        )]);
    for (table, sql) in counts {
        let rows: i64 = session
            .query_one(&sql, &[])
            .map_err(|e| refused(&e))?
            .get(0);
        if rows > 0 {
            return Ok(Some((table, rows)));
        }
    }
    Ok(None)
}

/// What the database of `dsn` holds, for an import that writes nothing: the first table that
/// holds state, if one does. Takes no lock, runs no migration and writes nothing; a schema newer
/// than this build, which `--apply` would refuse, is refused.
///
/// # Errors
/// [`DatabaseError`]: the database is not up, refused, or holds a newer schema.
pub(crate) fn plan(dsn: &Dsn) -> Result<Option<(&'static str, i64)>, DatabaseError> {
    let mut session = connect(dsn, false)?;
    session.batch("BEGIN READ ONLY").map_err(|e| refused(&e))?;
    let set =
        migrate::checked(&migrate::SHIPPED).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    migrate::refuse_newer(&mut session, &set).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    holding(&mut session)
}

/// The statements that write `moved`, its audit lines and `then` after them, into empty tables.
fn writes(moved: &Moved, lines: &[AuditLine]) -> Result<Vec<Stmt>, MoveError> {
    let load = |e: &dyn std::fmt::Display| MoveError::Database(DatabaseError::Load(e.to_string()));
    let (desired, ops, verdicts, held) = (
        desired::Held::default(),
        OpsState::default(),
        verdict_file::Held::default(),
        Accounts::default(),
    );
    let mut out = statements(&Commit {
        desired: Some((&desired, &moved.loaded.desired)),
        ops: Some((&ops, &moved.loaded.ops)),
        verdicts: Some((&verdicts, &moved.loaded.verdicts)),
        accounts: Some((&held, &moved.loaded.accounts)),
        audit: lines,
    })
    .map_err(|e| load(&e))?;
    for (jti, until) in &moved.loaded.spent {
        out.push(Stmt {
            sql: "INSERT INTO spent_tokens (jti, until_unix) VALUES ($1, $2)".to_owned(),
            params: vec![
                Val::Text(jti.clone()),
                Val::Int(state::int(*until).map_err(|e| load(&e))?),
            ],
        });
    }
    out.extend(records::writes(&moved.records).map_err(|e| load(&e))?);
    out.extend(devices::writes(&moved.devices));
    Ok(out)
}

/// The rows of each table of the desired state, the operations state and the verdicts of
/// `loaded`: what a comparison holds a load to, a per-service entry at its default left out as no
/// row keeps it (section 3.1).
fn rows_of(loaded: &Loaded) -> Result<Tables, MoveError> {
    let load = |e: &dyn std::fmt::Display| MoveError::Database(DatabaseError::Load(e.to_string()));
    let mut tables = state::desired_rows(&loaded.desired).map_err(|e| load(&e))?;
    tables.extend(state::ops_rows(&loaded.ops).map_err(|e| load(&e))?);
    tables.extend(state::verdicts_rows(&loaded.verdicts).map_err(|e| load(&e))?);
    tables.extend(accounts::rows(&loaded.accounts));
    Ok(tables)
}

/// The rows each state table would hold with `moved` imported, in [`STATE_TABLES`]' order: what
/// an import says it writes.
///
/// # Errors
/// [`MoveError`]: a part of the state could not be made into rows.
pub(crate) fn counts_of(moved: &Moved) -> Result<Counts, MoveError> {
    let tables = rows_of(&moved.loaded)?;
    let r = &moved.records;
    Ok(STATE_TABLES
        .iter()
        .map(|table| {
            let rows = match *table {
                "audit" => moved.audit.len(),
                "spent_tokens" => moved.loaded.spent.len(),
                "enrollment_requests" => r.requests.len(),
                "enrollment_issued" => r.issued.len(),
                "enrollment_hosts" => r.hosts.len(),
                "enrollment_host_running" => r.hosts.iter().filter(|h| h.running.is_some()).count(),
                "enrollment_deny" => r.deny.len(),
                "mac_nonces" => r.mac_nonces.len(),
                "mdm_devices" => moved.devices.len(),
                other => tables.get(other).map_or(0, |rows| rows.len()),
            };
            (*table, i64::try_from(rows).unwrap_or(i64::MAX))
        })
        .collect())
}

/// Every audit row `session` holds, oldest first.
fn audit_all(session: &mut Session) -> Result<Vec<AuditLine>, DatabaseError> {
    Ok(session
        .query(
            "SELECT seq, at_unix, by_name, action, service, node_id, reason, host_group FROM audit \
             ORDER BY seq",
            &[],
        )
        .map_err(|e| refused(&e))?
        .iter()
        .map(|row| {
            let logged = logged_of(row);
            AuditLine::of(&logged.entry, logged.at_unix.unwrap_or_default())
        })
        .collect())
}

/// Everything `session` holds, read by the readers a start uses, CI's spent ids past at `now`
/// left out.
fn read_moved(session: &mut Session, now: u64) -> Result<Moved, MoveError> {
    let loaded = load_all(session, now)
        .map_err(|e| MoveError::Database(DatabaseError::Load(e.to_string())))?;
    let audit = audit_all(session)?;
    let records = records::read_all(session)
        .map_err(|e| refused(&e))?
        .map_err(|e| DatabaseError::Refused(e.to_string()))?;
    let devices = devices::read_all(session)
        .map_err(|e| MoveError::Database(DatabaseError::Load(e.to_string())))?;
    let pruned: Option<i64> = session
        .query_opt(
            "SELECT audit_pruned_before FROM imports ORDER BY id DESC LIMIT 1",
            &[],
        )
        .map_err(|e| refused(&e))?
        .and_then(|row| row.get(0));
    Ok(Moved {
        loaded,
        audit,
        records,
        devices,
        pruned_before: pruned.and_then(|n| u64::try_from(n).ok()),
        unread: 0,
    })
}

/// `spent` in the order of its ids' bytes, whatever order the database's collation listed it in.
fn sorted(spent: &[(String, u64)]) -> Vec<(String, u64)> {
    let mut spent = spent.to_vec();
    spent.sort();
    spent
}

/// The first part in which `read`, what the database read back, is not `files`, whose audit
/// history is `lines`.
fn first_difference(
    files: &Moved,
    lines: &[AuditLine],
    read: &Moved,
) -> Result<Option<&'static str>, MoveError> {
    let (want, got) = (rows_of(&files.loaded)?, rows_of(&read.loaded)?);
    let empty = super::rows::Rows::new();
    let tables = state::DESIRED
        .iter()
        .chain(state::OPS.iter())
        .chain(state::VERDICTS.iter())
        .map(|table| {
            (
                table.name,
                want.get(table.name).unwrap_or(&empty) == got.get(table.name).unwrap_or(&empty),
            )
        });
    let (r, f) = (&read.records, &files.records);
    let parts = [
        (
            "spent_tokens",
            sorted(&read.loaded.spent) == sorted(&files.loaded.spent),
        ),
        ("audit", read.audit == lines),
        ("enrollment_requests", r.requests == f.requests),
        ("enrollment_issued", r.issued == f.issued),
        ("enrollment_hosts", r.hosts == f.hosts),
        ("enrollment_deny", r.deny == f.deny),
        ("mac_nonces", r.mac_nonces == f.mac_nonces),
        ("enrollment_serial", r.serial == f.serial),
        ("mdm_devices", read.devices == files.devices),
        ("imports", read.pruned_before == files.pruned_before),
    ];
    Ok(tables
        .chain(parts)
        .find(|(_, same)| !same)
        .map(|(part, _)| part))
}

/// Imports `moved` into the database of `dsn` (section 11): takes the coordinator's lock, runs
/// the migrations, refuses a database that holds state, and in one transaction writes every
/// table, the whole audit history with its numbers and then the row `imported` (its number one
/// more than the newest), the import's record with `manifest`, made at `now`; reads everything
/// back through the readers a start uses and compares it with `moved`. A difference rolls the
/// whole import back and names the first part that differs; otherwise it commits.
///
/// # Errors
/// [`MoveError`]: the database is not up or refused, another coordinator holds its lock, it
/// holds state, or what it read back differs.
pub(crate) fn import(
    dsn: &Dsn,
    moved: &Moved,
    manifest: &serde_json::Value,
    imported: &AuditLine,
    now: u64,
) -> Result<(), MoveError> {
    let mut session = connect(dsn, true)?;
    migrated(&mut session)?;
    if let Some((table, rows)) = holding(&mut session)? {
        return Err(MoveError::HoldsState { table, rows });
    }
    let mut lines = moved.audit.clone();
    lines.push(imported.clone());
    let mut statements = writes(moved, &lines)?;
    statements.push(Stmt {
        sql: "INSERT INTO imports (id, at_unix, manifest, audit_pruned_before) \
              VALUES (1, $1, $2, $3)"
            .to_owned(),
        params: vec![
            Val::Int(state::int(now).map_err(|e| DatabaseError::Load(e.to_string()))?),
            Val::Json(manifest.clone()),
            match moved.pruned_before {
                Some(at) => {
                    Val::Int(state::int(at).map_err(|e| DatabaseError::Load(e.to_string()))?)
                }
                None => Val::Null,
            },
        ],
    });
    session.batch("BEGIN").map_err(|e| refused(&e))?;
    let done = written_and_read(&mut session, &statements, moved, &lines, now);
    match done {
        Ok(()) => session.batch("COMMIT").map_err(|e| refused(&e).into()),
        Err(why) => {
            // A failed statement ended the transaction's use; the rollback ends it whole.
            let _ = session.batch("ROLLBACK");
            Err(why)
        }
    }
}

/// Runs `statements` in the open transaction of `session`, then reads it back and compares it.
fn written_and_read(
    session: &mut Session,
    statements: &[Stmt],
    moved: &Moved,
    lines: &[AuditLine],
    now: u64,
) -> Result<(), MoveError> {
    for statement in statements {
        session
            .execute(&statement.sql, &statement.refs())
            .map_err(|e| refused(&e))?;
    }
    let read = read_moved(session, now)?;
    match first_difference(moved, lines, &read)? {
        Some(part) => Err(MoveError::Differs { part }),
        None => Ok(()),
    }
}

/// The rows of each state table of the database, in [`STATE_TABLES`]' order.
fn counts(session: &mut Session) -> Result<Counts, DatabaseError> {
    let mut out = Vec::new();
    for table in STATE_TABLES {
        let rows: i64 = session
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .map_err(|e| refused(&e))?
            .get(0);
        out.push((table, rows));
    }
    Ok(out)
}

/// Reads everything the database of `dsn` holds (section 11): takes the coordinator's lock,
/// brings the schema up to this build as a start does, and reads, in one read-only snapshot of
/// the database, the state through the readers a start uses (CI's spent ids past at `now` left
/// out), the whole audit history, the enrollment records, and the rows of each state table.
///
/// # Errors
/// [`MoveError`]: the database is not up or refused, another coordinator holds its lock, or the
/// state does not load.
pub(crate) fn export(dsn: &Dsn, now: u64) -> Result<(Moved, Counts), MoveError> {
    let mut session = connect(dsn, true)?;
    migrated(&mut session)?;
    session
        .batch("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .map_err(|e| refused(&e))?;
    let moved = read_moved(&mut session, now)?;
    let counts = counts(&mut session)?;
    session.batch("COMMIT").map_err(|e| refused(&e))?;
    Ok((moved, counts))
}
