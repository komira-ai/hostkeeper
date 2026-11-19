//! A heartbeat's deciding transaction on a writing session (`docs/design/coordinator-multi-instance.md`,
//! sections 5.2 and 5.3), the Postgres side of [`crate::store::Decision`].
//!
//! [`open`] takes a writer of the pool for the decision and hands it a job that opens the
//! transaction and holds it until the decision ends:
//!
//! 1. the row of `tx_ledger`, first ([`super::writer`]);
//! 2. the locks of [`Scope::DECIDE`] in the one global order: the gate with `SKIP LOCKED` (held,
//!    or asked for by an operator's edit that waits, it refuses the decision at once:
//!    [`Opened::Busy`]), the rows of `state_versions` of the parts a decision reads and writes,
//!    and the audit counter;
//! 3. each of those parts this coordinator's copy is older than, read afresh under its lock, and
//!    the fleet's rows ([`read`]), each statement seeing every commit made before the gate was
//!    granted;
//! 4. while the caller decides, each change a store makes is an edit **joined** to the decision
//!    ([`join`]): the store's thread is the deciding one, so its edit holds no locks of its own,
//!    and its rows and audit lines are written in the decision's transaction at once, numbered
//!    on from the counter's number; a stage the decision enters is first claimed by a
//!    compare-and-set on the plan's row ([`claim`]);
//! 5. at the end ([`Open::finish`]) the orders that differ from those read, each conditional on
//!    the row as read, the tries no longer outstanding, the version of each part written and the
//!    counter; then the commit.
//!
//! A write that fails ends the decision: its transaction is rolled back, every later change of it
//! is refused, and its end answers the failure. A caller that drops the decision rolls it back.
//! A decision whose commit outcome is not known writes nothing twice: it carries no change of its
//! own to settle, and the parts it held are read again before they are trusted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::edit::{self, BUSY, Opening};
use super::pool::{Lease, Pool};
use super::rows::{Stmt, Val};
use super::session::describe;
use super::writer::{Halt, State, Tx, WriteFail, Writer};
use super::{LOST, Postgres, STOPPED, run_all, unwritten};
use crate::store::decision::{BakeRow, HostRow, OrderRow, Rows, StageClaim, TryRow, Writes};
use crate::store::{BackendError, Commit, Decision, Edit, Opened, Part, Scope};

/// What the decision is refused with when its caller dropped it.
const ABANDONED: &str = "the decision was dropped before its commit";

/// What a change is refused with after a write of its decision failed.
const ENDED: &str = "a write of this heartbeat's decision failed: it is rolled back";

/// The decision a thread of this coordinator holds open, if any: a store's edit begun on that
/// thread joins it.
pub(crate) type Seat = Mutex<Option<(ThreadId, Arc<Line>)>>;

/// What the caller sends the decision's job.
enum Step {
    /// The rows and audit lines of a store's change, the parts they write and the counter's next
    /// number after them.
    Write {
        statements: Vec<Stmt>,
        bumps: BTreeSet<Part>,
        next: Option<i64>,
    },
    /// A compare-and-set whose one row answers whether it took.
    Claim(Stmt),
    /// The orders and tries, then the commit.
    Finish(Writes),
}

/// What the job answers.
enum Reply {
    Opened(Result<Box<(Opening, Rows)>, WriteFail>),
    Wrote(Result<(), String>),
    Claimed(Result<bool, String>),
    Done(Result<BTreeMap<Part, i64>, WriteFail>),
}

/// The line to the decision's job, shared by the decision and the edits joined to it.
pub(crate) struct Line {
    steps: Mutex<mpsc::Sender<Step>>,
    replies: Mutex<mpsc::Receiver<Reply>>,
    /// The number the next audit line takes.
    next_seq: Mutex<Option<u64>>,
    /// Why a write of the decision failed, once one did.
    failed: Mutex<Option<String>>,
    /// Whether a change or a claim of the decision wrote a row.
    wrote: Mutex<bool>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Line {
    /// Sends `step` and waits for its reply; none when the job is gone.
    fn ask(&self, step: Step) -> Option<Reply> {
        let replies = locked(&self.replies);
        // A job that ended (a failed write) dropped its inbox; what it answered last is still in
        // the channel.
        let _ = locked(&self.steps).send(step);
        replies.recv().ok()
    }

    fn fail(&self, why: &str) -> BackendError {
        locked(&self.failed).get_or_insert_with(|| why.to_owned());
        unwritten(why)
    }

    fn failed(&self) -> Option<String> {
        locked(&self.failed).clone()
    }
}

/// A decision's transaction, open on a writer of the pool until it is finished or dropped.
pub(crate) struct Open {
    line: Arc<Line>,
    store: Postgres,
    /// The version of each part, as read under its lock, and whether this coordinator's copy of
    /// it was older.
    read: BTreeMap<Part, (i64, bool)>,
    _lease: Lease,
}

/// Opens a heartbeat's decision on `store` ([`crate::store::Backend::decide`]).
///
/// # Errors
/// [`BackendError`]: the store is lost, or the decision's locks or reads failed; nothing is held.
pub(super) fn open(store: &Postgres) -> Result<Opened, BackendError> {
    let inner = &store.inner;
    match inner.link.state() {
        State::Up => {}
        State::Stopped => return Err(unwritten(STOPPED)),
        State::Lost | State::Reviving => return Err(unwritten(LOST)),
    }
    let (steps, inbox) = mpsc::channel();
    let (reply, replies) = mpsc::channel();
    let known: BTreeMap<Part, Option<i64>> = Scope::DECIDE
        .parts
        .iter()
        .map(|part| (*part, inner.known.get(*part)))
        .collect();
    let (wait, instance) = (inner.gate_wait, inner.instance.id.clone());
    let lease = Pool::lease_deciding(&inner.writers, move |writer| {
        serve(writer, &known, wait, &instance, &reply, &inbox);
    });
    let opened = lease.and_then(|lease| match replies.recv() {
        Ok(Reply::Opened(opened)) => Some((lease, opened)),
        _ => None,
    });
    let Some((lease, opened)) = opened else {
        // The writer's thread is gone: the store is lost for good, which the alert says.
        inner.link.stop();
        return Err(unwritten(STOPPED));
    };
    let (opening, rows) = match opened {
        Ok(opened) => *opened,
        Err(WriteFail::Refused(why)) if why == BUSY => return Ok(Opened::Busy),
        Err(fail) => return Err(unwritten(fail.why())),
    };
    let line = Arc::new(Line {
        steps: Mutex::new(steps),
        replies: Mutex::new(replies),
        next_seq: Mutex::new(opening.next_seq),
        failed: Mutex::new(None),
        wrote: Mutex::new(false),
    });
    *locked(&inner.deciding) = Some((thread::current().id(), Arc::clone(&line)));
    let open = Open {
        line,
        store: store.clone(),
        read: opening.read,
        _lease: lease,
    };
    Ok(Opened::Deciding(Box::new(Decision::new(
        open,
        opening.fresh,
        rows,
    ))))
}

/// The job of a decision on `writer`: opens the transaction, answers what it read, writes each
/// step the caller sends and commits at its end.
fn serve(
    writer: &mut Writer,
    known: &BTreeMap<Part, Option<i64>>,
    wait: Duration,
    instance: &str,
    reply: &mpsc::Sender<Reply>,
    inbox: &mpsc::Receiver<Step>,
) {
    let mut opened = false;
    let done = writer.transact_naming(|tx| {
        let opening = edit::open(tx, Scope::DECIDE, known, wait)?;
        let rows = read(tx, instance)?;
        opened = true;
        // A caller that is gone drops its sender too, which the receive below finds.
        let _ = reply.send(Reply::Opened(Ok(Box::new((opening, rows)))));
        let (mut bumps, mut next) = (BTreeSet::new(), None);
        loop {
            let step = inbox
                .recv()
                .map_err(|_| Halt::Refuse(ABANDONED.to_owned()))?;
            match step {
                Step::Write {
                    statements,
                    bumps: written,
                    next: after,
                } => {
                    let wrote = run_all(tx, &statements);
                    let _ = reply.send(Reply::Wrote(wrote.as_ref().map(|_| ()).map_err(describe)));
                    wrote?;
                    bumps.extend(written);
                    next = after.or(next);
                }
                Step::Claim(statement) => {
                    let claimed = tx
                        .query_one(&statement.sql, &statement.refs())
                        .map(|row| row.get::<_, bool>(0));
                    let _ = reply.send(Reply::Claimed(
                        claimed.as_ref().map(|took| *took).map_err(describe),
                    ));
                    claimed?;
                }
                Step::Finish(writes) => {
                    orders(tx, &writes)?;
                    return Ok((edit::settle(tx, &bumps, next)?, None));
                }
            }
        }
    });
    let _ = reply.send(match (opened, done) {
        (false, Err(fail)) => Reply::Opened(Err(fail)),
        (_, done) => Reply::Done(done),
    });
}

/// The fleet's rows, read under the gate: the database's clock and the quiet time, the rows of
/// hosts another instance than `instance` heard last, every bake, order and try.
fn read(tx: &mut Tx<'_>, instance: &str) -> Result<Rows, postgres::Error> {
    let clock = tx.query_one(
        "SELECT extract(epoch FROM now())::bigint, \
         (SELECT until_unix FROM cluster_quiet WHERE id = 1)",
        &[],
    )?;
    let hosts = tx
        .query(
            "SELECT host_id, \"role\", beat, seen_unix, sent_unix, \"instance\" FROM fleet_hosts \
             WHERE \"instance\" <> $1",
            &[&instance],
        )?
        .iter()
        .map(|row| HostRow {
            host_id: row.get(0),
            role: row.get(1),
            beat: row.get(2),
            seen_unix: row.get(3),
            sent_unix: row.get(4),
            instance: row.get(5),
        })
        .collect();
    let bakes = tx
        .query(
            "SELECT host_id, \"service\", \"target\", \"beats\", at_unix FROM fleet_bakes",
            &[],
        )?
        .iter()
        .map(|row| BakeRow {
            host_id: row.get(0),
            service: row.get(1),
            target: row.get(2),
            beats: i64::from(row.get::<_, i32>(3)),
            at_unix: row.get(4),
        })
        .collect();
    let orders = tx
        .query(
            "SELECT host_id, \"service\", \"target\", uninstall_id, at_unix, \"try\" \
             FROM fleet_orders",
            &[],
        )?
        .iter()
        .map(|row| OrderRow {
            host_id: row.get(0),
            service: row.get(1),
            target: row.get(2),
            uninstall_id: row.get(3),
            at_unix: row.get(4),
            order_try: row.get(5),
        })
        .collect();
    let tries = tx
        .query("SELECT host_id, \"service\", \"try\" FROM fleet_tries", &[])?
        .iter()
        .map(|row| TryRow {
            host_id: row.get(0),
            service: row.get(1),
            order_try: row.get(2),
        })
        .collect();
    Ok(Rows {
        now_unix: clock.get(0),
        quiet_until: clock.get::<_, Option<i64>>(1).unwrap_or(0),
        hosts,
        bakes,
        orders,
        tries,
    })
}

/// What a write of the orders is refused with when a row is no longer as the decision read it.
fn moved(row: &OrderRow) -> Halt {
    Halt::Refuse(format!(
        "the order row of the host {} is not as this decision read it: nothing is ordered",
        row.host_id
    ))
}

fn key(row: &OrderRow) -> Vec<Val> {
    vec![
        Val::Text(row.host_id.clone()),
        Val::Text(row.service.clone()),
        Val::Text(row.target.clone()),
        Val::Int(row.at_unix),
    ]
}

fn values(row: &OrderRow) -> Vec<Val> {
    vec![
        Val::Text(row.service.clone()),
        Val::Text(row.target.clone()),
        Val::text_or_null(row.uninstall_id.as_deref()),
        Val::Int(row.at_unix),
        Val::Int(row.order_try),
    ]
}

/// Writes the orders and tries of `writes`, each conditional on the rows as the decision read
/// them: an ended order is deleted if it is still there (a beat may have freed it meanwhile, which
/// only frees a slot); a replaced one is changed only if it is still as read, and a given one
/// inserted only if its host holds none. The host's primary key is the backstop of one update
/// at a time on a host: a second order is refused, and the decision with it.
fn orders(tx: &mut Tx<'_>, writes: &Writes) -> Result<(), Halt> {
    for row in &writes.ended {
        let ended = Stmt {
            sql: "DELETE FROM fleet_orders WHERE host_id = $1 AND \"service\" = $2 \
                  AND \"target\" = $3 AND at_unix = $4"
                .to_owned(),
            params: key(row),
        };
        tx.execute(&ended.sql, &ended.refs())?;
    }
    for (was, now) in &writes.replaced {
        let replaced = Stmt {
            sql: "UPDATE fleet_orders SET \"service\" = $5, \"target\" = $6, uninstall_id = $7, \
                  at_unix = $8, \"try\" = $9 WHERE host_id = $1 AND \"service\" = $2 \
                  AND \"target\" = $3 AND at_unix = $4"
                .to_owned(),
            params: key(was).into_iter().chain(values(now)).collect(),
        };
        if tx.execute(&replaced.sql, &replaced.refs())? != 1 {
            return Err(moved(was));
        }
    }
    for row in &writes.given {
        let given = Stmt {
            sql: "INSERT INTO fleet_orders \
                  (host_id, \"service\", \"target\", uninstall_id, at_unix, \"try\") \
                  VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (host_id) DO NOTHING"
                .to_owned(),
            params: std::iter::once(Val::Text(row.host_id.clone()))
                .chain(values(row))
                .collect(),
        };
        if tx.execute(&given.sql, &given.refs())? != 1 {
            return Err(moved(row));
        }
    }
    for row in &writes.tries {
        tx.execute(
            "INSERT INTO fleet_tries (host_id, \"service\", \"try\") VALUES ($1, $2, $3) \
             ON CONFLICT (host_id, \"service\") \
             DO UPDATE SET \"try\" = greatest(fleet_tries.\"try\", excluded.\"try\")",
            &[&row.host_id, &row.service, &row.order_try],
        )?;
    }
    Ok(())
}

/// The decision this thread holds open on `store`, if any.
pub(super) fn seated(store: &Postgres) -> Option<Arc<Line>> {
    let me = thread::current().id();
    locked(&store.inner.deciding)
        .as_ref()
        .filter(|(by, _)| *by == me)
        .map(|(_, line)| Arc::clone(line))
}

/// A store's edit of `scope`, begun on the thread that holds the decision `line`: it takes no
/// lock (the decision holds every lock of its parts), reads nothing afresh (the decision did),
/// and its commit writes into the decision's transaction.
///
/// # Errors
/// [`BackendError`]: a part outside the decision's, or a decision a write of which failed.
pub(super) fn join(store: &Postgres, line: Arc<Line>, scope: Scope) -> Result<Edit, BackendError> {
    if let Some(outside) = scope
        .parts
        .iter()
        .find(|part| !Scope::DECIDE.parts.contains(part))
    {
        return Err(unwritten(&format!(
            "a heartbeat's decision does not hold the part {}",
            outside.name()
        )));
    }
    if let Some(why) = line.failed() {
        return Err(unwritten(&why));
    }
    let next_seq = *locked(&line.next_seq);
    let store = store.clone();
    Ok(Edit::joined(Joined { line, scope, store }, next_seq))
}

/// A store's edit joined to the decision its thread holds.
pub(crate) struct Joined {
    line: Arc<Line>,
    scope: Scope,
    store: Postgres,
}

impl Joined {
    /// Writes `change` in the decision's transaction, its audit lines numbered from the number
    /// the edit was begun with.
    ///
    /// # Errors
    /// [`BackendError`]: the change writes a part its edit does not hold, or its rows were
    /// refused, which ends the decision.
    pub(crate) fn commit(self, change: &Commit<'_>) -> Result<(), BackendError> {
        let (statements, bumps) = edit::plan(change, self.scope)?;
        let next = match change.audit.last() {
            Some(last) => Some(super::state::int(last.seq.saturating_add(1))?),
            None => None,
        };
        let step = Step::Write {
            statements,
            bumps,
            next,
        };
        match self.line.ask(step) {
            Some(Reply::Wrote(Ok(()))) => *locked(&self.line.wrote) = true,
            Some(Reply::Wrote(Err(why))) => return Err(self.line.fail(&why)),
            _ => return Err(self.line.fail(ENDED)),
        }
        if let Some(next) = next {
            *locked(&self.line.next_seq) = u64::try_from(next).ok();
        }
        self.store.reserve(change.audit);
        Ok(())
    }
}

/// Claims the stage of `claim` in the decision this thread holds on `store`: the plan's row moves
/// to the stage `to` only if it is at the stage the decision read it at (or, when it read no plan
/// at this version, there is none at it yet). True when this decision took it; outside a
/// decision, true (the edit that writes the stage holds the gate).
///
/// # Errors
/// [`BackendError`]: the statement failed, which ends the decision.
pub(super) fn claim(store: &Postgres, claim: &StageClaim<'_>) -> Result<bool, BackendError> {
    let Some(line) = seated(store) else {
        return Ok(true);
    };
    let int = |stage: u64| super::state::int(stage).map_err(BackendError::from);
    let (table, group) = match claim.group {
        Some(group) => ("group_rollouts", Some(Val::Text(group.to_owned()))),
        None => ("rollouts", None),
    };
    let mut params = vec![
        Val::Text(claim.service.to_owned()),
        Val::Text(claim.version.to_owned()),
    ];
    let at = |n: usize| match claim.group {
        Some(_) => format!(" AND host_group = ${n}"),
        None => String::new(),
    };
    let sql = match claim.from {
        Some(from) => {
            params.extend([Val::Int(int(from)?), Val::Int(int(claim.to)?)]);
            format!(
                "WITH taken AS (UPDATE {table} SET entered = $4 WHERE \"service\" = $1 \
                 AND \"version\" = $2 AND entered = $3{} RETURNING 1) \
                 SELECT EXISTS (SELECT 1 FROM taken)",
                at(5)
            )
        }
        None => format!(
            "SELECT NOT EXISTS (SELECT 1 FROM {table} WHERE \"service\" = $1 \
             AND \"version\" = $2{})",
            at(3)
        ),
    };
    params.extend(group);
    match line.ask(Step::Claim(Stmt { sql, params })) {
        Some(Reply::Claimed(Ok(took))) => {
            *locked(&line.wrote) |= took;
            Ok(took)
        }
        Some(Reply::Claimed(Err(why))) => Err(line.fail(&why)),
        _ => Err(line.fail(ENDED)),
    }
}

impl Open {
    /// Records that the caller took `part`, read afresh, into memory: its copy now carries the
    /// version read under the lock.
    pub(crate) fn taken(&mut self, part: Part) {
        let known = &self.store.inner.known;
        // Every part of the decision's scope was read under its lock.
        self.read
            .get(&part)
            .iter()
            .for_each(|(version, _)| known.set(part, *version));
    }

    /// Writes `writes` and commits. Each part written carries its new version once the commit
    /// landed, but the desired state, whose rows the commit stamps with numbers memory does not
    /// hold: it is read again. A decision that failed leaves every part of it to be read again.
    ///
    /// # Errors
    /// [`BackendError`]: a write of the decision failed, or the commit did not land or its
    /// outcome is not known.
    pub(crate) fn finish(self, writes: &Writes) -> Result<(), BackendError> {
        let known = &self.store.inner.known;
        if self.line.failed().is_none() && !*locked(&self.line.wrote) && writes.is_empty() {
            // Nothing to write: the transaction is rolled back, which frees the gate as a commit
            // would and leaves no row of the ledger, so a heartbeat that decides nothing writes
            // nothing.
            return Ok(());
        }
        let done = match self.line.failed() {
            Some(why) => Err(unwritten(&why)),
            None => match self.line.ask(Step::Finish(writes.clone())) {
                Some(Reply::Done(Ok(bumped))) => Ok(bumped),
                Some(Reply::Done(Err(fail))) => Err(unwritten(fail.why())),
                _ => {
                    self.store.inner.link.stop();
                    Err(unwritten(STOPPED))
                }
            },
        };
        match done {
            Ok(bumped) => {
                for (part, version) in &bumped {
                    known.set(*part, *version);
                }
                if bumped.contains_key(&Part::Desired) {
                    known.forget(Part::Desired);
                }
                Ok(())
            }
            Err(why) => {
                Scope::DECIDE
                    .parts
                    .iter()
                    .for_each(|part| known.forget(*part));
                Err(why)
            }
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // The seat is this decision's: one thread holds one decision at a time.
        locked(&self.store.inner.deciding).take_if(|(_, line)| Arc::ptr_eq(line, &self.line));
    }
}
