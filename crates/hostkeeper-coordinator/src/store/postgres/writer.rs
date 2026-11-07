//! The writing session (sections 4.4 and 5): the one session that holds the coordinator's
//! advisory lock of the database, on its own thread, through which every write goes, in the
//! order the coordinator makes them.
//!
//! Every transaction moves `lease.commits` on by one, on the condition that `lease.epoch` is
//! still the coordinator's own, before anything else it writes; the database refuses that update
//! on a session that does not hold the lock (the trigger `lease_fenced` of `0001`). So the
//! number of a transaction is known before its `COMMIT`, and a session lost while its `COMMIT`
//! was in flight is settled at the reconnect by reading the counter: equal to the number, it
//! landed; one less, it did not.
//!
//! A session that a write finds gone, or whose bounded wait ran out, is **lost**: the writer drops
//! it, and from then on every call is answered at once that it is lost ([`Link`]), until a
//! revival ([`Writer::revive`], asked for at each heartbeat and run on the writer's thread, never
//! under the caller's locks) opens a new session and takes the lock again. With the lock it reads
//! the lease: the same epoch, and the coordinator goes on; another, and another coordinator led
//! meanwhile: the store is **deposed**, and the coordinator exits.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::Owned;
use super::session::{Fail, Params, Session, describe, ends_session};

/// The key of the coordinator's advisory lock: a constant that never changes, which the trigger
/// `lease_fenced` of `0001` names by its two halves.
pub(crate) const LOCK_KEY: i64 = 0x686b_6370_6731;

/// The state of the writing session, as every caller sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// The session holds the lock and takes writes.
    Up,
    /// The session is lost; nothing is written until it is revived.
    Lost,
    /// A new session is being opened, on the writer's thread.
    Reviving,
    /// Another coordinator raised the epoch while the session was lost: this one's memory is
    /// stale, and it exits.
    Deposed {
        /// The epoch found.
        found: u64,
    },
}

/// What a commit of unknown outcome turned out to be, once the session is back.
#[derive(Clone, Debug)]
pub(crate) struct Settled {
    /// The number of the transaction (`lease.commits`).
    pub(crate) number: u64,
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
    unknown: Option<(u64, Option<Owned>)>,
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

    /// Marks the session lost, unless the store is deposed already.
    pub(crate) fn lose(&self, unknown: Option<(u64, Option<Owned>)>) {
        let mut shared = self.lock();
        if !matches!(shared.state, State::Deposed { .. }) {
            shared.state = State::Lost;
        }
        if unknown.is_some() {
            shared.unknown = unknown;
        }
    }

    /// Marks a lost session as being revived: true when it was lost, so the caller hands the
    /// revival to the writer; false when it is up, deposed or being revived already.
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
    /// The session was lost while the `COMMIT` of transaction `number` was in flight: it may or
    /// may not have landed, which the revival settles.
    Unknown {
        /// The transaction's number.
        number: u64,
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

/// The writing session and what it keeps: the epoch it holds, the count of transactions it
/// knows committed.
pub(crate) struct Writer {
    config: postgres::Config,
    statements: Arc<AtomicU64>,
    session: Option<Session>,
    epoch: i64,
    commits: i64,
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

    /// Runs a statement; its rows.
    ///
    /// # Errors
    /// The client's error.
    #[cfg(test)]
    pub(crate) fn query(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Vec<postgres::Row>, postgres::Error> {
        self.session.query(sql, params)
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
}

/// Takes the coordinator's advisory lock on `session`, without waiting: false when another
/// session holds it.
///
/// # Errors
/// The client's error.
pub(crate) fn try_lock(session: &mut Session) -> Result<bool, postgres::Error> {
    Ok(session
        .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])?
        .get(0))
}

impl Writer {
    /// A writer over `session`, which holds the lock, at `epoch` with `commits` transactions
    /// committed, reporting its state to `link`.
    pub(crate) fn new(
        config: postgres::Config,
        statements: Arc<AtomicU64>,
        session: Session,
        (epoch, commits): (i64, i64),
        link: Arc<Link>,
    ) -> Writer {
        Writer {
            config,
            statements,
            session: Some(session),
            epoch,
            commits,
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
    fn lose(&mut self, why: &str, unknown: Option<(u64, Option<Owned>)>) {
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
        if let Some(session) = self.session.as_mut()
            && let Err(e) = session.batch("ROLLBACK")
        {
            let why = describe(&e);
            self.lose(&why, None);
            return WriteFail::Lost(why);
        }
        WriteFail::Refused(why)
    }

    /// Runs `body` in one transaction, after moving `lease.commits` on by one on the condition
    /// that the epoch is still this coordinator's, and commits it.
    ///
    /// # Errors
    /// [`WriteFail`]: nothing of it is written, or ([`WriteFail::Unknown`]) its outcome is
    /// not known until the session is back.
    pub(crate) fn transact<R>(
        &mut self,
        change: Option<Owned>,
        body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error>,
    ) -> Result<R, WriteFail> {
        let epoch = self.epoch;
        let Some(session) = self.session.as_mut() else {
            return Err(WriteFail::Lost("the database session is lost".to_owned()));
        };
        let begun = session.batch("BEGIN").and_then(|()| {
            session.query_opt(
                "UPDATE lease SET commits = commits + 1 WHERE epoch = $1 RETURNING commits",
                &[&epoch],
            )
        });
        let number = match begun {
            Ok(Some(row)) => row.get::<_, i64>(0),
            Ok(None) => {
                // Another coordinator raised the epoch: only possible after this one lost the
                // lock, which a session that holds it has not.
                let why = "the lease's epoch is no longer this coordinator's".to_owned();
                let _ = session.batch("ROLLBACK");
                self.lose(&why, None);
                return Err(WriteFail::Lost(why));
            }
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
            Ok(()) => {
                self.commits = number;
                Ok(answer)
            }
            Err(e) if ends_session(&e) => {
                let why = describe(&e);
                let number = u64::try_from(number).unwrap_or_default();
                self.lose(&why, Some((number, change)));
                Err(WriteFail::Unknown { number, why })
            }
            // A deferred check refused the commit: the server rolled it back.
            Err(e) => Err(WriteFail::Refused(describe(&e))),
        }
    }

    /// Opens a new session, takes the lock again and reads the lease (section 5): with the same
    /// epoch the session is up again, and a commit of unknown outcome is settled; with another,
    /// the store is deposed. A session that cannot be opened or locked leaves the store lost,
    /// for the next heartbeat to try again.
    pub(crate) fn revive(&mut self) {
        match self.reopen() {
            Ok(Revived::Up(session, commits)) => {
                let session = *session;
                {
                    let mut shared = self.link.lock();
                    if let Some((number, change)) = shared.unknown.take() {
                        let landed = u64::try_from(commits).ok() == Some(number);
                        shared.settled.push(Settled {
                            number,
                            landed,
                            change,
                        });
                    }
                    shared.state = State::Up;
                }
                self.session = Some(session);
                self.commits = commits;
                tracing::warn!(
                    alert = "store_unreachable",
                    "the database session is back, on the same epoch"
                );
            }
            Ok(Revived::Deposed(found)) => {
                tracing::error!(
                    alert = "store_unreachable",
                    epoch = self.epoch,
                    found,
                    "another coordinator led the database while this one's session was lost"
                );
                self.link.set(State::Deposed { found });
            }
            Ok(Revived::Held) => {
                tracing::error!(
                    alert = "store_unreachable",
                    "another session holds the coordinator's lock of the database"
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

    fn reopen(&mut self) -> Result<Revived, String> {
        let mut session = Session::connect(&self.config, Arc::clone(&self.statements), false)
            .map_err(|fail| match fail {
                Fail::Pg(e) => describe(&e),
                Fail::Refused(why) => why,
            })?;
        if !try_lock(&mut session).map_err(|e| describe(&e))? {
            return Ok(Revived::Held);
        }
        let row = session
            .query_one("SELECT epoch, commits FROM lease", &[])
            .map_err(|e| describe(&e))?;
        let (epoch, commits): (i64, i64) = (row.get(0), row.get(1));
        if epoch != self.epoch {
            return Ok(Revived::Deposed(u64::try_from(epoch).unwrap_or_default()));
        }
        Ok(Revived::Up(Box::new(session), commits))
    }

    /// The epoch this coordinator holds.
    #[cfg(test)]
    pub(crate) fn epoch(&self) -> i64 {
        self.epoch
    }

    /// The transactions known committed.
    #[cfg(test)]
    pub(crate) fn commits(&self) -> i64 {
        self.commits
    }
}

enum Revived {
    Up(Box<Session>, i64),
    Deposed(u64),
    Held,
}
