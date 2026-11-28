//! The Postgres store (`docs/design/coordinator-storage.md`, build item C3): the coordinator's
//! state in the tables of migration `0001`, behind the same [`Backend`] as the files and the fake,
//! and the enrollment store's records behind its `Records` port ([`PgRecords`]). The tables of
//! migrations `0005`, `0011` and `0012` (the MDM's) are no longer read or written by any code: they
//! stay in place, unused, because a release before this one holds them and a format changes only
//! by addition.
//!
//! - **The start** ([`Postgres::open`]): open a writing session, take the coordinator's
//!   advisory lock shared without waiting (any number of coordinators hold it together; a session
//!   holding it exclusively, a release before this one or an import, refuses the start), run the
//!   migrations the database lacks ([`migrate`]), raise the epoch and make this instance's row of
//!   `instances` ([`instance`]), load everything, then open the rest of the pool's sessions, each
//!   holding the lock shared too. Nothing is read or written before the lock is held. A database
//!   that is not up is a refusal, told from one that refuses by SQLSTATE and I/O kind
//!   ([`DatabaseError`]).
//! - **Writes** go through the pool of writing sessions (`--database-pool`, [`pool`]), each on its
//!   own thread ([`writer`]): a write takes a free session. A commit compares the state before and
//!   after in memory and writes only the rows that differ ([`rows`], [`state`]), with its audit
//!   rows, in one transaction; its caller holds its store's lock while it waits for it, so the
//!   commits of one store land in the order the coordinator makes them.
//! - **Reads** that are not decisions (`Audit.List`, the enrollment lists, the alerts' scans) run
//!   on a second, read-only session, so a long list never holds a commit. They see committed rows
//!   only.
//! - **A lost session**: every call is answered at once that it is lost ([`Health::Lost`]), and
//!   each heartbeat asks for a revival ([`Backend::revive`]), which the writer's thread makes. A
//!   commit of unknown outcome is settled at the revival by its row of `tx_ledger`, and what it
//!   turned out to be is taken by the stores ([`Landing`]).

mod accounts;
pub(crate) mod admin_rows;
pub(crate) mod alert_rows;
mod at;
pub(crate) mod decide;
mod dsn;
pub(crate) mod edit;
mod fleet_rows;
mod generations;
mod groups;
pub(crate) mod hardware_rows;
mod instance;
mod layout;
pub(crate) mod login_rows;
mod maintenance_rows;
pub(crate) mod migrate;
pub(crate) mod parts;
pub(crate) mod periodic;
mod pool;
mod queue_seq;
mod records;
mod releases;
mod repositories;
pub(crate) mod rows;
pub(crate) mod session;
pub(crate) mod ssh;
pub(crate) mod ssh_book;
pub(crate) mod state;
pub(crate) mod transfer;
pub(crate) mod writer;
pub(crate) mod xcode_rows;

use std::io;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use hostkeeper_oidc::Clock;

pub use dsn::DsnError;
pub(crate) use dsn::{Dsn, TIMEOUTS, Timeouts};
pub(crate) use edit::Open;
pub(crate) use fleet_rows::Timing;
pub(crate) use records::PgRecords;

use self::instance::Instance;
use self::migrate::Retry;
use self::parts::Known;
use self::pool::Pool;
use self::rows::{Stmt, Val, laid_over};
use self::session::{Fail, Session, Worker, describe, ends_session, not_up};
use self::writer::{Link, State, Tx, WriteFail, Writer, try_lock_shared};
use super::beat::HostBeat;
use super::decision::StageClaim;
use super::{Backend, BackendError, Commit, Edit, Fresh, Health, Loaded, Part, Scope};
use crate::accounts::Accounts;
use crate::audit_file::{Actor, AuditLine, Detail, Logged, Query, UnixClock};
use crate::desired::{self, StoreError};
use crate::layout::Layout;
use crate::ops::{AUDIT_RING, OpsError, OpsState};
use crate::verdict_file;

/// Why the store could not be opened at start. The text names neither the connection string
/// nor the file it is in.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    /// The connection string is refused.
    #[error(transparent)]
    Dsn(#[from] DsnError),
    /// The server is not up yet: nothing listens, or it is starting up. Its supervisor starts
    /// the coordinator again.
    #[error("the database is not up: {0}")]
    NotUp(String),
    /// The server is up and refused: a login, a database, a setting, any other answer.
    #[error("the database refused the coordinator: {0}")]
    Refused(String),
    /// A session holds the coordinator's lock of the database exclusively: a release before
    /// this one, an import or an export.
    #[error("another coordinator holds the lock of this database")]
    Locked,
    /// The migrations could not be brought up to date.
    #[error("{0}")]
    Schema(String),
    /// The state the database holds could not be read.
    #[error("{0}")]
    Load(String),
}

impl From<Fail> for DatabaseError {
    fn from(fail: Fail) -> DatabaseError {
        match fail {
            Fail::Pg(e) if not_up(&e) => DatabaseError::NotUp(describe(&e)),
            Fail::Pg(e) => DatabaseError::Refused(describe(&e)),
            Fail::Refused(why) => DatabaseError::Refused(why),
        }
    }
}

/// The read-only session, made when a read first needs it and again after it is lost.
struct Reader {
    config: postgres::Config,
    statements: Arc<AtomicU64>,
    session: Option<Session>,
}

impl Reader {
    fn run<R>(
        &mut self,
        read: impl FnOnce(&mut Session) -> Result<R, postgres::Error>,
    ) -> Result<R, String> {
        let session = match self.session.take() {
            Some(session) => session,
            None => Session::connect(&self.config, Arc::clone(&self.statements), true).map_err(
                |fail| match fail {
                    Fail::Pg(e) => describe(&e),
                    Fail::Refused(why) => why,
                },
            )?,
        };
        let session = self.session.insert(session);
        read(session).map_err(|e| {
            if ends_session(&e) {
                self.session = None;
            }
            describe(&e)
        })
    }
}

/// A commit whose outcome was not known when its session was lost: its parts, owned.
#[derive(Clone, Debug, Default)]
pub(crate) struct Owned {
    desired: Option<(desired::Held, desired::Held)>,
    ops: Option<(OpsState, OpsState)>,
    verdicts: Option<(verdict_file::Held, verdict_file::Held)>,
    accounts: Option<(Accounts, Accounts)>,
    layout: Option<(Layout, Layout)>,
    audit: Vec<AuditLine>,
}

impl Owned {
    fn of(change: &Commit<'_>) -> Owned {
        Owned {
            desired: change.desired.map(|(b, a)| (b.clone(), a.clone())),
            ops: change.ops.map(|(b, a)| (b.clone(), a.clone())),
            verdicts: change.verdicts.map(|(b, a)| (b.clone(), a.clone())),
            accounts: change.accounts.map(|(b, a)| (b.clone(), a.clone())),
            layout: change.layout.map(|(b, a)| (b.clone(), a.clone())),
            audit: change.audit.to_vec(),
        }
    }
}

/// What a commit of unknown outcome turned out to be, once the session was back: whether it
/// landed, and the change it was, for the stores to take into memory when it did (section 4.4).
#[derive(Clone, Debug)]
pub(crate) struct Landing {
    /// The transaction's id (its row of `tx_ledger`).
    pub(crate) id: String,
    /// Whether it landed.
    pub(crate) landed: bool,
    change: Option<Owned>,
}

impl Landing {
    fn landed(&self) -> Option<&Owned> {
        self.change.as_ref().filter(|_| self.landed)
    }

    /// The audit lines of the change that landed: none when it did not.
    pub(crate) fn lines(&self) -> &[AuditLine] {
        self.landed().map_or(&[], |c| c.audit.as_slice())
    }

    /// `memory` with the desired state of the change laid over it, when it landed and had one.
    pub(crate) fn desired_over(&self, memory: &desired::Held) -> Option<desired::Held> {
        let (before, after) = self.landed()?.desired.as_ref()?;
        let rows = |held| state::desired_rows(held).ok();
        let over = laid_over(&rows(memory)?, &rows(before)?, &rows(after)?);
        state::desired_of(&over).ok()
    }

    /// `memory` with the operations state of the change laid over it, its audit entries in the
    /// ring, when it landed and had one.
    pub(crate) fn ops_over(&self, memory: &OpsState) -> Option<OpsState> {
        let change = self.landed()?;
        let (before, after) = change.ops.as_ref()?;
        let rows = |state| state::ops_rows(state).ok();
        let over = laid_over(&rows(memory)?, &rows(before)?, &rows(after)?);
        let mut taken = state::ops_of(&over).ok()?;
        let mut ring = memory.audit.clone();
        for line in &change.audit {
            if !ring.iter().any(|entry| entry.seq == line.seq) {
                ring.push(line.clone().into_logged().entry);
            }
        }
        ring.sort_by_key(|entry| entry.seq);
        ring.drain(..ring.len().saturating_sub(AUDIT_RING));
        taken.audit = ring;
        Some(taken)
    }

    /// `memory` with the verdicts of the change laid over it, when it landed and had them.
    pub(crate) fn verdicts_over(&self, memory: &verdict_file::Held) -> Option<verdict_file::Held> {
        let (before, after) = self.landed()?.verdicts.as_ref()?;
        let rows = |held| state::verdicts_rows(held).ok();
        let over = laid_over(&rows(memory)?, &rows(before)?, &rows(after)?);
        state::verdicts_of(&over).ok()
    }

    /// `memory` with the accounts of the change laid over it, when it landed and had them.
    pub(crate) fn accounts_over(&self, memory: &Accounts) -> Option<Accounts> {
        let (before, after) = self.landed()?.accounts.as_ref()?;
        let over = laid_over(
            &accounts::rows(memory),
            &accounts::rows(before),
            &accounts::rows(after),
        );
        accounts::of_tables(&over).ok()
    }

    /// `memory` with the layout of the change laid over it, when it landed and had one.
    pub(crate) fn layout_over(&self, memory: &Layout) -> Option<Layout> {
        let (before, after) = self.landed()?.layout.as_ref()?;
        let over = laid_over(
            &layout::rows(memory),
            &layout::rows(before),
            &layout::rows(after),
        );
        layout::of_tables(&over).ok()
    }
}

/// What a start of the store is given beside its connection string
/// (`docs/design/coordinator-multi-instance.md`, section 12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Options {
    /// How many writing sessions the instance holds (`--database-pool`).
    pub(crate) pool: NonZeroUsize,
    /// How a migration waits for its locks, and how often it is tried again.
    pub(crate) migrate: Retry,
    /// How often this instance's row of `instances` is upserted (`--instance-beat-secs`).
    pub(crate) beat: Duration,
    /// The release this instance runs, for its row.
    pub(crate) release: String,
    /// The addresses this instance listens on, for its row.
    pub(crate) listen: String,
    /// How long an edit waits for the rollout gate and its parts' rows
    /// (`--operator-gate-wait-ms`); a heartbeat's never waits.
    pub(crate) gate_wait: Duration,
    /// How old the stored beat of a host must be before a beat older by the host's clock is
    /// believed (`--beat-reorder-secs`), and the quiet time a gap in the beats sets
    /// (`--quiet-secs`) ([`fleet_rows`]).
    pub(crate) fleet: fleet_rows::Timing,
}

struct Inner {
    writers: Arc<Pool>,
    reader: Worker<Reader>,
    link: Arc<Link>,
    #[cfg(test)]
    statements: Arc<AtomicU64>,
    #[cfg(test)]
    fault: writer::FaultSlot,
    last_seq: AtomicU64,
    epoch: u64,
    clock: Clock,
    instance: Instance,
    /// The version of each part this instance's copy of it carries.
    known: Arc<Known>,
    /// How long an edit waits for its locks (`--operator-gate-wait-ms`).
    gate_wait: Duration,
    /// The times the beat statement of the fleet's rows reads ([`fleet_rows`]).
    fleet: fleet_rows::Timing,
    /// The heartbeat's decision a thread holds open, which that thread's edits join.
    deciding: decide::Seat,
    /// What a test runs between the read of a part's version and the read of its rows.
    #[cfg(test)]
    between: Mutex<Option<parts::Between>>,
    /// The thread that upserts the instance's row, and the sender whose drop stops it.
    beat: Mutex<Option<(mpsc::Sender<()>, std::thread::JoinHandle<()>)>>,
}

/// The Postgres store: a handle shared by the three stores, CI's spent ids and the enrollment
/// store's records. Its sessions close when the last handle is dropped.
#[derive(Clone)]
pub(crate) struct Postgres {
    inner: Arc<Inner>,
}

/// What a write is refused with while a writing session is lost.
const LOST: &str = "the database session is lost";

/// What a write is refused with once a writer's thread is gone.
const STOPPED: &str = "the writing session stopped";

/// What a store refused because the database could not take the write, as a store that cannot
/// write says it: refused `store_failed`.
fn unwritten(why: &str) -> BackendError {
    BackendError::Ops(OpsError::Io(StoreError::Io {
        path: state::place("database"),
        source: io::Error::other(why.to_owned()),
    }))
}

/// The audit line of a row of `audit`, its columns in their order (`host_group`, of migration
/// `0008`, then `api_token`, of `0027`).
fn logged_of(row: &postgres::Row) -> Logged {
    let unsigned = |i: usize| u64::try_from(row.get::<_, i64>(i)).unwrap_or_default();
    AuditLine {
        seq: unsigned(0),
        at_unix: unsigned(1),
        by: row.get(2),
        action: row.get(3),
        detail: Detail {
            service: row.get(4),
            node_id: row.get(5),
            host_group: row.get(7),
            reason: row.get(6),
            subject: None,
            source: None,
            api_token: row.get(8),
        },
    }
    .into_logged()
}

/// The statement that writes the audit line `line`.
fn audit_insert(line: &AuditLine) -> Result<Stmt, StoreError> {
    Ok(Stmt {
        sql: "INSERT INTO audit \
              (seq, at_unix, by_name, action, service, node_id, reason, host_group, api_token) \
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
            .to_owned(),
        params: vec![
            Val::Int(state::int(line.seq)?),
            Val::Int(state::int(line.at_unix)?),
            Val::Text(line.by.clone()),
            Val::Text(line.action.clone()),
            Val::text_or_null(line.detail.service.as_deref()),
            Val::text_or_null(line.detail.node_id.as_deref()),
            Val::text_or_null(line.detail.reason.as_deref()),
            Val::text_or_null(line.detail.host_group.as_deref()),
            Val::text_or_null(line.detail.api_token.as_deref()),
        ],
    })
}

/// Runs `statements` in `tx`, in order.
pub(crate) fn run_all(tx: &mut Tx<'_>, statements: &[Stmt]) -> Result<(), postgres::Error> {
    for statement in statements {
        tx.execute(&statement.sql, &statement.refs())?;
    }
    Ok(())
}

impl Postgres {
    /// Opens the store of `dsn` at start (section 5) with `options`: the writing sessions, the
    /// lock, the migrations, the epoch raised and recorded with `holder` and the time of `unix`,
    /// this instance's row, and the state loaded, CI's spent ids past by `clock` left out. The
    /// read-only session is made when a read first needs it.
    ///
    /// # Errors
    /// [`DatabaseError`]: the database is not up, refused, holds the lock for another session,
    /// holds a newer schema, a migration failed, or the state does not load.
    pub(crate) fn open(
        dsn: &Dsn,
        options: &Options,
        holder: &str,
        unix: &UnixClock,
        clock: Clock,
    ) -> Result<(Postgres, Loaded), DatabaseError> {
        Postgres::open_with(dsn, TIMEOUTS, options, holder, unix, clock)
    }

    /// [`Postgres::open`] with the bounded waits `timeouts`.
    pub(crate) fn open_with(
        dsn: &Dsn,
        timeouts: Timeouts,
        options: &Options,
        holder: &str,
        unix: &UnixClock,
        clock: Clock,
    ) -> Result<(Postgres, Loaded), DatabaseError> {
        let config = dsn.config(timeouts);
        let statements = Arc::new(AtomicU64::new(0));
        let now_unix = state::int(unix()).map_err(|e| DatabaseError::Load(e.to_string()))?;
        let spent_now = clock();
        let opening = {
            let config = config.clone();
            let statements = Arc::clone(&statements);
            let (options, holder) = (options.clone(), holder.to_owned());
            // The client must not run on a thread of the runtime the start runs in.
            std::thread::Builder::new()
                .name("hk-store-open".to_owned())
                .spawn(move || {
                    open_sessions(&config, statements, &options, &holder, now_unix, spent_now)
                })
                .map_err(|e| DatabaseError::Refused(e.to_string()))?
        };
        let Opened {
            sessions,
            epoch,
            loaded,
            versions,
            last,
            instance,
        } = opening
            .join()
            .map_err(|_| DatabaseError::Load("the start of the store stopped".to_owned()))??;
        let link = Arc::new(Link::new(sessions.len()));
        #[cfg(test)]
        let fault = writer::FaultSlot::default();
        let writers = sessions
            .into_iter()
            .map(|session| {
                #[cfg_attr(not(test), expect(unused_mut, reason = "a test's fault is set on it"))]
                let mut writer = Writer::new(
                    config.clone(),
                    Arc::clone(&statements),
                    session,
                    Arc::clone(&link),
                );
                #[cfg(test)]
                {
                    writer.fault = Arc::clone(&fault);
                }
                writer
            })
            .collect();
        let spawned = |e: io::Error| DatabaseError::Refused(e.to_string());
        let writers = Arc::new(Pool::spawn("hk-store-writer", writers).map_err(spawned)?);
        let (beat_stop, beat_stopped) = mpsc::channel();
        let beating = {
            let (writers, link, instance) =
                (Arc::clone(&writers), Arc::clone(&link), instance.clone());
            let every = options.beat;
            // The thread holds the pool, not the store, so the store's last handle is never its
            // own and a drop of the store has stopped and joined it before the sessions close.
            std::thread::Builder::new()
                .name("hk-store-instance".to_owned())
                .spawn(move || {
                    instance::beat_every(&beat_stopped, every, || {
                        // A beat that is not written is the store's own alert (a lost
                        // session), and the next beat writes the row.
                        let _ = instance::beat(&writers, &link, &instance);
                    });
                })
                .map_err(spawned)?
        };
        let inner = Inner {
            writers,
            reader: Worker::spawn(
                "hk-store-reader",
                Reader {
                    config,
                    statements: Arc::clone(&statements),
                    session: None,
                },
            )
            .map_err(spawned)?,
            link,
            #[cfg(test)]
            statements,
            #[cfg(test)]
            fault,
            last_seq: AtomicU64::new(last),
            epoch: u64::try_from(epoch).unwrap_or_default(),
            clock,
            instance,
            known: Arc::new(Known::of(versions)),
            deciding: decide::Seat::default(),
            gate_wait: options.gate_wait,
            fleet: options.fleet,
            #[cfg(test)]
            between: Mutex::new(None),
            beat: Mutex::new(Some((beat_stop, beating))),
        };
        let inner = Arc::new(inner);
        tracing::info!(
            epoch,
            sessions = options.pool.get(),
            instance = %inner.instance.id,
            "the database's lock is held, shared"
        );
        Ok((Postgres { inner }, loaded))
    }

    /// Runs `body` in one transaction of the writing session; at once [`WriteFail::Lost`] while
    /// the session is lost.
    ///
    /// # Errors
    /// [`WriteFail`]: it is not written, or its outcome is not known.
    pub(crate) fn write<R: Send + 'static>(
        &self,
        body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, WriteFail> {
        self.write_change(None, body)
    }

    /// [`Postgres::write`] of the change `change`, which a revival hands back with its outcome
    /// when its commit's outcome is not known ([`Landing`]).
    fn write_change<R: Send + 'static>(
        &self,
        change: Option<Owned>,
        body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, WriteFail> {
        write_on(&self.inner.writers, &self.inner.link, change, body)
    }

    /// Runs `read` on the read-only session.
    ///
    /// # Errors
    /// What the session or the server said.
    pub(crate) fn read<R: Send + 'static>(
        &self,
        read: impl FnOnce(&mut Session) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, String> {
        self.inner
            .reader
            .call(move |reader| reader.run(read))
            .unwrap_or_else(|| Err("the reading session stopped".to_owned()))
    }

    /// Whether a commit's outcome is not yet known.
    #[cfg(test)]
    pub(crate) fn unsettled(&self) -> bool {
        self.inner.link.unsettled()
    }

    /// How many statements the store's sessions have run.
    #[cfg(test)]
    pub(crate) fn statements(&self) -> u64 {
        self.inner.statements.load(Ordering::Relaxed)
    }

    /// Runs `job` on a writer's thread, the one a write would take: what a test reaches a
    /// writing session through.
    #[cfg(test)]
    pub(crate) fn on_writer<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Writer) -> R + Send + 'static,
    ) -> Option<R> {
        self.inner.writers.call(job)
    }

    /// Waits for the jobs handed to every writer so far, a revival among them: `None` when a
    /// writer's thread is gone.
    #[cfg(test)]
    pub(crate) fn settle(&self) -> Option<()> {
        self.inner.writers.call_each(|_| ()).map(drop)
    }

    /// How many writes have waited for a free writing session.
    #[cfg(test)]
    pub(crate) fn waited(&self) -> usize {
        self.inner.writers.waited()
    }

    /// Makes the next commit of any writer meet `fault`.
    #[cfg(test)]
    pub(crate) fn fault(&self, fault: writer::Fault) {
        *self
            .inner
            .fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(fault);
    }

    /// Stops the store's sessions, its lock released, and the upserts of its instance's row,
    /// though handles of it remain: what a test does before it starts a store again on the same
    /// database.
    #[cfg(test)]
    pub(crate) fn stop(&self) {
        self.inner.stop_beat();
        self.inner.writers.stop();
        self.inner.reader.stop();
    }

    fn reserve(&self, lines: &[AuditLine]) {
        if let Some(last) = lines.last() {
            self.inner.last_seq.fetch_max(last.seq, Ordering::Relaxed);
        }
    }

    /// Runs `between` between the version and the rows of the next reload of a part: what a test
    /// commits a change of the part in.
    #[cfg(test)]
    pub(crate) fn between_reads(&self, between: impl FnOnce() + Send + 'static) {
        *self
            .inner
            .between
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(between));
    }

    /// What the next reload runs between its two reads: nothing, unless a test set it.
    fn between(&self) -> parts::Between {
        #[cfg(test)]
        if let Some(between) = self
            .inner
            .between
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return between;
        }
        Box::new(|| {})
    }
}

/// Runs `body` in one transaction on a free session of `pool`, the change `change` handed back by
/// a revival when its commit's outcome is not known; at once [`WriteFail::Lost`] while `link`
/// says a session is lost or a writer's thread is gone.
fn write_on<R: Send + 'static>(
    pool: &Pool,
    link: &Link,
    change: Option<Owned>,
    body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error> + Send + 'static,
) -> Result<R, WriteFail> {
    on_writer(pool, link, move |writer| writer.transact(change, body))
}

/// Runs `job` on a writer of `pool`, unless `link` says the store is lost: what [`write_on`] runs
/// a transaction with, and a heartbeat its row's one statement ([`fleet_rows`]).
///
/// # Errors
/// [`WriteFail`]: the job's, or [`WriteFail::Lost`] at once while the store is lost.
fn on_writer<R: Send + 'static>(
    pool: &Pool,
    link: &Link,
    job: impl FnOnce(&mut Writer) -> Result<R, WriteFail> + Send + 'static,
) -> Result<R, WriteFail> {
    match link.state() {
        State::Up => {}
        State::Stopped => return Err(WriteFail::Lost(STOPPED.to_owned())),
        State::Lost | State::Reviving => return Err(WriteFail::Lost(LOST.to_owned())),
    }
    pool.call(job).unwrap_or_else(|| {
        // The writer's thread is gone: the store is lost for good, which the alert says.
        link.stop();
        Err(WriteFail::Lost(STOPPED.to_owned()))
    })
}

/// What [`open_sessions`] opened.
struct Opened {
    /// The writing sessions, each holding the lock shared.
    sessions: Vec<Session>,
    epoch: i64,
    loaded: Loaded,
    /// The version of each part, read with `loaded` in one snapshot.
    versions: std::collections::BTreeMap<Part, i64>,
    /// The number of the newest audit row.
    last: u64,
    instance: Instance,
}

/// Opens the writing sessions at start, on a thread of its own: the connect, the shared lock, the
/// migrations, the epoch and this instance's row, the load, then the rest of the pool's sessions,
/// each with the shared lock.
fn open_sessions(
    config: &postgres::Config,
    statements: Arc<AtomicU64>,
    options: &Options,
    holder: &str,
    now_unix: i64,
    spent_now: u64,
) -> Result<Opened, DatabaseError> {
    let refused = |e: postgres::Error| DatabaseError::Refused(describe(&e));
    let locked = |statements: &Arc<AtomicU64>| -> Result<Session, DatabaseError> {
        let mut session = Session::connect(config, Arc::clone(statements), false)?;
        if !try_lock_shared(&mut session).map_err(refused)? {
            return Err(DatabaseError::Locked);
        }
        Ok(session)
    };
    let mut session = locked(&statements)?;
    let set =
        migrate::checked(&migrate::SHIPPED).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    let ran = migrate::apply_with(&mut session, &set, options.migrate, &mut std::thread::sleep)
        .map_err(|e| DatabaseError::Schema(e.to_string()))?;
    if !ran.is_empty() {
        tracing::info!(?ran, "the database's migrations ran");
    }
    session.batch("BEGIN").map_err(refused)?;
    let row = session
        .query_one(
            "UPDATE lease SET epoch = epoch + 1, holder = $1, since_unix = $2 RETURNING epoch",
            &[&holder, &now_unix],
        )
        .map_err(refused)?;
    let instance =
        instance::start(&mut session, &options.release, &options.listen).map_err(refused)?;
    session.batch("COMMIT").map_err(refused)?;
    queue_seq::restamp(&mut session).map_err(refused)?;
    let epoch: i64 = row.get(0);
    let (loaded, versions) =
        parts::load(&mut session, spent_now).map_err(|e| DatabaseError::Load(e.to_string()))?;
    // The counter set past the newest line before anything is served: a release before this one
    // numbered its lines from `max(seq)` without it (`docs/design/coordinator-multi-instance.md`,
    // section 6). Idempotent, never lowered, and serialized with every writer of lines by the row.
    session
        .execute(
            "UPDATE audit_counter SET next = \
             greatest(next, (SELECT coalesce(max(seq), 0) + 1 FROM audit))",
            &[],
        )
        .map_err(refused)?;
    let last: Option<i64> = session
        .query_one("SELECT max(seq) FROM audit", &[])
        .map_err(refused)?
        .get(0);
    let last = last.and_then(|n| u64::try_from(n).ok()).unwrap_or(0);
    // The pool's sessions, and one more for heartbeats' decisions ([`pool::Pool`]).
    let mut sessions = vec![session];
    for _ in 0..options.pool.get() {
        sessions.push(locked(&statements)?);
    }
    Ok(Opened {
        sessions,
        epoch,
        loaded,
        versions,
        last,
        instance,
    })
}

impl Backend for Postgres {
    fn load(&self) -> Result<Loaded, BackendError> {
        match self.inner.link.state() {
            State::Up => {}
            State::Stopped => return Err(unwritten(STOPPED)),
            State::Lost | State::Reviving => return Err(unwritten(LOST)),
        }
        let now = (self.inner.clock)();
        self.inner
            .writers
            .call(move |writer| match writer.session() {
                Some(session) => parts::load(session, now).map(|(loaded, _)| loaded),
                None => Err(unwritten(LOST)),
            })
            .unwrap_or_else(|| {
                self.inner.link.stop();
                Err(unwritten(STOPPED))
            })
    }

    /// A commit made without an edit: in an edit of every part ([`Scope::WHOLE`]), so its rows
    /// move their parts' versions on as any edit's do.
    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError> {
        if edit::plan(change, Scope::WHOLE)?.0.is_empty() {
            return Ok(());
        }
        self.begin(Scope::WHOLE)?.commit_or(change, |_| Ok(()))
    }

    fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, BackendError> {
        let filter = &query.filter;
        let actor = filter.actor.map(|actor| match actor {
            Actor::Operator => "operator",
            Actor::Ci => "ci",
            Actor::Coordinator => "coordinator",
        });
        let params: Vec<Val> = vec![
            Val::Int(state::int(query.since_seq)?),
            Val::text_or_null(actor),
            Val::text_or_null(filter.by_name),
            Val::Texts(filter.actions.iter().map(|a| (*a).to_owned()).collect()),
            Val::text_or_null(filter.service),
            Val::text_or_null(filter.node_id),
            Val::text_or_null(filter.host_group),
            Val::Int(i64::try_from(query.limit.max(1)).unwrap_or(i64::MAX)),
        ];
        let order = if query.newest { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT seq, at_unix, by_name, action, service, node_id, reason, host_group, api_token \
             FROM audit WHERE seq > $1 \
             AND ($2::text IS NULL OR CASE $2::text WHEN 'operator' \
                  THEN by_name NOT IN ('coordinator', 'ci') ELSE by_name = $2::text END) \
             AND ($3::text IS NULL OR by_name = $3::text) \
             AND (cardinality($4::text[]) = 0 OR action = ANY($4::text[])) \
             AND ($5::text IS NULL OR service = $5::text) \
             AND ($6::text IS NULL OR node_id = $6::text) \
             AND ($7::text IS NULL OR host_group = $7::text) \
             ORDER BY seq {order} LIMIT $8"
        );
        let newest = query.newest;
        let mut page = self
            .read(move |session| {
                let refs: Vec<&(dyn postgres::types::ToSql + Sync)> = params
                    .iter()
                    .map(|p| p as &(dyn postgres::types::ToSql + Sync))
                    .collect();
                Ok(session
                    .query(&sql, &refs)?
                    .iter()
                    .map(logged_of)
                    .collect::<Vec<_>>())
            })
            .map_err(|why| unwritten(&why))?;
        if newest {
            page.reverse();
        }
        Ok(page)
    }

    fn audit_each(&self, visit: &mut dyn FnMut(Logged)) -> Result<(), BackendError> {
        let mut after = 0_i64;
        loop {
            let page = self
                .read(move |session| {
                    Ok(session
                        .query(
                            "SELECT seq, at_unix, by_name, action, service, node_id, reason, \
                             host_group, api_token FROM audit WHERE seq > $1 ORDER BY seq LIMIT 1024",
                            &[&after],
                        )?
                        .iter()
                        .map(logged_of)
                        .collect::<Vec<_>>())
                })
                .map_err(|why| unwritten(&why))?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            after = state::int(last.entry.seq)?;
            page.into_iter().for_each(&mut *visit);
        }
    }

    fn last_seq(&self) -> u64 {
        self.inner.last_seq.load(Ordering::Relaxed)
    }

    fn audit_pruned_before(&self) -> Result<Option<u64>, BackendError> {
        let pruned = self
            .read(|session| {
                Ok(session
                    .query_opt(
                        "SELECT audit_pruned_before FROM imports ORDER BY id DESC LIMIT 1",
                        &[],
                    )?
                    .and_then(|row| row.get::<_, Option<i64>>(0)))
            })
            .map_err(|why| unwritten(&why))?;
        Ok(pruned.and_then(|n| u64::try_from(n).ok()))
    }

    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError> {
        let (jti, until, now) = (
            jti.to_owned(),
            state::int(until_unix)?,
            state::int(now_unix)?,
        );
        let recorded = self
            .write(move |tx| {
                tx.execute("DELETE FROM spent_tokens WHERE until_unix <= $1", &[&now])?;
                tx.execute(
                    "INSERT INTO spent_tokens (jti, until_unix) VALUES ($1, $2) \
                     ON CONFLICT (jti) DO NOTHING",
                    &[&jti, &until],
                )
            })
            .map_err(|fail| unwritten(fail.why()))?;
        if recorded == 0 {
            return Err(BackendError::Spent);
        }
        Ok(())
    }

    fn health(&self) -> Health {
        match self.inner.link.state() {
            State::Up => Health::Up,
            State::Lost | State::Reviving | State::Stopped => Health::Lost,
        }
    }

    fn revive(&self) {
        if self.inner.link.start_revival() && !self.inner.writers.post_each(Writer::revive) {
            // A writer whose thread is gone never runs the revival: the store is lost for good.
            self.inner.link.stop();
        }
    }

    fn take_landings(&self) -> Vec<Landing> {
        self.inner
            .link
            .take_settled()
            .into_iter()
            .map(|settled| {
                tracing::warn!(
                    alert = "store_unreachable",
                    id = %settled.id,
                    landed = settled.landed,
                    "a commit whose session was lost while it was in flight is settled"
                );
                Landing {
                    id: settled.id,
                    landed: settled.landed,
                    change: settled.change,
                }
            })
            .collect()
    }

    fn epoch(&self) -> Option<u64> {
        Some(self.inner.epoch)
    }

    /// Migration `0003`'s columns of `desired` keep them.
    fn keeps_write_records(&self) -> bool {
        true
    }

    fn begin(&self, scope: Scope) -> Result<Edit, BackendError> {
        edit::begin(self, scope)
    }

    fn stale(&self) -> Vec<Part> {
        self.stale_parts()
    }

    fn keep_beat(&self, beat: HostBeat) {
        fleet_rows::keep(self, beat);
    }

    fn reload(&self, part: Part) -> Result<Fresh, BackendError> {
        self.reload_part(part)
    }

    fn decide(&self) -> Result<super::Opened, BackendError> {
        decide::open(self)
    }

    fn claim_stage(&self, claim: &StageClaim<'_>) -> Result<bool, BackendError> {
        decide::claim(self, claim)
    }
}

impl Inner {
    /// Stops the thread that upserts the instance's row, and waits for it.
    fn stop_beat(&self) {
        let beat = self
            .beat
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((stop, thread)) = beat {
            drop(stop);
            // A panic of the thread was its own; it holds nothing the store needs.
            let _ = thread.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop_beat();
        // The writing sessions go first, their lock freed before they close ([`Writer`]'s drop).
        self.writers.stop();
        self.reader.stop();
    }
}

#[cfg(test)]
#[path = "tests/mod.rs"]
pub(crate) mod tests;
