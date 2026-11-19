//! The parts of the state as rows (`docs/design/coordinator-multi-instance.md`, section 6): each
//! part's tables, read whole; the version of each part (`state_versions`); and the rule that a
//! copy of a part is read with its version in **one snapshot**, the version first, so that a copy
//! never carries a version newer than its rows.
//!
//! Every transaction that writes a part's rows moves its version on before it commits
//! ([`super::edit`]). A copy read in one snapshot that holds version `v` therefore holds exactly
//! the rows of `v`: a commit after the snapshot changes neither. Read in two (the rows, then a
//! commit of `v + 1`, then the version) the old rows would be labelled `v + 1`, and the copy
//! would never be found older than the rows again until the next change of that part.
//!
//! The ops part carries with its tables the ring of the last changes (the audit lines that are not
//! audit-only) and the hosts' facts generations (`enrollment_hosts.facts_generation`, raised in
//! the transaction of the change that raises them), so that a host is never told a generation
//! newer than the labels it is sent with.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::accounts;
use super::layout;
use super::queue_seq;
use super::rows::{Table, Tables};
use super::session::{Session, describe};
use super::state;
use super::writer::State;
use super::{logged_of, unwritten};
use crate::audit_actions::AuditOnly;
use crate::ops::{AUDIT_RING, AuditEntry};
use crate::store::{BackendError, Facts, Fresh, Loaded, Part};

/// The version of each part this coordinator's copy of it carries: none for a part whose copy
/// is not known to match any version (a commit of it whose outcome was not known).
#[derive(Debug, Default)]
pub(crate) struct Known {
    versions: Mutex<BTreeMap<Part, i64>>,
}

impl Known {
    /// The versions `versions`, as a load read them.
    pub(crate) fn of(versions: BTreeMap<Part, i64>) -> Known {
        Known {
            versions: Mutex::new(versions),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<Part, i64>> {
        self.versions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The version the copy of `part` carries.
    pub(crate) fn get(&self, part: Part) -> Option<i64> {
        self.lock().get(&part).copied()
    }

    /// Records that the copy of `part` now carries `version`.
    pub(crate) fn set(&self, part: Part, version: i64) {
        self.lock().insert(part, version);
    }

    /// Records that the copy of `part` matches no version known: the next request reads it again.
    pub(crate) fn forget(&self, part: Part) {
        self.lock().remove(&part);
    }
}

/// What a part's rows are, as read: its tables, and for the ops part its ring and the facts
/// generations.
pub(crate) struct Raw {
    part: Part,
    tables: Tables,
    ring: Vec<AuditEntry>,
    facts: Facts,
    /// For the desired part, the numbers its versions were set under ([`super::queue_seq`]).
    seqs: BTreeMap<(Option<String>, String), i64>,
}

/// The tables of `part`.
fn tables_of(part: Part) -> &'static [Table] {
    match part {
        Part::Desired => &state::DESIRED,
        Part::Ops => &state::OPS,
        Part::Accounts => &accounts::TABLES,
        Part::Layout => &layout::TABLES,
        Part::Verdicts => &state::VERDICTS,
    }
}

/// Reads `part`'s rows on `session`, in the transaction it is in.
///
/// # Errors
/// The client's error.
pub(crate) fn raw(session: &mut Session, part: Part) -> Result<Raw, postgres::Error> {
    let mut tables = Tables::new();
    for table in tables_of(part) {
        let rows = session.query(&table.select(), &[])?;
        tables.insert(table.name, table.rows_of(&rows)?);
    }
    let (ring, facts) = match part {
        Part::Ops => (ring(session)?, facts(session)?),
        _ => (Vec::new(), Facts::new()),
    };
    let seqs = match part {
        Part::Desired => queue_seq::read(session)?,
        _ => BTreeMap::new(),
    };
    Ok(Raw {
        part,
        tables,
        ring,
        facts,
        seqs,
    })
}

/// The ring of the last changes: the newest audit lines that are not audit-only (the console's
/// stay out of it, as they do in memory, [`crate::ops::OpsStore::append_audit_only`]), oldest
/// first.
fn ring(session: &mut Session) -> Result<Vec<AuditEntry>, postgres::Error> {
    let audit_only: Vec<&str> = AuditOnly::ALL.iter().map(|only| only.as_str()).collect();
    let rows = session.query(
        "SELECT seq, at_unix, by_name, action, service, node_id, reason, host_group, api_token \
         FROM audit WHERE action <> ALL($2) ORDER BY seq DESC LIMIT $1",
        &[&(AUDIT_RING as i64), &audit_only],
    )?;
    Ok(rows.iter().rev().map(|row| logged_of(row).entry).collect())
}

/// The facts generation of every host that has one.
fn facts(session: &mut Session) -> Result<Facts, postgres::Error> {
    let rows = session.query(
        "SELECT host_id, facts_generation FROM enrollment_hosts \
         WHERE facts_generation IS NOT NULL ORDER BY host_id",
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|row| {
            let generation: i64 = row.get(1);
            (
                row.get::<_, String>(0),
                u64::try_from(generation).unwrap_or_default(),
            )
        })
        .collect())
}

impl Raw {
    /// The part as the store holds it, in a [`Fresh`] of that part alone.
    ///
    /// # Errors
    /// [`BackendError`]: the rows hold what this build refuses.
    pub(crate) fn fresh(self) -> Result<Fresh, BackendError> {
        let tables = &self.tables;
        let mut fresh = Fresh::default();
        match self.part {
            Part::Desired => {
                let mut held = state::desired_of(tables)?;
                held.seqs = self.seqs;
                fresh.desired = Some(held);
            }
            Part::Ops => {
                let mut ops = state::ops_of(tables)?;
                ops.audit = self.ring;
                fresh.ops = Some((ops, self.facts));
            }
            Part::Accounts => fresh.accounts = Some(accounts::of_tables(tables)?),
            Part::Layout => fresh.layout = Some(layout::of_tables(tables)?),
            Part::Verdicts => fresh.verdicts = Some(state::verdicts_of(tables)?),
        }
        Ok(fresh)
    }
}

/// The version of every part, by part.
///
/// # Errors
/// The client's error.
pub(crate) fn versions(session: &mut Session) -> Result<BTreeMap<Part, i64>, postgres::Error> {
    let rows = session.query("SELECT part, version FROM state_versions", &[])?;
    Ok(Part::ALL
        .into_iter()
        .filter_map(|part| {
            let row = rows
                .iter()
                .find(|row| row.get::<_, &str>(0) == part.name())?;
            Some((part, row.get::<_, i64>(1)))
        })
        .collect())
}

/// Runs `read` in one read-only transaction of one snapshot (`REPEATABLE READ`) on `session`,
/// rolled back when it fails.
///
/// # Errors
/// The client's error.
pub(crate) fn in_snapshot<R>(
    session: &mut Session,
    read: impl FnOnce(&mut Session) -> Result<R, postgres::Error>,
) -> Result<R, postgres::Error> {
    session.batch("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
    match read(session) {
        Ok(answer) => {
            session.batch("COMMIT")?;
            Ok(answer)
        }
        Err(e) => {
            // A session that cannot roll back is broken, and its caller drops it.
            let _ = session.batch("ROLLBACK");
            Err(e)
        }
    }
}

/// What a reload runs between the read of a part's version and the read of its rows: nothing,
/// but for a test that commits a change of the part there.
pub(crate) type Between = Box<dyn FnOnce() + Send>;

/// `part` read again on `session` when its version is not `known`: rows and version in one
/// snapshot, the version first, `between` run between the two reads. `None` when the version is
/// `known`.
///
/// # Errors
/// The client's error.
pub(crate) fn reread(
    session: &mut Session,
    part: Part,
    known: Option<i64>,
    between: Between,
) -> Result<Option<(i64, Raw)>, postgres::Error> {
    in_snapshot(session, |session| {
        let version: i64 = session
            .query_one(
                "SELECT version FROM state_versions WHERE part = $1",
                &[&part.name()],
            )?
            .get(0);
        if known == Some(version) {
            return Ok(None);
        }
        between();
        Ok(Some((version, raw(session, part)?)))
    })
}

/// What [`load`] reads: the versions, every part's rows, and the spent ids' rows.
type Read = (BTreeMap<Part, i64>, Vec<Raw>, Vec<postgres::Row>);

/// Reads everything [`load`] takes, in the transaction `session` is in.
fn read_all(session: &mut Session, now: i64) -> Result<Read, postgres::Error> {
    let versions = versions(session)?;
    let raws = Part::ALL
        .into_iter()
        .map(|part| raw(session, part))
        .collect::<Result<Vec<_>, _>>()?;
    let spent = session.query(
        "SELECT jti, until_unix FROM spent_tokens WHERE until_unix > $1 ORDER BY jti",
        &[&now],
    )?;
    Ok((versions, raws, spent))
}

/// Everything the database holds for the coordinator, read on `session` at start in one
/// snapshot with the version of each part, the spent ids past at `now` left out.
///
/// # Errors
/// [`BackendError`]: what is held cannot be read, or holds what this build refuses.
pub(crate) fn load(
    session: &mut Session,
    now: u64,
) -> Result<(Loaded, BTreeMap<Part, i64>), BackendError> {
    let now = state::int(now)?;
    let read = in_snapshot(session, |session| read_all(session, now))
        .map_err(|e| unwritten(&describe(&e)))?;
    loaded_of(read)
}

/// [`load`] in the snapshot of the transaction `session` is already in: an export's.
///
/// # Errors
/// [`BackendError`], as [`load`].
pub(crate) fn load_here(session: &mut Session, now: u64) -> Result<Loaded, BackendError> {
    let now = state::int(now)?;
    let read = read_all(session, now).map_err(|e| unwritten(&describe(&e)))?;
    Ok(loaded_of(read)?.0)
}

/// The state `read` holds, and the versions read with it.
fn loaded_of(read: Read) -> Result<(Loaded, BTreeMap<Part, i64>), BackendError> {
    let (versions, raws, spent) = read;
    let mut loaded = Loaded::default();
    for raw in raws {
        let fresh = raw.fresh()?;
        loaded.desired = fresh.desired.unwrap_or(loaded.desired);
        if let Some((ops, facts)) = fresh.ops {
            loaded.ops = ops;
            loaded.facts = Some(facts);
        }
        loaded.accounts = fresh.accounts.unwrap_or(loaded.accounts);
        loaded.layout = fresh.layout.unwrap_or(loaded.layout);
        loaded.verdicts = fresh.verdicts.unwrap_or(loaded.verdicts);
    }
    loaded.spent = spent
        .iter()
        .map(|row| {
            let until: i64 = row.get(1);
            (
                row.get::<_, String>(0),
                u64::try_from(until).unwrap_or_default(),
            )
        })
        .collect();
    Ok((loaded, versions))
}

impl super::Postgres {
    /// [`Backend::stale`]: the parts whose versions moved on from this coordinator's copies.
    pub(super) fn stale_parts(&self) -> Vec<Part> {
        if self.inner.link.state() != State::Up {
            return Vec::new();
        }
        match self.read(versions) {
            Ok(versions) => Part::ALL
                .into_iter()
                .filter(|part| versions.get(part).copied() != self.inner.known.get(*part))
                .collect(),
            Err(why) => {
                tracing::debug!(%why, "the versions of the parts could not be read");
                Vec::new()
            }
        }
    }

    /// [`Backend::reload`] of `part`.
    pub(super) fn reload_part(&self, part: Part) -> Result<Fresh, BackendError> {
        let known = self.inner.known.get(part);
        let between = self.between();
        let reread = self
            .read(move |session| reread(session, part, known, between))
            .map_err(|why| unwritten(&why))?;
        let Some((version, raw)) = reread else {
            return Ok(Fresh::default());
        };
        let fresh = raw.fresh()?;
        self.inner.known.set(part, version);
        Ok(fresh)
    }
}
