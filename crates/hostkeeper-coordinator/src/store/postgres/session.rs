//! One session of the database and the thread that owns it.
//!
//! The blocking `postgres` client runs a runtime of its own for every call, and panics when it
//! is called from a thread inside the coordinator's tokio runtime (section 4.4). So a session
//! never leaves the thread of its [`Worker`]: a caller hands the worker a job and waits for the
//! answer, as it waits for an `fsync` of a file.
//!
//! A session is checked when it connects: the server must make a commit durable before it
//! acknowledges it (`fsync`, `synchronous_commit` and `full_page_writes` not `off`), and every
//! table name resolves through `search_path` to the store's schema.

use std::error::Error as _;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use postgres::error::{Severity, SqlState};
use postgres::types::ToSql;
use postgres::{Client, NoTls, Row};

/// The schema the store's tables live in.
pub(crate) const SCHEMA: &str = "hostkeeper";

/// The parameters of a statement.
pub(crate) type Params<'a> = &'a [&'a (dyn ToSql + Sync)];

/// Why a session could not be made.
#[derive(Debug)]
pub(crate) enum Fail {
    /// The client's error: the server or the connection.
    Pg(postgres::Error),
    /// The server answered, with what the store refuses (a setting, a schema).
    Refused(String),
}

impl From<postgres::Error> for Fail {
    fn from(e: postgres::Error) -> Fail {
        Fail::Pg(e)
    }
}

/// The text of a client error: the server's severity, message and SQLSTATE, or the client's own
/// error and its cause. It never holds the connection string.
pub(crate) fn describe(e: &postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!(
            "{}: {} (SQLSTATE {})",
            db.severity(),
            db.message(),
            db.code().code()
        ),
        None => match e.source() {
            Some(cause) => format!("{e}: {cause}"),
            None => e.to_string(),
        },
    }
}

/// Whether `e` ended the session, or must be taken as having ended it: the connection is
/// closed or broke (no answer of the server), the server ended the session (`FATAL`, `PANIC`),
/// or a bounded wait ran out (a statement or lock timeout, a cancel). A plain refusal of a
/// statement (`ERROR`: a constraint, a trigger) keeps the session.
pub(crate) fn ends_session(e: &postgres::Error) -> bool {
    if e.is_closed() {
        return true;
    }
    let Some(db) = e.as_db_error() else {
        return true;
    };
    let fatal = matches!(
        db.parsed_severity(),
        Some(Severity::Fatal | Severity::Panic) | None
    );
    let waited = [
        SqlState::QUERY_CANCELED,
        SqlState::LOCK_NOT_AVAILABLE,
        SqlState::IDLE_IN_TRANSACTION_SESSION_TIMEOUT,
        SqlState::ADMIN_SHUTDOWN,
        SqlState::CRASH_SHUTDOWN,
    ]
    .contains(db.code());
    fatal || waited
}

/// Whether a failure to connect is the server not being up yet, by the client's typed errors
/// (SQLSTATE and I/O kind), never by its message: nothing listens, the connection was reset,
/// a Unix socket not made yet, a connect that timed out, or the server starting up (`57P03`).
pub(crate) fn not_up(e: &postgres::Error) -> bool {
    if let Some(code) = e.code() {
        return *code == SqlState::CANNOT_CONNECT_NOW;
    }
    let mut cause = e.source();
    while let Some(c) = cause {
        if let Some(io) = c.downcast_ref::<io::Error>() {
            return matches!(
                io.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::TimedOut
            );
        }
        cause = c.source();
    }
    false
}

/// A session: a client connected and checked, and the count of statements the store has run,
/// shared by every session of one store.
pub(crate) struct Session {
    client: Client,
    statements: Arc<AtomicU64>,
}

impl Session {
    /// Connects by `config`, refuses a server that does not make commits durable, and sets the
    /// search path to the store's schema; with `read_only`, every transaction is read-only.
    ///
    /// # Errors
    /// [`Fail`]: the connect failed, or the server's settings are refused.
    pub(crate) fn connect(
        config: &postgres::Config,
        statements: Arc<AtomicU64>,
        read_only: bool,
    ) -> Result<Session, Fail> {
        let client = config.connect(NoTls)?;
        let mut session = Session { client, statements };
        session.require_durable()?;
        session.batch(&format!(
            "SET search_path TO {SCHEMA}; SET default_transaction_isolation = 'read committed'; \
             SET default_transaction_read_only = {}",
            if read_only { "on" } else { "off" }
        ))?;
        Ok(session)
    }

    /// Refuses a server where a commit may be acknowledged before it is on disk.
    fn require_durable(&mut self) -> Result<(), Fail> {
        for setting in ["fsync", "synchronous_commit", "full_page_writes"] {
            let row = self.query_one(&format!("SHOW {setting}"), &[])?;
            let value: String = row.get(0);
            if value == "off" {
                return Err(Fail::Refused(format!(
                    "the database has {setting} off; the coordinator acknowledges what is committed, so a commit must be on disk when it returns"
                )));
            }
        }
        Ok(())
    }

    fn count(&self) {
        self.statements.fetch_add(1, Ordering::Relaxed);
    }

    /// Runs statements that take no parameters.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn batch(&mut self, sql: &str) -> Result<(), postgres::Error> {
        self.count();
        self.client.batch_execute(sql)
    }

    /// Runs a statement; the number of rows it changed.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn execute(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<u64, postgres::Error> {
        self.count();
        self.client.execute(sql, params)
    }

    /// Runs a statement; its rows.
    ///
    /// # Errors
    /// The client's error.
    pub(crate) fn query(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Vec<Row>, postgres::Error> {
        self.count();
        self.client.query(sql, params)
    }

    /// Runs a statement that returns exactly one row.
    ///
    /// # Errors
    /// The client's error, and when it returns no row or more than one.
    pub(crate) fn query_one(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Row, postgres::Error> {
        self.count();
        self.client.query_one(sql, params)
    }

    /// Runs a statement that returns at most one row.
    ///
    /// # Errors
    /// The client's error, and when it returns more than one row.
    pub(crate) fn query_opt(
        &mut self,
        sql: &str,
        params: Params<'_>,
    ) -> Result<Option<Row>, postgres::Error> {
        self.count();
        self.client.query_opt(sql, params)
    }

    /// The server's process id of this session: what a test ends it by.
    ///
    /// # Errors
    /// The client's error.
    #[cfg(test)]
    pub(crate) fn backend_pid(&mut self) -> Result<i32, postgres::Error> {
        Ok(self.query_one("SELECT pg_backend_pid()", &[])?.get(0))
    }
}

/// A job a worker runs on its thread.
type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

/// A thread that owns a state `S` (a session among it) and runs the jobs handed to it, one at
/// a time, in the order they came.
pub(crate) struct Worker<S> {
    jobs: Mutex<Option<mpsc::Sender<Job<S>>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl<S: Send + 'static> Worker<S> {
    /// A worker named `name` that owns `state`.
    ///
    /// # Errors
    /// The thread could not be made.
    pub(crate) fn spawn(name: &str, mut state: S) -> io::Result<Worker<S>> {
        let (jobs, inbox) = mpsc::channel::<Job<S>>();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                for job in inbox {
                    job(&mut state);
                }
            })?;
        Ok(Worker {
            jobs: Mutex::new(Some(jobs)),
            thread: Mutex::new(Some(thread)),
        })
    }

    fn sender(&self) -> Option<mpsc::Sender<Job<S>>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Runs `job` on the worker's thread and waits for its answer; `None` when the thread is
    /// gone (a job of it panicked, or the worker is stopped).
    pub(crate) fn call<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Option<R> {
        let (answer, answered) = mpsc::sync_channel(1);
        let sent = self.sender()?.send(Box::new(move |state: &mut S| {
            // The caller may be gone; the job ran all the same.
            let _ = answer.send(job(state));
        }));
        sent.ok()?;
        answered.recv().ok()
    }

    /// Hands `job` to the worker's thread without waiting for it. A worker that is gone drops it.
    pub(crate) fn post(&self, job: impl FnOnce(&mut S) + Send + 'static) {
        if let Some(jobs) = self.sender() {
            // A thread that is gone has nothing to run it on.
            let _ = jobs.send(Box::new(job));
        }
    }

    /// Stops the thread once the jobs handed to it have run, and waits for it: its state, and
    /// the session among it, are dropped by then.
    pub(crate) fn stop(&self) {
        drop(
            self.jobs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            // A panic of a job was the job's caller's to see.
            let _ = thread.join();
        }
    }
}

impl<S> Drop for Worker<S> {
    fn drop(&mut self) {
        drop(
            self.jobs
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}
