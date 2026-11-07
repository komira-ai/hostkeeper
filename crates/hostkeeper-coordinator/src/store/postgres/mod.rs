//! The Postgres store (`docs/design/coordinator-storage.md`, build item C3): the coordinator's
//! state in the tables of migration `0001`, behind the same [`Backend`] as the files and the fake,
//! and the enrollment store's records behind its `Records` port ([`PgRecords`]).
//!
//! - **The start** ([`Postgres::open`]): open the writing session, take the coordinator's
//!   advisory lock without waiting (another coordinator holding it refuses the start), run the
//!   migrations the database lacks, raise the epoch, and load everything. Nothing is read or
//!   written before the lock is held. A database that is not up is a refusal, told from one that
//!   refuses by SQLSTATE and I/O kind ([`DatabaseError`]).
//! - **Writes** go through the one writing session, on its own thread ([`writer`]), in the order
//!   the coordinator makes them. A commit compares the state before and after in memory and
//!   writes only the rows that differ ([`rows`], [`state`]), with its audit rows, in one
//!   transaction.
//! - **Reads** that are not decisions (`Audit.List`, the enrollment lists, the alerts' scans) run
//!   on a second, read-only session, so a long list never holds a commit. They see committed rows
//!   only.
//! - **A lost session**: every call is answered at once that it is lost ([`Health::Lost`]), and
//!   each heartbeat asks for a revival ([`Backend::revive`]), which the writer's thread makes. A
//!   commit of unknown outcome is settled at the revival by the lease's counter, and what it
//!   turned out to be is taken by the stores ([`Landing`]). Another epoch found deposes the store.

mod dsn;
mod generations;
pub(crate) mod migrate;
mod records;
pub(crate) mod rows;
pub(crate) mod session;
pub(crate) mod state;
pub(crate) mod transfer;
pub(crate) mod writer;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_oidc::Clock;

pub use dsn::DsnError;
pub(crate) use dsn::{Dsn, TIMEOUTS, Timeouts};
pub(crate) use records::PgRecords;

use self::rows::{Stmt, Tables, Val, changes, laid_over};
use self::session::{Fail, Session, Worker, describe, ends_session, not_up};
use self::writer::{Link, State, Tx, WriteFail, Writer, try_lock};
use super::{Backend, BackendError, Commit, Health, Loaded};
use crate::audit_file::{Actor, AuditLine, Detail, Logged, Query, UnixClock};
use crate::desired::{self, StoreError};
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
    /// Another session holds the coordinator's lock of the database.
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
    audit: Vec<AuditLine>,
}

impl Owned {
    fn of(change: &Commit<'_>) -> Owned {
        Owned {
            desired: change.desired.map(|(b, a)| (b.clone(), a.clone())),
            ops: change.ops.map(|(b, a)| (b.clone(), a.clone())),
            verdicts: change.verdicts.map(|(b, a)| (b.clone(), a.clone())),
            audit: change.audit.to_vec(),
        }
    }
}

/// What a commit of unknown outcome turned out to be, once the session was back: whether it
/// landed, and the change it was, for the stores to take into memory when it did (section 4.4).
#[derive(Clone, Debug)]
pub(crate) struct Landing {
    /// The transaction's number.
    pub(crate) number: u64,
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
}

struct Inner {
    writer: Worker<Writer>,
    reader: Worker<Reader>,
    link: Arc<Link>,
    #[cfg(test)]
    statements: Arc<AtomicU64>,
    last_seq: AtomicU64,
    epoch: u64,
    clock: Clock,
}

/// The Postgres store: a handle shared by the three stores, CI's spent ids and the enrollment
/// store's records. Its sessions close when the last handle is dropped.
#[derive(Clone)]
pub(crate) struct Postgres {
    inner: Arc<Inner>,
}

/// What a store refused because the database could not take the write, as a store that cannot
/// write says it: refused `store_failed`.
fn unwritten(why: &str) -> BackendError {
    BackendError::Ops(OpsError::Io(StoreError::Io {
        path: state::place("database"),
        source: io::Error::other(why.to_owned()),
    }))
}

/// The state the writing session `session` holds, the spent ids past at `now` left out.
fn load_all(session: &mut Session, now: u64) -> Result<Loaded, BackendError> {
    let read = |session: &mut Session, tables: &[rows::Table]| -> Result<Tables, BackendError> {
        let mut out = Tables::new();
        for table in tables {
            let rows = session
                .query(&table.select(), &[])
                .and_then(|rows| table.rows_of(&rows))
                .map_err(|e| unwritten(&describe(&e)))?;
            out.insert(table.name, rows);
        }
        Ok(out)
    };
    let desired = state::desired_of(&read(session, &state::DESIRED)?)?;
    let mut ops = state::ops_of(&read(session, &state::OPS)?)?;
    let verdicts = state::verdicts_of(&read(session, &state::VERDICTS)?)?;
    let ring = session
        .query(
            "SELECT seq, at_unix, by_name, action, service, node_id, reason FROM audit \
             ORDER BY seq DESC LIMIT $1",
            &[&(AUDIT_RING as i64)],
        )
        .map_err(|e| unwritten(&describe(&e)))?;
    ops.audit = ring.iter().rev().map(|row| logged_of(row).entry).collect();
    let now = state::int(now)?;
    let spent = session
        .query(
            "SELECT jti, until_unix FROM spent_tokens WHERE until_unix > $1 ORDER BY jti",
            &[&now],
        )
        .map_err(|e| unwritten(&describe(&e)))?
        .iter()
        .map(|row| {
            let until: i64 = row.get(1);
            (
                row.get::<_, String>(0),
                u64::try_from(until).unwrap_or_default(),
            )
        })
        .collect();
    Ok(Loaded {
        desired,
        ops,
        verdicts,
        spent,
    })
}

/// The audit line of a row of `audit`, its columns in their order.
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
            reason: row.get(6),
        },
    }
    .into_logged()
}

/// The statement that writes the audit line `line`.
fn audit_insert(line: &AuditLine) -> Result<Stmt, StoreError> {
    Ok(Stmt {
        sql: "INSERT INTO audit (seq, at_unix, by_name, action, service, node_id, reason) \
              VALUES ($1, $2, $3, $4, $5, $6, $7)"
            .to_owned(),
        params: vec![
            Val::Int(state::int(line.seq)?),
            Val::Int(state::int(line.at_unix)?),
            Val::Text(line.by.clone()),
            Val::Text(line.action.clone()),
            Val::text_or_null(line.detail.service.as_deref()),
            Val::text_or_null(line.detail.node_id.as_deref()),
            Val::text_or_null(line.detail.reason.as_deref()),
        ],
    })
}

/// The statements of `change`.
fn statements(change: &Commit<'_>) -> Result<Vec<Stmt>, BackendError> {
    let mut out = Vec::new();
    if let Some((before, after)) = change.desired {
        let (b, a) = (state::desired_rows(before)?, state::desired_rows(after)?);
        out.extend(changes(&state::DESIRED, &b, &a));
    }
    if let Some((before, after)) = change.ops {
        let (b, a) = (state::ops_rows(before)?, state::ops_rows(after)?);
        out.extend(changes(&state::OPS, &b, &a));
    }
    if let Some((before, after)) = change.verdicts {
        let (b, a) = (state::verdicts_rows(before)?, state::verdicts_rows(after)?);
        out.extend(changes(&state::VERDICTS, &b, &a));
    }
    for line in change.audit {
        out.push(audit_insert(line)?);
    }
    Ok(out)
}

/// Runs `statements` in `tx`, in order.
pub(crate) fn run_all(tx: &mut Tx<'_>, statements: &[Stmt]) -> Result<(), postgres::Error> {
    for statement in statements {
        tx.execute(&statement.sql, &statement.refs())?;
    }
    Ok(())
}

impl Postgres {
    /// Opens the store of `dsn` at start (section 5): the writing session, the lock, the
    /// migrations, the epoch raised and recorded with `holder` and the time of `unix`, and the
    /// state loaded, CI's spent ids past by `clock` left out. The read-only session is made when
    /// a read first needs it.
    ///
    /// # Errors
    /// [`DatabaseError`]: the database is not up, refused, holds the lock for another session,
    /// holds a newer schema, a migration failed, or the state does not load.
    pub(crate) fn open(
        dsn: &Dsn,
        holder: &str,
        unix: &UnixClock,
        clock: Clock,
    ) -> Result<(Postgres, Loaded), DatabaseError> {
        Postgres::open_with(dsn, TIMEOUTS, holder, unix, clock)
    }

    /// [`Postgres::open`] with the bounded waits `timeouts`.
    pub(crate) fn open_with(
        dsn: &Dsn,
        timeouts: Timeouts,
        holder: &str,
        unix: &UnixClock,
        clock: Clock,
    ) -> Result<(Postgres, Loaded), DatabaseError> {
        let config = dsn.config(timeouts);
        let statements = Arc::new(AtomicU64::new(0));
        let now_unix = state::int(unix()).map_err(|e| DatabaseError::Load(e.to_string()))?;
        let spent_now = clock();
        let holder = holder.to_owned();
        let opening = {
            let config = config.clone();
            let statements = Arc::clone(&statements);
            // The client must not run on a thread of the runtime the start runs in.
            std::thread::Builder::new()
                .name("hk-store-open".to_owned())
                .spawn(move || open_session(&config, statements, &holder, now_unix, spent_now))
                .map_err(|e| DatabaseError::Refused(e.to_string()))?
        };
        let (session, epoch, commits, loaded, last) = opening
            .join()
            .map_err(|_| DatabaseError::Load("the start of the store stopped".to_owned()))??;
        let link = Arc::new(Link::default());
        let writer = Writer::new(
            config.clone(),
            Arc::clone(&statements),
            session,
            (epoch, commits),
            Arc::clone(&link),
        );
        let spawned = |e: io::Error| DatabaseError::Refused(e.to_string());
        let inner = Inner {
            writer: Worker::spawn("hk-store-writer", writer).map_err(spawned)?,
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
            last_seq: AtomicU64::new(last),
            epoch: u64::try_from(epoch).unwrap_or_default(),
            clock,
        };
        tracing::info!(epoch, "the database's lock is held");
        Ok((
            Postgres {
                inner: Arc::new(inner),
            },
            loaded,
        ))
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
        if self.inner.link.state() != State::Up {
            return Err(WriteFail::Lost("the database session is lost".to_owned()));
        }
        let answer = self
            .inner
            .writer
            .call(move |writer| writer.transact(change, body));
        answer.unwrap_or_else(|| {
            // The writer's thread is gone: the store is lost for good, which the alert says.
            self.inner.link.lose(None);
            Err(WriteFail::Lost("the writing session stopped".to_owned()))
        })
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

    /// Whether the writing session is up with no commit's outcome unknown: when what the tables
    /// hold is what this coordinator committed.
    pub(crate) fn settled_up(&self) -> bool {
        self.inner.link.state() == State::Up && !self.inner.link.unsettled()
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

    /// Runs `job` on the writer's thread: what a test reaches the writing session through.
    #[cfg(test)]
    pub(crate) fn on_writer<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Writer) -> R + Send + 'static,
    ) -> Option<R> {
        self.inner.writer.call(job)
    }

    /// Stops the store's sessions, its lock released, though handles of it remain: what a test
    /// does before it starts a store again on the same database.
    #[cfg(test)]
    pub(crate) fn stop(&self) {
        self.inner.writer.stop();
        self.inner.reader.stop();
    }

    fn reserve(&self, lines: &[AuditLine]) {
        if let Some(last) = lines.last() {
            self.inner.last_seq.fetch_max(last.seq, Ordering::Relaxed);
        }
    }
}

/// Opens the writing session at start, on a thread of its own: the connect, the lock, the
/// migrations, the epoch, the load. Answers the session, its epoch, the lease's count of
/// transactions, the state, and the number of the newest audit row.
fn open_session(
    config: &postgres::Config,
    statements: Arc<AtomicU64>,
    holder: &str,
    now_unix: i64,
    spent_now: u64,
) -> Result<(Session, i64, i64, Loaded, u64), DatabaseError> {
    let mut session = Session::connect(config, statements, false)?;
    let refused = |e: postgres::Error| DatabaseError::Refused(describe(&e));
    if !try_lock(&mut session).map_err(refused)? {
        return Err(DatabaseError::Locked);
    }
    let set =
        migrate::checked(&migrate::SHIPPED).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    let ran =
        migrate::apply(&mut session, &set).map_err(|e| DatabaseError::Schema(e.to_string()))?;
    if !ran.is_empty() {
        tracing::info!(?ran, "the database's migrations ran");
    }
    session.batch("BEGIN").map_err(refused)?;
    let row = session
        .query_one(
            "UPDATE lease SET epoch = epoch + 1, holder = $1, since_unix = $2 RETURNING epoch, commits",
            &[&holder, &now_unix],
        )
        .map_err(refused)?;
    session.batch("COMMIT").map_err(refused)?;
    let (epoch, commits): (i64, i64) = (row.get(0), row.get(1));
    let loaded =
        load_all(&mut session, spent_now).map_err(|e| DatabaseError::Load(e.to_string()))?;
    let last: Option<i64> = session
        .query_one("SELECT max(seq) FROM audit", &[])
        .map_err(refused)?
        .get(0);
    let last = last.and_then(|n| u64::try_from(n).ok()).unwrap_or(0);
    Ok((session, epoch, commits, loaded, last))
}

impl Backend for Postgres {
    fn load(&self) -> Result<Loaded, BackendError> {
        let now = (self.inner.clock)();
        self.inner
            .writer
            .call(move |writer| match writer.session() {
                Some(session) => load_all(session, now),
                None => Err(unwritten("the database session is lost")),
            })
            .unwrap_or_else(|| Err(unwritten("the writing session stopped")))
    }

    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError> {
        let statements = statements(change)?;
        if statements.is_empty() {
            return Ok(());
        }
        match self.write_change(Some(Owned::of(change)), move |tx| run_all(tx, &statements)) {
            Ok(()) => {
                self.reserve(change.audit);
                Ok(())
            }
            Err(WriteFail::Unknown { why, .. }) => {
                // Its numbers stay taken, whether or not the revival finds it landed.
                self.reserve(change.audit);
                Err(unwritten(&why))
            }
            Err(fail) => Err(unwritten(fail.why())),
        }
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
            Val::Int(i64::try_from(query.limit.max(1)).unwrap_or(i64::MAX)),
        ];
        let order = if query.newest { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT seq, at_unix, by_name, action, service, node_id, reason FROM audit \
             WHERE seq > $1 \
             AND ($2::text IS NULL OR CASE $2::text WHEN 'operator' \
                  THEN by_name NOT IN ('coordinator', 'ci') ELSE by_name = $2::text END) \
             AND ($3::text IS NULL OR by_name = $3::text) \
             AND (cardinality($4::text[]) = 0 OR action = ANY($4::text[])) \
             AND ($5::text IS NULL OR service = $5::text) \
             AND ($6::text IS NULL OR node_id = $6::text) \
             ORDER BY seq {order} LIMIT $7"
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
                            "SELECT seq, at_unix, by_name, action, service, node_id, reason \
                             FROM audit WHERE seq > $1 ORDER BY seq LIMIT 1024",
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
            State::Lost | State::Reviving => Health::Lost,
            State::Deposed { found } => Health::Deposed { found },
        }
    }

    fn revive(&self) {
        if self.inner.link.start_revival() {
            self.inner.writer.post(Writer::revive);
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
                    number = settled.number,
                    landed = settled.landed,
                    "a commit whose session was lost while it was in flight is settled"
                );
                Landing {
                    number: settled.number,
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
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The writing session goes first, and the lock with it.
        self.writer.stop();
        self.reader.stop();
    }
}

#[cfg(test)]
#[path = "tests/mod.rs"]
pub(crate) mod tests;
