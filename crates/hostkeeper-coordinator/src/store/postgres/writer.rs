//! The writing session (sections 4.4 and 5): the one session of an instance through which every
//! write goes, on its own thread, in the order the instance makes them. It holds the
//! coordinator's advisory lock of the database in **shared** mode: any number of coordinators hold
//! it together, and a release before this one, which takes it exclusively, is refused while any
//! of them runs (`docs/design/coordinator-multi-instance.md`, section 4).
//!
//! Every transaction inserts a row of `tx_ledger` as its first statement and so has an id before
//! its `COMMIT`. A session lost while its `COMMIT` was in flight is settled at the reconnect by
//! inserting the same id again, doing nothing when it exists: the insert waits for the first
//! transaction while it is in progress, so the database says whether it landed (no row inserted:
//! it did; one inserted: it did not, and now never will).
//!
//! A session that a write finds gone, or whose bounded wait ran out, is **lost**: the writer drops
//! it, and from then on every call is answered at once that it is lost ([`Link`]), until a
//! revival ([`Writer::revive`], asked for at each heartbeat and run on the writer's thread, never
//! under the caller's locks) opens a new session and takes the lock again. Another coordinator
//! having started meanwhile is normal: there is no leader and nothing to be deposed by.
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

/// The state of the writing session, as every caller sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// The session holds the lock and takes writes.
    Up,
    /// The session is lost; nothing is written until it is revived.
    Lost,
    /// A new session is being opened, on the writer's thread.
    Reviving,
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
    state: State,
    /// The transaction whose `COMMIT` was in flight when the session was lost, and its change:
    /// recorded in the same step as the loss, so no revival can settle it without its change.
    unknown: Option<(String, Option<Owned>)>,
    /// When that transaction began, by the database's clock (its row's `at_unix`), when known: the
    /// ledger purges rows older than [`LEDGER_KEEP_SECS`], so a missing row of an older one says
    /// nothing.
    unknown_begun: Option<i64>,
    /// What the revivals found, in their order, until they are taken.
    settled: Vec<Settled>,
}

/// The state of the writing session, shared by the writer and its callers.
#[derive(Debug)]
pub(crate) struct Link {
    shared: Mutex<Shared>,
}

impl Default for Link {
    fn default() -> Self {
        Link {
            shared: Mutex::new(Shared {
                state: State::Up,
                unknown: None,
                unknown_begun: None,
                settled: Vec::new(),
            }),
        }
    }
}

impl Link {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The session's state now.
    pub(crate) fn state(&self) -> State {
        self.lock().state
    }

    fn set(&self, state: State) {
        self.lock().state = state;
    }

    /// Marks the session lost.
    pub(crate) fn lose(&self, unknown: Option<(String, Option<Owned>)>) {
        let mut shared = self.lock();
        shared.state = State::Lost;
        if unknown.is_some() {
            shared.unknown = unknown;
        }
    }

    /// Records when the transaction of unknown outcome began (the database's clock, in seconds).
    pub(crate) fn begun(&self, at_unix: i64) {
        self.lock().unknown_begun = Some(at_unix);
    }

    /// Marks a lost session as being revived: true when it was lost, so the caller hands the
    /// revival to the writer; false when it is up or being revived already.
    pub(crate) fn start_revival(&self) -> bool {
        let mut shared = self.lock();
        if shared.state == State::Lost {
            shared.state = State::Reviving;
            return true;
        }
        false
    }

    /// The outcomes of the commits of unknown outcome known since the last take, in their order.
    pub(crate) fn take_settled(&self) -> Vec<Settled> {
        std::mem::take(&mut self.lock().settled)
    }

    /// Whether a commit's outcome is not yet known.
    pub(crate) fn unsettled(&self) -> bool {
        self.lock().unknown.is_some()
    }
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

/// The writing session.
pub(crate) struct Writer {
    config: postgres::Config,
    statements: Arc<AtomicU64>,
    session: Option<Session>,
    link: Arc<Link>,
    #[cfg(test)]
    pub(crate) fault: Option<Fault>,
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
            link,
            #[cfg(test)]
            fault: None,
        }
    }

    /// The session, while it is up.
    pub(crate) fn session(&mut self) -> Option<&mut Session> {
        self.session.as_mut()
    }

    /// Drops the session: lost, with the transaction `unknown` in flight if one was.
    fn lose(&mut self, why: &str, unknown: Option<(String, Option<Owned>)>) {
        tracing::error!(
            alert = "store_unreachable",
            %why,
            "the database session is lost: nothing is written until it is back"
        );
        self.session = None;
        self.link.lose(unknown);
    }

    /// The failure of a statement of a transaction: the session stays for a refusal (after a
    /// `ROLLBACK`), and is lost otherwise.
    fn failed(&mut self, e: &postgres::Error) -> WriteFail {
        let why = describe(e);
        if ends_session(e) {
            self.lose(&why, None);
            return WriteFail::Lost(why);
        }
        // The session is there: a statement of it failed.
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
        let answer = match body(&mut tx) {
            Ok(answer) => answer,
            Err(e) => return Err(self.failed(&e)),
        };
        #[cfg(test)]
        let fault = self.fault.take();
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
                self.lose(&why, Some((id.clone(), change)));
                self.link.begun(begun);
                Err(WriteFail::Unknown { id, why })
            }
            // A deferred check refused the commit: the server rolled it back.
            Err(e) => Err(WriteFail::Refused(describe(&e))),
        }
    }

    /// Opens a new session, takes the lock again and settles the commit of unknown outcome, if
    /// one is waiting (section 4): a session that cannot be opened or locked leaves the store lost,
    /// for the next heartbeat to try again.
    pub(crate) fn revive(&mut self) {
        let (unknown, begun) = {
            let shared = self.link.lock();
            (
                shared.unknown.as_ref().map(|(id, _)| id.clone()),
                shared.unknown_begun,
            )
        };
        match self.reopen(unknown.as_deref(), begun) {
            Ok(Revived::Up(session, landed)) => {
                let session = *session;
                {
                    let mut shared = self.link.lock();
                    shared.unknown_begun = None;
                    if let Some((id, change)) = shared.unknown.take() {
                        shared.settled.push(Settled {
                            id,
                            landed: landed == Some(true),
                            change,
                        });
                    }
                    shared.state = State::Up;
                }
                self.session = Some(session);
                tracing::warn!(alert = "store_unreachable", "the database session is back");
            }
            Ok(Revived::Held) => {
                tracing::error!(
                    alert = "store_unreachable",
                    "another session holds the coordinator's lock of the database exclusively"
                );
                self.link.set(State::Lost);
            }
            Err(why) => {
                tracing::error!(
                    alert = "store_unreachable",
                    %why,
                    "the database session could not be opened again"
                );
                self.link.set(State::Lost);
            }
        }
    }

    /// A new session holding the lock, and whether the transaction `unknown` landed: `None` when
    /// there is none to settle. The settle inserts the id again: no row inserted means the first
    /// insert committed (the insert waited for it while it was in progress), a row inserted means
    /// the transaction never committed and now cannot.
    fn reopen(&mut self, unknown: Option<&str>, begun: Option<i64>) -> Result<Revived, String> {
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
            Some(id) if too_old(&mut session, begun)? => {
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
            Some(id) => {
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
fn too_old(session: &mut Session, begun: Option<i64>) -> Result<bool, String> {
    let Some(begun) = begun else {
        return Ok(false);
    };
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
