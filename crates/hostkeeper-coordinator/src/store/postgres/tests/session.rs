//! Sessions: how a client error is told to end the session or not, how a failure to connect is
//! told "not up" from "refused", and the worker thread a session lives on.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use super::{TestDatabase, admin, off_runtime};
use crate::store::postgres::session::{Fail, Session, Worker, describe, ends_session, not_up};
use crate::store::postgres::{DatabaseError, Dsn, TIMEOUTS};

fn config_of(text: &str) -> postgres::Config {
    Dsn::parse(text)
        .expect("a connection string")
        .config(TIMEOUTS)
}

fn connect_error(config: &postgres::Config) -> Fail {
    match Session::connect(config, Arc::new(AtomicU64::new(0)), false) {
        Err(fail) => fail,
        Ok(_) => panic!("the connect was not refused"),
    }
}

/// A server on a port of its own that reads what the client sends first and answers with `reply`
/// bytes, once, then closes.
fn server_answering(reply: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("its address").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut startup = [0_u8; 512];
            let _ = stream.read(&mut startup);
            let _ = stream.write_all(&reply);
        }
    });
    port
}

/// An `ErrorResponse` message with the severity, SQLSTATE and message given.
fn error_response(severity: &str, code: &str, message: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (tag, text) in [
        (b'S', severity),
        (b'V', severity),
        (b'C', code),
        (b'M', message),
    ] {
        body.push(tag);
        body.extend_from_slice(text.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut out = vec![b'E'];
    out.extend_from_slice(&u32::try_from(body.len() + 4).expect("short").to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Proves: a connect that the server answers with `57P03` (starting up), one to a port where
/// nothing listens, and one to a Unix socket that does not exist all mean "not up"; a server that
/// answers with a login refusal, one that closes the connection without a word, and a
/// configuration that names no host do not.
///
/// Catches: a database that is still starting being taken as one that refuses the coordinator, and
/// a refusal that is waited out as if the server would come up.
#[test]
fn a_connect_is_not_up_by_the_clients_typed_errors_only() {
    let starting = server_answering(error_response(
        "FATAL",
        "57P03",
        "the database system is starting up",
    ));
    let fail = connect_error(&config_of(&format!(
        "host=127.0.0.1 port={starting} user=u"
    )));
    let Fail::Pg(e) = &fail else {
        panic!("the client's error")
    };
    assert!(not_up(e));
    assert_eq!(
        DatabaseError::from(fail).to_string(),
        "the database is not up: FATAL: the database system is starting up (SQLSTATE 57P03)"
    );

    let refusing = server_answering(error_response(
        "FATAL",
        "28P01",
        "password authentication failed",
    ));
    let fail = connect_error(&config_of(&format!(
        "host=127.0.0.1 port={refusing} user=u"
    )));
    let Fail::Pg(e) = &fail else {
        panic!("the client's error")
    };
    assert!(!not_up(e));
    assert_eq!(
        DatabaseError::from(fail).to_string(),
        "the database refused the coordinator: FATAL: password authentication failed (SQLSTATE 28P01)"
    );

    let silent = server_answering(Vec::new());
    let Fail::Pg(e) = connect_error(&config_of(&format!("host=127.0.0.1 port={silent} user=u")))
    else {
        panic!("the client's error")
    };
    assert!(
        !not_up(&e),
        "an end of stream is not a server that is not up"
    );

    let Fail::Pg(e) = connect_error(&config_of("host=/no/such/directory user=u")) else {
        panic!("the client's error")
    };
    assert!(not_up(&e), "a socket that is not made yet");

    let no_host = postgres::Config::new();
    let Fail::Pg(e) = connect_error(&no_host) else {
        panic!("the client's error")
    };
    assert!(!not_up(&e));
    assert_eq!(
        describe(&e),
        "invalid configuration: both host and hostaddr are missing"
    );
}

/// Proves: a session that was ended by the server (its connection closed, or told `FATAL`) and one
/// whose statement was cancelled or could not get a lock end the session, while a statement the
/// server refuses with a plain error (a constraint, a division by zero) keeps it; the text of each
/// is the server's severity, message and SQLSTATE.
///
/// Catches: a store that keeps writing on a dead session, and one that drops a good session for a
/// refused statement.
#[test]
fn a_session_ends_when_the_server_ends_it_or_a_wait_runs_out_and_not_for_a_refusal() {
    let db = TestDatabase::new();
    let (migrated, _) = db.open("migrated").expect("the store opens");
    migrated.stop();
    let config = db.dsn().config(TIMEOUTS);
    let (refused, cancelled, locked, ended) = off_runtime(move || {
        let connect =
            || Session::connect(&config, Arc::new(AtomicU64::new(0)), false).expect("a session");
        let mut session = connect();
        let refused = session.query_one("SELECT 1/0", &[]).expect_err("a refusal");
        let cancelled = session
            .query_one("SELECT pg_cancel_backend(pg_backend_pid()), count(*) FROM generate_series(1, 1000000)", &[])
            .expect_err("a cancel");

        let mut holder = connect();
        holder.batch("BEGIN").expect("a transaction");
        holder
            .batch("SELECT * FROM lease FOR UPDATE")
            .expect("a row lock");
        let locked = session
            .query("SELECT * FROM lease FOR UPDATE NOWAIT", &[])
            .expect_err("no lock");

        let pid = session.backend_pid().expect("a pid");
        admin(None)
            .execute("SELECT pg_terminate_backend($1)", &[&pid])
            .expect("the session is ended");
        let ended = session
            .query_one("SELECT 1", &[])
            .expect_err("a closed session");
        let again = session
            .query_one("SELECT 1", &[])
            .expect_err("still closed");
        (refused, cancelled, locked, (ended, again))
    });
    assert!(!ends_session(&refused));
    assert_eq!(
        describe(&refused),
        "ERROR: division by zero (SQLSTATE 22012)"
    );
    assert!(ends_session(&cancelled));
    assert_eq!(
        describe(&cancelled),
        "ERROR: canceling statement due to user request (SQLSTATE 57014)"
    );
    assert!(ends_session(&locked));
    assert_eq!(
        describe(&locked),
        "ERROR: could not obtain lock on row in relation \"lease\" (SQLSTATE 55P03)"
    );
    assert!(ends_session(&ended.0), "{}", describe(&ended.0));
    assert!(ends_session(&ended.1), "{}", describe(&ended.1));
    assert!(ended.1.is_closed());
    assert_eq!(describe(&ended.1), "connection closed");
}

/// Proves: a worker runs the jobs handed to it one at a time in the order they came, answers a
/// call with its job's answer, stops once the jobs before it ran, and answers nothing after it is
/// stopped, dropped or its thread's job panicked.
///
/// Catches: jobs that run out of order, and a call that waits for a thread that is gone.
#[test]
fn a_worker_runs_its_jobs_in_order_and_answers_nothing_once_gone() {
    let worker = Worker::spawn("test-worker", Vec::<u32>::new()).expect("a worker");
    for n in 1..=3 {
        worker.post(move |seen| seen.push(n));
    }
    assert_eq!(worker.call(|seen| seen.clone()), Some(vec![1, 2, 3]));
    assert_eq!(worker.call(|seen| seen.len()), Some(3));
    worker.stop();
    assert_eq!(worker.call(|seen| seen.len()), None);
    worker.post(|seen| seen.push(4));
    worker.stop();

    let ran = Arc::new(Mutex::new(Vec::new()));
    let worker = Worker::spawn("test-worker-2", ()).expect("a worker");
    let late = Arc::clone(&ran);
    worker.post(move |()| late.lock().expect("lock").push("posted"));
    drop(worker);
    assert_eq!(
        *ran.lock().expect("lock"),
        vec!["posted"],
        "a dropped worker ran its jobs first"
    );

    let worker = Worker::spawn("test-worker-3", ()).expect("a worker");
    assert_eq!(
        worker.call(|()| -> u32 { panic!("a job that panics") }),
        None
    );
    assert_eq!(worker.call(|()| 1), None);
    worker.post(|()| ());
    worker.stop();
}
