//! The start's connection: what a refused or unreachable database says, and what that text
//! leaves out.

use std::net::TcpListener;

use super::{TestDatabase, server_url};
use crate::store::postgres::{DatabaseError, Dsn};

const PASSWORD: &str = "s3cret-text";

/// The host and port of the test server.
fn server() -> (String, u16) {
    let config: postgres::Config = server_url().parse().expect("the server's URL parses");
    let host = match config.get_hosts().first() {
        Some(postgres::config::Host::Tcp(host)) => host.clone(),
        _ => "127.0.0.1".to_owned(),
    };
    (host, config.get_ports().first().copied().unwrap_or(5432))
}

/// Proves: a login the server refuses (a role that does not exist) fails the start with the
/// server's own words as a refusal, and the text holds neither the password nor the database
/// that was named.
///
/// Catches: a connection string, a password or a file name in the text of a refused start.
#[test]
fn a_refused_connection_names_neither_the_password_nor_the_file() {
    let db = TestDatabase::new();
    let (host, port) = server();
    let user = format!("nobody_{}", db.name());
    let dsn = Dsn::parse(&format!(
        "host={host} port={port} dbname={} user={user} password={PASSWORD} sslmode=disable",
        db.name()
    ))
    .expect("a connection string");
    let error = TestDatabase::open_dsn(&dsn, "nobody")
        .err()
        .expect("the login is refused");
    assert!(matches!(error, DatabaseError::Refused(_)));
    let text = error.to_string();
    assert_eq!(
        text,
        format!(
            "the database refused the coordinator: FATAL: role \"{user}\" does not exist (SQLSTATE 28000)"
        )
    );
    assert!(!text.contains(PASSWORD));
    assert!(!text.contains("--database-dsn-file"));
}

/// Proves: a port where nothing listens fails the start as `the database is not up`, told from
/// a refusal by the kind of the I/O error, with a text that holds no password.
///
/// Catches: a database that is still starting being reported as one that refused the coordinator.
#[test]
fn a_database_where_nothing_listens_is_not_up() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("its address").port();
    drop(listener);
    let dsn = Dsn::parse(&format!(
        "host=127.0.0.1 port={port} dbname=x user=u password={PASSWORD} sslmode=disable"
    ))
    .expect("a connection string");
    let error = TestDatabase::open_dsn(&dsn, "nobody")
        .err()
        .expect("nothing answers");
    assert!(matches!(error, DatabaseError::NotUp(_)));
    let text = error.to_string();
    assert_eq!(
        text,
        "the database is not up: error connecting to server: Connection refused (os error 111)"
    );
    assert!(!text.contains(PASSWORD));
}
