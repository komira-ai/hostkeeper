//! The writing sessions (sections 4.4 and 5): the pool of sessions of an instance through which
//! every write goes (`--database-pool`, `docs/design/coordinator-multi-instance.md`, section 4,
//! "Sessions"), each on its own thread, so a write waits for a free session, not for every write
//! before it. Each session holds the coordinator's advisory lock of the database in **shared**
//! mode: any number of coordinators hold it together, and a release before this one, which takes
//! it exclusively, is refused while any of them runs (section 4).
//!
//! Every transaction inserts a row of `tx_ledger` as its first statement and so has an id before
//! its `COMMIT`. A session lost while its `COMMIT` was in flight is settled at the reconnect by
//! inserting the same id again, doing nothing when it exists: the insert waits for the first
//! transaction while it is in progress, so the database says whether it landed (no row inserted:
//! it did; one inserted: it did not, and now never will).
//!
//! A session that a write finds gone, or whose bounded wait ran out, is **lost**: its writer drops
//! it, and from then on every call is answered at once that the store is lost ([`Link`]), whichever
//! session it would have taken, until a revival ([`Writer::revive`], asked for at each heartbeat
//! and run on every writer's thread, never under the caller's locks) opens each session that is
//! gone, or found gone, again and takes the lock again. The store is up again once no session is
//! lost. Another coordinator having started meanwhile is normal: there is no leader and nothing
//! to be deposed by.
//!
//! A writer that ends (the store stopped or dropped) frees the lock before its session closes,
//! so an exclusive taker succeeds at once.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::Owned;
use super::session::{Fail, Params, Session, describe, ends_session};

/// The key of the coordinator's advisory lock: a constant that never changes, which the trigger
/// `lease_fenced` of `0001` names by its two halves.
pub(crate) const LOCK_KEY: i64 = 0x686b_6370_6731;

/// How long a row of `tx_ledger` is kept. A revival that finds a commit older than this, and its
/// row gone, refuses to answer that it did not land.
const LEDGER_KEEP_SECS: i64 = 3600;

/// The state of the writing sessions, as every caller sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Every session holds the lock and takes writes.
    Up,
    /// A session is lost; nothing is written until it is revived.
    Lost,
    /// The lost sessions are being opened, on their writers' threads.
    Reviving,
    /// A writer's thread is gone: nothing is written again.
    Stopped,
}

/// What a commit of unknown outcome turned out to be, once the session is back.
#[derive(Clone, Debug)]
pub(crate) struct Settled {
    /// The id of the transaction (its row of `tx_ledger`).
    pub(crate) id: String,
    /// Whether it landed.
    pub(crate) landed: bool,
    /// The change it was, when its caller handed one over.
    pub(crate) change: Option<Owned>,
}

#[derive(Debug)]
struct Shared {
    /// How many writers the pool has.
    writers: usize,
    /// How many of them hold no session.
    lost: usize,
    /// How many writers have yet to run the revival last asked for.
    reviving: usize,
    /// How many commits of unknown outcome wait for their writer's revival.
    unsettled: usize,
    /// Whether a writer's thread is gone: the store is lost for good.
    stopped: bool,
    /// What the revivals found, in their order, until they are taken.
    settled: Vec<Settled>,
}

/// The state of the writing sessions, shared by the writers and their callers.
#[derive(Debug)]
pub(crate) struct Link {
    shared: Mutex<Shared>,
}

impl Link {
    /// The link of a pool of `writers` writers, each with its session.
    pub(crate) fn new(writers: usize) -> Link {
        Link {
            shared: Mutex::new(Shared {
                writers,
                lost: 0,
                reviving: 0,
                unsettled: 0,
                stopped: false,
                settled: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sessions' state now: up while no session is lost.
    pub(crate) fn state(&self) -> State {
        let shared = self.lock();
        match (shared.stopped, shared.lost, shared.reviving) {
            (true, _, _) => State::Stopped,
            (false, 0, _) => State::Up,
            (false, _, 1..) => State::Reviving,
            (false, _, 0) => State::Lost,
        }
    }

    /// Records that a writer's session is lost when `held` (it had one until now), with a commit
    /// of unknown outcome when `unknown`.
    fn lose(&self, held: bool, unknown: bool) {
        let mut shared = self.lock();
        shared.lost += usize::from(held);
        shared.unsettled += usize::from(unknown);
    }

    /// Records that a writer's thread is gone: the store is lost for good.
    pub(crate) fn stop(&self) {
        self.lock().stopped = true;
    }

    /// Marks a store with a lost session as being revived: true when it was lost and no revival
    /// is under way, so the caller hands the revival to every writer; false otherwise.
    pub(crate) fn start_revival(&self) -> bool {
        let mut shared = self.lock();
        if shared.lost > 0 && shared.reviving == 0 && !shared.stopped {
            shared.reviving = shared.writers;
            return true;
        }
        false
    }

    /// Records that a writer ran the revival: with its session back when `back`, and the commit
    /// of unknown outcome it settled, if it had one.
    fn revived(&self, back: bool, settled: Option<Settled>) {
        let mut shared = self.lock();
        shared.reviving = shared.reviving.saturating_sub(1);
        if back {
            shared.lost = shared.lost.saturating_sub(1);
        }
        if let Some(settled) = settled {
            shared.unsettled = shared.unsettled.saturating_sub(1);
            shared.settled.push(settled);
        }
    }

    /// The outcomes of the commits of unknown outcome known since the last take, in their order.
    pub(crate) fn take_settled(&self) -> Vec<Settled> {
        std::mem::take(&mut self.lock().settled)
    }

    /// Whether a commit's outcome is not yet known.
    pub(crate) fn unsettled(&self) -> bool {
        self.lock().unsettled > 0
    }
}

/// A commit whose `COMMIT` was in flight when its writer's session was lost: its id, its change,
/// and when it began by the database's clock (its row's `at_unix`). The ledger purges rows older
/// than [`LEDGER_KEEP_SECS`], so a missing row of an older one says nothing.
#[derive(Debug)]
struct Unknown {
    id: String,
    change: Option<Owned>,
    begun: i64,
}

/// Why a write did not commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteFail {
    /// The server refused a statement or the commit: nothing of it is written, and the session
    /// stays.
    Refused(String),
    /// The session is lost, or was before the transaction's `COMMIT`: nothing of it is written.
    Lost(String),
    /// The session was lost while the `COMMIT` of transaction `id` was in flight: it may or
    /// may not have landed, which the revival settles.
    Unknown {
        /// The transaction's id.
        id: String,
        /// What the client said.
        why: String,
    },
}

impl WriteFail {
    /// The text of the failure.
    pub(crate) fn why(&self) -> &str {
        match self {
            WriteFail::Refused(why) | WriteFail::Lost(why) | WriteFail::Unknown { why, .. } => why,
        }
    }
}

/// A fault a test makes the writer meet at the commit of its next transaction.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The `COMMIT` lands, and the session ends before its answer is read: a commit of unknown
    /// outcome that landed.
    LoseAfterCommit,
}

/// The fault the next commit of any writer of one pool meets, which a test sets.
#[cfg(test)]
pub(crate) type FaultSlot = Arc<Mutex<Option<Fault>>>;

/// One writing session of the pool, owned by its thread.
pub(crate) struct Writer {
    config: postgres::Config,
    statements: Arc<AtomicU64>,
    session: Option<Session>,
    /// The commit of unknown outcome this writer lost its session with, until its revival
    /// settles it.
    unknown: Option<Unknown>,
    link: Arc<Link>,
    #[cfg(test)]
    pub(crate) fault: FaultSlot,
}

/// The writing session within one transaction.
pub(crate) struct Tx<'a> {
    session: &'a mut Session,
}

impl Tx<'_> {
    /// Runs a statement; the number of rows it changed.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn execute(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<u64, postgres::Error> {
        self.session.execute(sql, params)
    }

    /// Runs a statement that returns exactly one row.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn query_one(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<postgres::Row, postgres::Error> {
        self.session.query_one(sql, params)
    }

    /// Runs a statement that returns at most one row.
    ///
    /// # Errors
    /// The client's error, and when it returns more than one row.
    pub(crate) fn query_opt(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Option<postgres::Row>, postgres::Error> {
        self.session.query_opt(sql, params)
    }

    /// Runs a statement; its rows, as many as there are.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn query(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Vec<postgres::Row>, postgres::Error> {
        self.session.query(sql, params)
    }

    /// Runs statements that take no parameters and return nothing.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn batch(&mut self, sql: &str) -> Result<(), postgres::Error> {
        self.session.batch(sql)
    }

    /// The session itself, for a reader of a part's rows inside the transaction.
    pub(crate) fn session(&mut self) -> &mut Session {
        self.session
    }
}

/// Why the body of a transaction stopped before its commit.
#[derive(Debug)]
pub(crate) enum Halt {
    /// A statement failed: the session stays for a refusal, and is lost otherwise.
    Pg(postgres::Error),
    /// The body refused, for the reason given, after a statement the server refused or before
    /// any: the transaction is rolled back and the session stays.
    Refuse(String),
}

impl From<postgres::Error> for Halt {
    fn from(e: postgres::Error) -> Halt {
        Halt::Pg(e)
    }
}

/// Takes the coordinator's advisory lock on `session` shared, without waiting: false when a
/// session holds it exclusively (a release before this one, an import or an export).
///
/// # Errors
/// The client's error.
pub(crate) fn try_lock_shared(session: &mut Session) -> Result<bool, postgres::Error> {
    Ok(session
        .query_one("SELECT pg_try_advisory_lock_shared($1)", &[&LOCK_KEY])?
        .get(0))
}

/// Takes the coordinator's advisory lock on `session` exclusively, without waiting: false when
/// any session holds it, shared or not. What an import or an export takes, so none runs beside a
/// coordinator.
///
/// # Errors
/// The client's error.
pub(crate) fn try_lock(session: &mut Session) -> Result<bool, postgres::Error> {
    Ok(session
        .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])?
        .get(0))
}

impl Drop for Writer {
    /// Lets go of the lock before the session closes, so the lock is free once the writer is
    /// gone. Closing a session only sends the server its end and returns; the server ends the
    /// session, and frees the lock with it, a moment later, when a successor may already be
    /// trying the lock. The server answers the unlock once the lock is free, within the session's
    /// bounded waits. A transaction left open (a job that panicked in it) is rolled back first, so
    /// an aborted one does not refuse the unlock. A lost writer holds no session: nothing to do.
    fn drop(&mut self) {
        if let Some(session) = self.session.as_mut() {
            // A session that cannot run it is broken, and the server frees the lock as it
            // ends it.
            let _ = session.batch("ROLLBACK; SELECT pg_advisory_unlock_all()");
        }
    }
}

impl Writer {
    /// A writer over `session`, which holds the lock, reporting its state to `link`.
    pub(crate) fn new(
        config: postgres::Config,
        statements: Arc<AtomicU64>,
        session: Session,
        link: Arc<Link>,
    ) -> Writer {
        Writer {
            config,
            statements,
            session: Some(session),
            unknown: None,
            link,
            #[cfg(test)]
            fault: FaultSlot::default(),
        }
    }

    /// Drops the session as lost while the `COMMIT` of transaction `id`, begun at `begun` by the
    /// database's clock, was in flight: what a test makes a commit of unknown outcome of an id of
    /// its own with.
    #[cfg(test)]
    pub(crate) fn lose_unknown(&mut self, id: &str, begun: i64) {
        let unknown = Unknown {
            id: id.to_owned(),
            change: None,
            begun,
        };
        self.lose("a test lost the session", Some(unknown));
    }

    /// The session, while it is up.
    pub(crate) fn session(&mut self) -> Option<&mut Session> {
        self.session.as_mut()
    }

    /// Drops the session: lost, with the transaction `unknown` in flight if one was.
    fn lose(&mut self, why: &str, unknown: Option<Unknown>) {
        tracing::error!(
            alert = "store_unreachable",
            %why,
            "the database session is lost: nothing is written until it is back"
        );
        let held = self.session.take().is_some();
        self.link.lose(held, unknown.is_some());
        self.unknown = unknown;
    }

    /// The failure of a statement of a transaction: the session stays for a refusal (after a
    /// `ROLLBACK`), and is lost otherwise.
    fn failed(&mut self, e: &postgres::Error) -> WriteFail {
        let why = describe(e);
        if ends_session(e) {
            self.lose(&why, None);
            return WriteFail::Lost(why);
        }
        self.refused(why)
    }

    /// A transaction refused for `why`, its session still there: rolled back, and the session
    /// lost when even that fails.
    fn refused(&mut self, why: String) -> WriteFail {
        let rolled_back = self
            .session
            .as_mut()
            .map_or(Ok(()), |session| session.batch("ROLLBACK"));
        if let Err(e) = rolled_back {
            let why = describe(&e);
            self.lose(&why, None);
            return WriteFail::Lost(why);
        }
        WriteFail::Refused(why)
    }

    /// Runs `body` in one transaction, after inserting the transaction's row of `tx_ledger` (and
    /// deleting the rows older than [`LEDGER_KEEP_SECS`]), and commits it.
    ///
    /// # Errors
    /// [`WriteFail`]: nothing of it is written, or ([`WriteFail::Unknown`]) its outcome is
    /// not known until the session is back.
    pub(crate) fn transact<R>(
        &mut self,
        change: Option<Owned>,
        body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error>,
    ) -> Result<R, WriteFail> {
        self.transact_naming(|tx| Ok((body(tx)?, change)))
    }

    /// [`Writer::transact`] of a body that says what change it made, for a revival to hand back
    /// when the commit's outcome is not known, and that may refuse ([`Halt::Refuse`]) without
    /// losing the session.
    ///
    /// # Errors
    /// [`WriteFail`], as [`Writer::transact`]; a body's refusal is [`WriteFail::Refused`].
    pub(crate) fn transact_naming<R>(
        &mut self,
        body: impl FnOnce(&mut Tx<'_>) -> Result<(R, Option<Owned>), Halt>,
    ) -> Result<R, WriteFail> {
        let Some(session) = self.session.as_mut() else {
            return Err(WriteFail::Lost("the database session is lost".to_owned()));
        };
        let begun = session.batch("BEGIN").and_then(|()| {
            let row = session.query_one(
                "INSERT INTO tx_ledger (at_unix) VALUES (extract(epoch FROM now())::bigint) \
                 RETURNING id::text, at_unix",
                &[],
            )?;
            let id: String = row.get(0);
            let begun: i64 = row.get(1);
            session.execute(
                "DELETE FROM tx_ledger WHERE at_unix < extract(epoch FROM now())::bigint - $1",
                &[&LEDGER_KEEP_SECS],
            )?;
            Ok((id, begun))
        });
        let (id, begun) = match begun {
            Ok(begun) => begun,
            Err(e) => return Err(self.failed(&e)),
        };
        let mut tx = Tx {
            session: &mut *session,
        };
        let (answer, change) = match body(&mut tx) {
            Ok(answered) => answered,
            Err(Halt::Pg(e)) => return Err(self.failed(&e)),
            Err(Halt::Refuse(why)) => return Err(self.refused(why)),
        };
        #[cfg(test)]
        let fault = self
            .fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let committed = session.batch("COMMIT");
        #[cfg(test)]
        let committed = match (committed, fault) {
            (Ok(()), Some(Fault::LoseAfterCommit)) => {
                let _ = session.batch("SELECT pg_terminate_backend(pg_backend_pid())");
                session.batch("SELECT 1")
            }
            (committed, _) => committed,
        };
        match committed {
            Ok(()) => Ok(answer),
            Err(e) if ends_session(&e) => {
                let why = describe(&e);
                let unknown = Unknown {
                    id: id.clone(),
                    change,
                    begun,
                };
                self.lose(&why, Some(unknown));
                Err(WriteFail::Unknown { id, why })
            }
            // A deferred check refused the commit: the server rolled it back.
            Err(e) => Err(WriteFail::Refused(describe(&e))),
        }
    }

    /// Runs the revival on this writer (section 4): a session that is up is asked one statement,
    /// and kept when it answers; a session that is gone, or found gone so, is opened again, the
    /// lock taken again, and the commit of unknown outcome this writer lost it with settled, if it
    /// has one. A session that cannot be opened or locked leaves the store lost, for the next
    /// heartbeat to try again.
    pub(crate) fn revive(&mut self) {
        if let Some(session) = self.session.as_mut() {
            match session.batch("SELECT 1") {
                Ok(()) => {
                    self.link.revived(false, None);
                    return;
                }
                Err(e) => self.lose(&describe(&e), None),
            }
        }
        let unknown = self
            .unknown
            .as_ref()
            .map(|unknown| (unknown.id.clone(), unknown.begun));
        match self.reopen(unknown.as_ref().map(|(id, begun)| (id.as_str(), *begun))) {
            Ok(Revived::Up(session, landed)) => {
                let settled = self.unknown.take().map(|unknown| Settled {
                    id: unknown.id,
                    landed: landed == Some(true),
                    change: unknown.change,
                });
                self.session = Some(*session);
                self.link.revived(true, settled);
                tracing::warn!(alert = "store_unreachable", "the database session is back");
            }
            Ok(Revived::Held) => {
                tracing::error!(
                    alert = "store_unreachable",
                    "another session holds the coordinator's lock of the database exclusively"
                );
                self.link.revived(false, None);
            }
            Err(why) => {
                tracing::error!(
                    alert = "store_unreachable",
                    %why,
                    "the database session could not be opened again"
                );
                self.link.revived(false, None);
            }
        }
    }

    /// A new session holding the lock, and whether the transaction `unknown` landed: `None` when
    /// there is none to settle. The settle inserts the id again: no row inserted means the first
    /// insert committed (the insert waited for it while it was in progress), a row inserted means
    /// the transaction never committed and now cannot.
    fn reopen(&mut self, unknown: Option<(&str, i64)>) -> Result<Revived, String> {
        let mut session = Session::connect(&self.config, Arc::clone(&self.statements), false)
            .map_err(|fail| match fail {
                Fail::Pg(e) => describe(&e),
                Fail::Refused(why) => why,
            })?;
        if !try_lock_shared(&mut session).map_err(|e| describe(&e))? {
            return Ok(Revived::Held);
        }
        let landed = match unknown {
            None => None,
            Some((id, begun)) if too_old(&mut session, begun)? => {
                // The ledger may have purged the row of a commit that landed, so a missing row
                // proves nothing: only a row that is there is an answer.
                let found: i64 = session
                    .query_one(
                        "SELECT count(*) FROM tx_ledger WHERE id = $1::text::uuid",
                        &[&id],
                    )
                    .map_err(|e| describe(&e))?
                    .get(0);
                if found == 0 {
                    return Err(format!(
                        "the commit {id} began over {LEDGER_KEEP_SECS} seconds ago and its row \
                         of the ledger is gone: whether it landed cannot be told; restart this \
                         coordinator to read the database afresh"
                    ));
                }
                Some(true)
            }
            Some((id, _)) => {
                let inserted = session
                    .execute(
                        "INSERT INTO tx_ledger (id, at_unix) \
                         VALUES ($1::text::uuid, extract(epoch FROM now())::bigint) \
                         ON CONFLICT (id) DO NOTHING",
                        &[&id],
                    )
                    .map_err(|e| describe(&e))?;
                Some(inserted == 0)
            }
        };
        Ok(Revived::Up(Box::new(session), landed))
    }
}

/// Whether a transaction that began at `begun` is as old as the ledger's retention, by the
/// database's clock.
fn too_old(session: &mut Session, begun: i64) -> Result<bool, String> {
    let now: i64 = session
        .query_one("SELECT extract(epoch FROM now())::bigint", &[])
        .map_err(|e| describe(&e))?
        .get(0);
    Ok(now - begun >= LEDGER_KEEP_SECS)
}

enum Revived {
    /// The session is back; whether the transaction waiting to be settled landed.
    Up(Box<Session>, Option<bool>),
    Held,
}

#[cfg(test)]
mod link_tests {
    use super::*;

    /// Proves: a link of two writers is up until a session is lost; a revival is asked for once
    /// and not again while a writer has yet to run it; the store is up as soon as no session is
    /// lost, though a writer has yet to report; a revival that brought nothing back leaves it
    /// lost for the next one; and once a writer's thread is gone the store is stopped for good,
    /// asking for no revival, lost sessions or not.
    ///
    /// Catches: a second revival posted over one under way (two reopens of one session), a store
    /// that waits for every writer before it is up, and a stopped store that keeps reviving.
    #[test]
    fn the_link_counts_lost_sessions_and_revivals_of_the_pool() {
        let link = Link::new(2);
        assert_eq!(link.state(), State::Up);
        assert!(!link.start_revival(), "nothing is lost");
        link.lose(true, false);
        assert_eq!(link.state(), State::Lost);
        assert!(link.start_revival());
        assert_eq!(link.state(), State::Reviving);
        assert!(!link.start_revival(), "one revival at a time");
        link.revived(false, None);
        link.revived(false, None);
        assert_eq!(link.state(), State::Lost, "nothing came back");
        assert!(link.start_revival());
        link.revived(true, None);
        assert_eq!(
            link.state(),
            State::Up,
            "up before the other writer reports"
        );
        link.revived(false, None);

        link.lose(true, true);
        assert!(link.unsettled());
        link.stop();
        assert_eq!(link.state(), State::Stopped);
        assert!(!link.start_revival(), "a stopped store is not revived");
    }
}
