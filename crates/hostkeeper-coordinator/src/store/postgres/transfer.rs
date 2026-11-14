//! The database's side of `export` (`docs/design/coordinator-storage.md`, section 11): the whole
//! state read out of a database under the coordinator's lock, its schema first brought up to this
//! build as a start brings it.
//!
//! It runs on one session of its own, on the caller's thread (not in a runtime), and takes the
//! coordinator's advisory lock without waiting, so a coordinator serving on the database refuses
//! it. It does not raise the epoch: it is no coordinator that commits as one.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use hostkeeper_enrollment::enroll::records::Snapshot;
use hostkeeper_mdm_gate::DeviceRecord;

use super::session::{Session, describe};
use super::writer::try_lock;
use super::{DatabaseError, Dsn, Timeouts, devices, load_all, logged_of, migrate, records};
use crate::audit_file::AuditLine;
use crate::store::Loaded;

/// The bounded waits of an export: an operator's run, not a heartbeat under
/// the fleet lock, so a statement may run as long as a whole audit history takes to read.
const MOVE_TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(5),
    statement: Duration::from_secs(300),
    lock: Duration::from_secs(3),
    tcp: Duration::from_secs(5),
};

/// Every table that holds state, in the order an export counts them; `imports` is the import's
/// own record, and the bookkeeping (`schema_version`, `lease`, `enrollment_serial`) is not here.
pub(crate) const STATE_TABLES: [&str; 46] = [
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
    "lab_rows",
    "lab_racks",
    "lab_placements",
    "lab_cluster",
    "lab_devices",
    "lab_sleds",
    "lab_sled_items",
];

/// The rows of each state table, in [`STATE_TABLES`]' order.
pub(crate) type Counts = Vec<(&'static str, i64)>;

/// Everything the coordinator keeps durable, whole: what an export reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Moved {
    /// The desired state, the operations state, the verdicts, the accounts and the lab layout, with the spent ids not yet
    /// past; the ring of the operations state is not part of it, which the audit history holds.
    pub(crate) loaded: Loaded,
    /// The whole audit history, oldest first.
    pub(crate) audit: Vec<AuditLine>,
    /// The enrollment store's records.
    pub(crate) records: Snapshot,
    /// The MDM gate's device records, in the order of their enrollment ids.
    pub(crate) devices: Vec<DeviceRecord>,
    /// The time of the oldest audit line the files still held when older ones were deleted, as
    /// the import record of a database a release before this one filled from files says.
    pub(crate) pruned_before: Option<u64>,
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
fn read_moved(session: &mut Session, now: u64) -> Result<Moved, DatabaseError> {
    let loaded = load_all(session, now).map_err(|e| DatabaseError::Load(e.to_string()))?;
    let audit = audit_all(session)?;
    let records = records::read_all(session)
        .map_err(|e| refused(&e))?
        .map_err(|e| DatabaseError::Refused(e.to_string()))?;
    let devices = devices::read_all(session).map_err(|e| DatabaseError::Load(e.to_string()))?;
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
    })
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
/// [`DatabaseError`]: the database is not up or refused, another coordinator holds its lock, or the
/// state does not load.
pub(crate) fn export(dsn: &Dsn, now: u64) -> Result<(Moved, Counts), DatabaseError> {
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
