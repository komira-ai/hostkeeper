//! An edit as one transaction on a writing session (`docs/design/coordinator-multi-instance.md`,
//! sections 5.2 and 6), the Postgres side of [`crate::store::Edit`].
//!
//! [`begin`] takes a writer of the pool for the edit and hands it a job that opens the
//! transaction and holds it while the caller makes its change:
//!
//! 1. the row of `tx_ledger`, first, as every transaction's ([`super::writer`]);
//! 2. the locks of the edit's [`Scope`] in the one global order ([`Scope::locks`]): the gate,
//!    when a part a decision reads is among its parts (an operator's edit queues for it; a
//!    heartbeat's asks with `SKIP LOCKED` and is refused at once when it is held or asked for),
//!    then each part's row of `state_versions` in [`Part`]'s order, then the row of
//!    `audit_counter` when it writes audit lines. Each waits at most `--operator-gate-wait-ms`, and
//!    a wait that runs out refuses the edit, its session kept;
//! 3. each part whose version is not the one this coordinator's copy carries, read afresh under
//!    its lock (no writer of it can commit meanwhile), and handed to the caller with the number
//!    the counter gives its first audit line;
//! 4. the caller makes its change on its memory, with the fresh parts taken into it, and sends
//!    back the rows that differ and its audit lines ([`Open::commit`]); the job writes them,
//!    moves on the version of each part it wrote, and the counter past the last line, and
//!    commits. A caller that drops the edit instead rolls it back.
//!
//! The counter's row is locked from the start of the edit to its commit, so the audit numbers
//! follow the commits and a rollback leaves no gap; it is the last lock taken, so no transaction
//! waits for another's gate or part while holding it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::time::Duration;

use postgres::error::SqlState;

use super::parts::{self, Known};
use super::pool::{Lease, Pool};
use super::queue_seq;
use super::rows::{Stmt, Val, changes};
use super::session::describe;
use super::writer::{Halt, State, Tx, WriteFail, Writer};
use super::{
    LOST, Owned, Postgres, STOPPED, accounts, audit_insert, layout, run_all, state, unwritten,
};
use crate::audit_actions::AuditOnly;
use crate::store::{BackendError, Commit, Edit, Fresh, Lock, Part, Scope, Wait};

/// What a heartbeat's edit is refused with when the gate is held or asked for.
pub(crate) const BUSY: &str = "the rollout gate is held: the next heartbeat writes again";

/// What an edit is refused with when its caller dropped it.
const ABANDONED: &str = "the edit was dropped before its commit";

/// How much longer than the wait for a lock a statement of the lock phase may run, so that the
/// lock's own timeout, not the statement's, ends a wait that runs out.
const STATEMENT_MARGIN_MS: i64 = 1000;

/// What the job of an edit tells its caller.
enum Answer {
    /// The locks are held: what was read afresh, or why the edit cannot be made.
    Opened(Result<Box<Opening>, WriteFail>),
    /// The commit: the version each part it wrote moved on to, or why it did not land.
    Done(Result<BTreeMap<Part, i64>, WriteFail>),
}

/// What an edit, or a heartbeat's decision, read under its locks.
pub(super) struct Opening {
    pub(super) fresh: Fresh,
    /// The version of each part, as read under its lock, and whether this coordinator's copy
    /// of it was older.
    pub(super) read: BTreeMap<Part, (i64, bool)>,
    pub(super) next_seq: Option<u64>,
}

/// What the caller sends to be committed.
struct Planned {
    statements: Vec<Stmt>,
    /// The parts whose rows it writes, whose version moves on.
    bumps: BTreeSet<Part>,
    /// What the counter is set to: one past the last audit line, when there is one.
    next: Option<i64>,
    change: Owned,
}

/// An edit's transaction, open on a writer of the pool until it is committed or dropped.
pub(crate) struct Open {
    steps: mpsc::Sender<Planned>,
    answers: mpsc::Receiver<Answer>,
    store: Postgres,
    scope: Scope,
    read: BTreeMap<Part, (i64, bool)>,
    taken: BTreeSet<Part>,
    _lease: Lease,
}

/// Begins an edit of `scope` on `store` ([`crate::store::Backend::begin`]).
///
/// # Errors
/// [`BackendError`]: the store is lost, its locks were not free in time, or a heartbeat's edit
/// found the gate held; nothing is held.
pub(super) fn begin(store: &Postgres, scope: Scope) -> Result<Edit, BackendError> {
    // On the thread of a heartbeat's decision the edit is the decision's.
    if let Some(line) = super::decide::seated(store) {
        return super::decide::join(store, line, scope);
    }
    let inner = &store.inner;
    match inner.link.state() {
        State::Up => {}
        State::Stopped => return Err(unwritten(STOPPED)),
        State::Lost | State::Reviving => return Err(unwritten(LOST)),
    }
    let (steps, inbox) = mpsc::channel();
    let (answer, answers) = mpsc::channel();
    let known: BTreeMap<Part, Option<i64>> = scope
        .parts
        .iter()
        .map(|part| (*part, inner.known.get(*part)))
        .collect();
    let wait = inner.gate_wait;
    let lease = Pool::lease(&inner.writers, move |writer| {
        serve(writer, scope, &known, wait, &answer, &inbox);
    });
    let opened = lease.and_then(|lease| match answers.recv() {
        Ok(Answer::Opened(opened)) => Some((lease, opened)),
        _ => None,
    });
    let Some((lease, opened)) = opened else {
        // The writer's thread is gone: the store is lost for good, which the alert says.
        inner.link.stop();
        return Err(unwritten(STOPPED));
    };
    let Opening {
        fresh,
        read,
        next_seq,
    } = *opened.map_err(|fail| unwritten(fail.why()))?;
    let open = Open {
        steps,
        answers,
        store: store.clone(),
        scope,
        read,
        taken: BTreeSet::new(),
        _lease: lease,
    };
    Ok(Edit::holding(open, fresh, next_seq))
}

/// The job of an edit on `writer`: opens the transaction, answers what it read, waits for the
/// caller's change and commits it.
fn serve(
    writer: &mut Writer,
    scope: Scope,
    known: &BTreeMap<Part, Option<i64>>,
    wait: Duration,
    answer: &mpsc::Sender<Answer>,
    inbox: &mpsc::Receiver<Planned>,
) {
    let mut opened = false;
    let done = writer.transact_naming(|tx| {
        let opening = open(tx, scope, known, wait)?;
        opened = true;
        // A caller that is gone drops its sender too, which the receive below finds.
        let _ = answer.send(Answer::Opened(Ok(Box::new(opening))));
        let planned = inbox
            .recv()
            .map_err(|_| Halt::Refuse(ABANDONED.to_owned()))?;
        let bumped = write(tx, &planned)?;
        Ok((bumped, Some(planned.change)))
    });
    let _ = answer.send(match (opened, done) {
        (false, Err(fail)) => Answer::Opened(Err(fail)),
        (_, done) => Answer::Done(done),
    });
}

/// Whether `e` is a wait for a lock that ran out: the lock's timeout or the statement's.
fn waited_out(e: &postgres::Error) -> bool {
    e.code()
        .is_some_and(|code| [SqlState::LOCK_NOT_AVAILABLE, SqlState::QUERY_CANCELED].contains(code))
}

/// Takes the locks of `scope` in their order, each waiting at most `wait`, and reads afresh each
/// part whose version is not the one in `known`.
pub(super) fn open(
    tx: &mut Tx<'_>,
    scope: Scope,
    known: &BTreeMap<Part, Option<i64>>,
    wait: Duration,
) -> Result<Opening, Halt> {
    let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX - STATEMENT_MARGIN_MS);
    tx.execute(
        "SELECT set_config('lock_timeout', $1, true), set_config('statement_timeout', $2, true)",
        &[
            &format!("{wait_ms}ms"),
            &format!("{}ms", wait_ms + STATEMENT_MARGIN_MS),
        ],
    )?;
    let mut versions = BTreeMap::new();
    let mut next_seq = None;
    for lock in scope.locks() {
        let held = match lock {
            Lock::Gate => take_gate(tx, scope.wait),
            Lock::Part(part) => tx
                .query_one(
                    "SELECT version FROM state_versions WHERE part = $1 FOR UPDATE",
                    &[&part.name()],
                )
                .map(|row| versions.insert(part, row.get::<_, i64>(0)).is_none()),
            Lock::AuditCounter => tx
                .query_one(
                    "SELECT next FROM audit_counter WHERE id = 1 FOR UPDATE",
                    &[],
                )
                .map(|row| {
                    next_seq = u64::try_from(row.get::<_, i64>(0)).ok();
                    true
                }),
        };
        held.map_err(|e| match waited_out(&e) {
            true => Halt::Refuse(format!(
                "the rollout gate or a part of the state was not free within {wait_ms} ms \
                 (--operator-gate-wait-ms): {}",
                describe(&e)
            )),
            false => Halt::Pg(e),
        })?
        .then_some(())
        .ok_or_else(|| Halt::Refuse(BUSY.to_owned()))?;
    }
    tx.batch("SET LOCAL lock_timeout TO DEFAULT; SET LOCAL statement_timeout TO DEFAULT")?;
    let mut fresh = Fresh::default();
    let mut read = BTreeMap::new();
    for (part, version) in versions {
        let stale = known.get(&part).copied().flatten() != Some(version);
        if stale {
            let raw = parts::raw(tx.session(), part)?;
            absorb(
                &mut fresh,
                raw.fresh().map_err(|e| Halt::Refuse(e.to_string()))?,
            );
        }
        read.insert(part, (version, stale));
    }
    Ok(Opening {
        fresh,
        read,
        next_seq,
    })
}

/// Takes the gate as `wait` says: true when it is held now, false when a heartbeat found it held
/// or asked for.
fn take_gate(tx: &mut Tx<'_>, wait: Wait) -> Result<bool, postgres::Error> {
    match wait {
        Wait::Beat => Ok(tx
            .query_opt(
                "SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE SKIP LOCKED",
                &[],
            )?
            .is_some()),
        Wait::Operator => tx
            .query_one("SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE", &[])
            .map(|_| true),
    }
}

/// `part`, read afresh alone, laid into `fresh`.
fn absorb(fresh: &mut Fresh, part: Fresh) {
    fresh.desired = fresh.desired.take().or(part.desired);
    fresh.ops = fresh.ops.take().or(part.ops);
    fresh.accounts = fresh.accounts.take().or(part.accounts);
    fresh.layout = fresh.layout.take().or(part.layout);
    fresh.verdicts = fresh.verdicts.take().or(part.verdicts);
}

/// Writes `planned`: its rows, then the version of each part it wrote, then the counter.
fn write(tx: &mut Tx<'_>, planned: &Planned) -> Result<BTreeMap<Part, i64>, Halt> {
    run_all(tx, &planned.statements)?;
    settle(tx, &planned.bumps, planned.next)
}

/// Moves on the version of each part of `bumps`, and the counter to `next` when there is one.
pub(super) fn settle(
    tx: &mut Tx<'_>,
    bumps: &BTreeSet<Part>,
    next: Option<i64>,
) -> Result<BTreeMap<Part, i64>, Halt> {
    let mut bumped = BTreeMap::new();
    for part in bumps {
        let version: i64 = tx
            .query_one(
                "UPDATE state_versions SET version = version + 1 WHERE part = $1 \
                 RETURNING version",
                &[&part.name()],
            )?
            .get(0);
        bumped.insert(*part, version);
    }
    if let Some(next) = next {
        tx.execute("UPDATE audit_counter SET next = $1 WHERE id = 1", &[&next])?;
    }
    Ok(bumped)
}

/// The statements of `change` and the parts whose rows they write: each part's rows that differ,
/// in the order of a commit's ([`super::statements`]), then its audit lines, each line that
/// changes a host's facts raising the host's facts generation to its number. A line in the ring
/// of changes (one that is not audit-only) is a write of the ops part.
///
/// # Errors
/// [`BackendError`]: a state this build cannot write as rows, or a part written that `scope` does
/// not hold.
pub(crate) fn plan(
    change: &Commit<'_>,
    scope: Scope,
) -> Result<(Vec<Stmt>, BTreeSet<Part>), BackendError> {
    let mut statements = Vec::new();
    let mut bumps = BTreeSet::new();
    let mut part = |part: Part, written: Vec<Stmt>| {
        if !written.is_empty() {
            bumps.insert(part);
            statements.extend(written);
        }
    };
    if let Some((before, after)) = change.desired {
        let (b, a) = (state::desired_rows(before)?, state::desired_rows(after)?);
        let mut written = changes(&state::DESIRED, &b, &a);
        written.extend(queue_seq::stamps(before, after));
        part(Part::Desired, written);
    }
    if let Some((before, after)) = change.ops {
        let (b, a) = (state::ops_rows(before)?, state::ops_rows(after)?);
        part(Part::Ops, changes(&state::OPS, &b, &a));
    }
    if let Some((before, after)) = change.verdicts {
        let (b, a) = (state::verdicts_rows(before)?, state::verdicts_rows(after)?);
        part(Part::Verdicts, changes(&state::VERDICTS, &b, &a));
    }
    if let Some((before, after)) = change.accounts {
        let (b, a) = (accounts::rows(before), accounts::rows(after));
        part(Part::Accounts, changes(&accounts::TABLES, &b, &a));
    }
    if let Some((before, after)) = change.layout {
        let (b, a) = (layout::rows(before), layout::rows(after));
        part(Part::Layout, changes(&layout::TABLES, &b, &a));
    }
    for line in change.audit {
        statements.push(audit_insert(line)?);
        if !AuditOnly::is_audit_only(&line.action) {
            bumps.insert(Part::Ops);
        }
        if let (true, Some(host)) = (
            crate::facts::raises(&line.action),
            line.detail.node_id.as_deref(),
        ) {
            statements.push(Stmt {
                sql: "UPDATE enrollment_hosts \
                      SET facts_generation = GREATEST(coalesce(facts_generation, 0), $2) \
                      WHERE host_id = $1"
                    .to_owned(),
                params: vec![Val::Text(host.to_owned()), Val::Int(state::int(line.seq)?)],
            });
        }
    }
    if let Some(outside) = bumps.iter().find(|part| !scope.parts.contains(part)) {
        return Err(unwritten(&format!(
            "the change writes the part {} its edit does not hold",
            outside.name()
        )));
    }
    Ok((statements, bumps))
}

impl Open {
    /// Records that the caller took `part`, read afresh, into memory: its copy now carries the
    /// version read under the lock.
    pub(crate) fn taken(&mut self, part: Part) {
        let (known, taken) = (&self.store.inner.known, &mut self.taken);
        self.read.get(&part).iter().for_each(|(version, _)| {
            known.set(part, *version);
            taken.insert(part);
        });
    }

    /// Commits `change` in the edit's transaction, its audit lines numbered from the counter's
    /// number. Each part it wrote whose copy the caller holds as read under the lock carries the
    /// new version once it landed; a copy older than the rows and not taken still carries its
    /// old one, and a part of a commit whose outcome is not known carries none.
    ///
    /// # Errors
    /// [`BackendError`]: the change is not durable, or (a commit of unknown outcome) not known to
    /// be until the revival settles it.
    pub(crate) fn commit(self, change: &Commit<'_>) -> Result<(), BackendError> {
        let (statements, bumps) = plan(change, self.scope)?;
        let next = match change.audit.last() {
            Some(last) => Some(state::int(last.seq.saturating_add(1))?),
            None => None,
        };
        let planned = Planned {
            statements,
            bumps,
            next,
            change: Owned::of(change),
        };
        let known: &Known = &self.store.inner.known;
        let done = match self.steps.send(planned).map(|()| self.answers.recv()) {
            Ok(Ok(Answer::Done(done))) => done,
            _ => {
                self.store.inner.link.stop();
                Err(WriteFail::Lost(STOPPED.to_owned()))
            }
        };
        match done {
            Ok(bumped) => {
                for (part, version) in bumped {
                    let stale = self.read.get(&part).is_none_or(|(_, stale)| *stale);
                    if !stale || self.taken.contains(&part) {
                        known.set(part, version);
                    }
                }
                // The numbers it took are in the rows, not in memory: read the part again.
                if change
                    .desired
                    .is_some_and(|(b, a)| queue_seq::stamped(b, a))
                {
                    known.forget(Part::Desired);
                }
                self.store.reserve(change.audit);
                Ok(())
            }
            Err(WriteFail::Unknown { why, .. }) => {
                self.scope.parts.iter().for_each(|part| known.forget(*part));
                // Its numbers stay taken, whether or not the revival finds it landed.
                self.store.reserve(change.audit);
                Err(unwritten(&why))
            }
            Err(fail) => Err(unwritten(fail.why())),
        }
    }
}
