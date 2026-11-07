//! The tests of the Postgres store that need a server, and what they share: a database of each
//! test's own on the server `HOSTKEEPER_TEST_POSTGRES_URL` names.
//!
//! A test that needs the server fails when the variable is not set, saying how to start one; it
//! never skips. Each test makes its own database with a unique name and drops it, so tests running
//! in parallel share nothing.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};

use postgres::{Client, NoTls};
use tracing_subscriber::fmt::MakeWriter;

use super::{Dsn, Postgres, TIMEOUTS, Timeouts};
use crate::audit_file::UnixClock;
use crate::store::Loaded;

/// The URL of the test server ([`hostkeeper_testkit::postgres_url`]).
pub(crate) fn server_url() -> String {
    hostkeeper_testkit::postgres_url()
}

/// The key of the advisory lock the tests take around a `CREATE DATABASE` or `DROP DATABASE`,
/// which the server does not take side by side.
const CATALOG_KEY: i64 = 0x686b_7465_7374;

/// Runs `f` on a thread of its own: the blocking client must not run inside a test's runtime.
pub(crate) fn off_runtime<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    match std::thread::spawn(f).join() {
        Ok(r) => r,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// A client of the server's own database, for what a test does beside the store.
pub(crate) fn admin(dbname: Option<&str>) -> Client {
    let mut config: postgres::Config = server_url().parse().expect("the test server's URL parses");
    if let Some(name) = dbname {
        config.dbname(name);
    }
    config.connect(NoTls).expect("the test server answers")
}

/// Runs `sql` on the server's own database under the catalog lock.
fn catalog(sql: String) {
    off_runtime(move || {
        let mut client = admin(None);
        for statement in [
            format!("SET lock_timeout = '300s'; SELECT pg_advisory_lock({CATALOG_KEY})"),
            sql,
            format!("SELECT pg_advisory_unlock({CATALOG_KEY})"),
        ] {
            client
                .batch_execute(&statement)
                .expect("the catalog change runs");
        }
    });
}

/// A database of one test's own, dropped with it.
pub(crate) struct TestDatabase {
    name: String,
}

impl TestDatabase {
    /// A new, empty database.
    pub(crate) fn new() -> TestDatabase {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut random = [0_u8; 4];
        getrandom::fill(&mut random).expect("random bytes");
        let name = format!(
            "hk_t_{}_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            u32::from_le_bytes(random)
        );
        catalog(format!("CREATE DATABASE \"{name}\""));
        TestDatabase { name }
    }

    /// Its name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// A connection string for it, in keyword form, as the file of `--database-dsn-file` holds it.
    pub(crate) fn dsn_text(&self) -> String {
        let config: postgres::Config = server_url().parse().expect("the test server's URL parses");
        let host = match config.get_hosts().first() {
            Some(postgres::config::Host::Tcp(host)) => host.clone(),
            Some(postgres::config::Host::Unix(path)) => path.display().to_string(),
            None => "127.0.0.1".to_owned(),
        };
        let port = config.get_ports().first().copied().unwrap_or(5432);
        let mut text = format!(
            "host={host} port={port} dbname={} sslmode=disable",
            self.name
        );
        if let Some(user) = config.get_user() {
            text.push_str(&format!(" user={user}"));
        }
        if let Some(password) = config.get_password() {
            let password = String::from_utf8_lossy(password)
                .replace('\\', "\\\\")
                .replace('\'', "\\'");
            text.push_str(&format!(" password='{password}'"));
        }
        text
    }

    /// Its connection string, read and checked as a start reads it.
    pub(crate) fn dsn(&self) -> Dsn {
        Dsn::parse(&self.dsn_text()).expect("the test database's connection string")
    }

    /// A client of it, beside the store.
    pub(crate) fn client(&self) -> Client {
        let name = self.name.clone();
        off_runtime(move || admin(Some(&name)))
    }

    /// Runs `sql` on it, beside the store, with the search path of the store's schema.
    pub(crate) fn run(&self, sql: &str) -> Result<(), postgres::Error> {
        let (name, sql) = (self.name.clone(), sql.to_owned());
        off_runtime(move || {
            admin(Some(&name)).batch_execute(&format!("SET search_path TO hostkeeper; {sql}"))
        })
    }

    /// The rows `sql` answers, each as text, beside the store.
    pub(crate) fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        let (name, sql) = (self.name.clone(), sql.to_owned());
        off_runtime(move || {
            let mut client = admin(Some(&name));
            client
                .batch_execute("SET search_path TO hostkeeper")
                .expect("the search path");
            client
                .simple_query(&sql)
                .expect("the query runs")
                .into_iter()
                .filter_map(|message| match message {
                    postgres::SimpleQueryMessage::Row(row) => Some(
                        (0..row.len())
                            .map(|i| row.get(i).map(str::to_owned))
                            .collect(),
                    ),
                    _ => None,
                })
                .collect()
        })
    }

    /// The store opened on it, as a start opens it, holding the lock as `holder`.
    pub(crate) fn open(&self, holder: &str) -> Result<(Postgres, Loaded), super::DatabaseError> {
        self.open_with(TIMEOUTS, holder)
    }

    /// The store opened on a connection string of the test's own making.
    pub(crate) fn open_dsn(
        dsn: &Dsn,
        holder: &str,
    ) -> Result<(Postgres, Loaded), super::DatabaseError> {
        Postgres::open_with(dsn, TIMEOUTS, holder, &unix(), Arc::new(|| SPENT_NOW))
    }

    /// [`TestDatabase::open`] with the bounded waits `timeouts`.
    pub(crate) fn open_with(
        &self,
        timeouts: Timeouts,
        holder: &str,
    ) -> Result<(Postgres, Loaded), super::DatabaseError> {
        Postgres::open_with(
            &self.dsn(),
            timeouts,
            holder,
            &unix(),
            Arc::new(|| SPENT_NOW),
        )
    }

    /// Ends every session of the store on it but the tests' own: what a restart of the server
    /// or a network that dropped does to the coordinator.
    pub(crate) fn end_sessions(&self) {
        let name = self.name.clone();
        off_runtime(move || {
            let mut client = admin(None);
            client
                .execute(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = $1 AND pid <> pg_backend_pid()",
                    &[&name],
                )
                .expect("the sessions are ended");
        });
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let name = self.name.clone();
        // A database that cannot be dropped is the server's to clean; the test is over.
        let _ = std::thread::spawn(move || {
            let mut client = admin(None);
            for statement in [
                format!("SET lock_timeout = '300s'; SELECT pg_advisory_lock({CATALOG_KEY})"),
                format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"),
                format!("SELECT pg_advisory_unlock({CATALOG_KEY})"),
            ] {
                let _ = client.batch_execute(&statement);
            }
        })
        .join();
    }
}

/// The coordinator's wall clock in the tests: a fixed second.
pub(crate) const AT: u64 = 1_900_000_000;

/// The second CI's spent ids are judged by in the tests.
pub(crate) const SPENT_NOW: u64 = 150;

/// The clock of the tests' audit lines and lease.
pub(crate) fn unix() -> UnixClock {
    Arc::new(|| AT)
}

mod connect;
mod coordinator;
mod generations;
mod lost;
mod reads;
mod records;
mod records_errors;
mod schema;
mod serve;
mod serving;
mod session;
mod start;
pub(crate) mod store;

/// What the process logged since it started, as the tests' subscriber wrote it.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The writer of the tests' subscriber: it keeps what is logged in [`LOG`].
#[derive(Clone)]
struct Capture;

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        LOG.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Capture {
        Capture
    }
}

/// Turns on the process's logging, once, into [`LOG`]: the store's alerts are lines of it.
pub(crate) fn logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Capture)
            .with_ansi(false)
            .finish();
        // Another subscriber set first is the process's; the lines are then not captured.
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// Whether the process logged `text`.
pub(crate) fn logged(text: &str) -> bool {
    String::from_utf8_lossy(&LOG.lock().unwrap_or_else(PoisonError::into_inner)).contains(text)
}
