//! A database of one test's own on the server `HOSTKEEPER_TEST_POSTGRES_URL` names, made with
//! a unique name and dropped with the test, so tests running in parallel share nothing. A test
//! that needs the server fails when the variable is not set, saying how to start one; it never
//! skips.

use std::sync::atomic::{AtomicU64, Ordering};

use postgres::{Client, NoTls};

/// The key of the advisory lock the coordinator holds on its database.
pub const COORDINATOR_LOCK: i64 = 0x686b_6370_6731;

/// The key of the advisory lock the tests take around a `CREATE DATABASE` or `DROP DATABASE`,
/// which the server does not take side by side.
const CATALOG_LOCK: i64 = 0x686b_6974_6573;

/// The URL of the test server ([`hostkeeper_testkit::postgres_url`]).
pub fn server_url() -> String {
    hostkeeper_testkit::postgres_url()
}

/// A client of the server, of the database `dbname` when one is named.
pub fn client(dbname: Option<&str>) -> Client {
    let mut config: postgres::Config = server_url().parse().expect("the server's URL parses");
    if let Some(name) = dbname {
        config.dbname(name);
    }
    config.connect(NoTls).expect("the test server answers")
}

/// Runs `f` on a session of the server's own database that holds the catalog lock, which other
/// tests take around their `CREATE DATABASE` and `DROP DATABASE`.
pub fn with_catalog_lock<T>(f: impl FnOnce(&mut Client) -> T) -> T {
    let mut admin = client(None);
    admin
        .batch_execute(&format!(
            "SET lock_timeout = '300s'; SELECT pg_advisory_lock({CATALOG_LOCK})"
        ))
        .expect("the catalog lock");
    let out = f(&mut admin);
    admin
        .batch_execute(&format!("SELECT pg_advisory_unlock({CATALOG_LOCK})"))
        .expect("the catalog lock is released");
    out
}

/// Runs `statements` on the server's own database under the catalog lock.
fn catalog(statements: &[String]) {
    with_catalog_lock(|admin| {
        for statement in statements {
            admin
                .batch_execute(statement)
                .expect("the catalog change runs");
        }
    });
}

/// A database of one test's own, dropped with it.
pub struct Database {
    name: String,
}

impl Database {
    pub fn new() -> Database {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut random = [0_u8; 4];
        getrandom::fill(&mut random).expect("random bytes");
        let name = format!(
            "hk_i_{}_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            u32::from_le_bytes(random)
        );
        catalog(&[format!("CREATE DATABASE \"{name}\"")]);
        Database { name }
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The connection string of the database, in keyword form, as the file holds it.
    pub fn dsn_text(&self) -> String {
        let config: postgres::Config = server_url().parse().expect("the server's URL parses");
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

    /// Waits until no session holds the coordinator's lock: the server frees it when the
    /// backend of a session that was closed has gone, which the close does not wait for.
    pub fn lock_is_free(&self) {
        let mut session = client(Some(&self.name));
        session
            .batch_execute(&format!(
                "SELECT pg_advisory_lock({COORDINATOR_LOCK}); \
                 SELECT pg_advisory_unlock({COORDINATOR_LOCK})"
            ))
            .expect("the lock is taken and freed");
    }

    /// The audit numbers the database holds.
    pub fn audit_numbers(&self) -> Vec<i64> {
        client(Some(&self.name))
            .query("SELECT seq FROM hostkeeper.audit ORDER BY seq", &[])
            .expect("the audit table")
            .iter()
            .map(|row| row.get(0))
            .collect()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // A database that cannot be dropped is the server's to clean; the test is over.
        let drop = format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name);
        let _ = std::panic::catch_unwind(|| catalog(&[drop]));
    }
}
