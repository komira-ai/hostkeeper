//! The reads that are not decisions (the audit log, the retention mark), the commit that has
//! nothing to write, and a start that the database refuses at the lease.

use super::TestDatabase;
use crate::audit_file::{Actor, Filter, Query};
use crate::store::conformance_tests::lines;
use crate::store::{Backend, Commit};

fn seqs(page: &[crate::audit_file::Logged]) -> Vec<u64> {
    page.iter().map(|logged| logged.entry.seq).collect()
}

/// Proves: a commit with nothing in it is answered Ok at once and writes nothing (the
/// transaction counter does not move), and a commit with an audit line numbers it above the
/// newest.
///
/// Catches: an empty change that takes a transaction number, or a last number that does not move.
#[test]
fn a_commit_with_nothing_in_it_writes_nothing() {
    let db = TestDatabase::new();
    let (store, _) = db.open("empty").expect("the store opens");
    let commits =
        |store: &crate::store::Postgres| store.on_writer(|w| w.commits()).expect("writer");
    store.commit(&Commit::default()).expect("nothing to write");
    assert_eq!(commits(&store), 0);
    assert_eq!(store.last_seq(), 0);
    let held = lines(1, 3);
    store
        .commit(&Commit {
            audit: &held,
            ..Commit::default()
        })
        .expect("the lines are kept");
    assert_eq!(commits(&store), 1);
    assert_eq!(store.last_seq(), 3);
}

/// Proves: the audit log is paged by who did it (an operator is neither the coordinator nor CI),
/// every line is visited in order, and a read the database cannot answer is refused with the
/// place of the error rather than an empty page.
///
/// Catches: a filter by actor that keeps the wrong lines, and a failed read read as no lines.
#[test]
fn the_audit_log_is_paged_by_actor_and_a_failed_read_is_an_error() {
    let db = TestDatabase::new();
    let (store, _) = db.open("audit").expect("the store opens");
    let held = lines(1, 9);
    store
        .commit(&Commit {
            audit: &held,
            ..Commit::default()
        })
        .expect("the lines are kept");
    let page = |actor| {
        store.audit_page(&Query {
            since_seq: 0,
            limit: 100,
            newest: false,
            filter: Filter {
                actor: Some(actor),
                ..Filter::default()
            },
        })
    };
    // `lines` names its actors alice, coordinator and ci in turn by the line's number.
    assert_eq!(
        seqs(&page(Actor::Coordinator).expect("a page")),
        vec![1, 4, 7]
    );
    assert_eq!(seqs(&page(Actor::Ci).expect("a page")), vec![2, 5, 8]);
    assert_eq!(seqs(&page(Actor::Operator).expect("a page")), vec![3, 6, 9]);
    let mut visited = Vec::new();
    store
        .audit_each(&mut |logged| visited.push(logged.entry.seq))
        .expect("every line");
    assert_eq!(visited, (1..=9).collect::<Vec<u64>>());

    // The reader's session is the page's above; it is ended, and each read after finds it gone.
    db.end_sessions();
    let read = page(Actor::Ci).expect_err("the reader is gone");
    assert!(
        read.to_string().starts_with("database:database: "),
        "{read}"
    );
    store
        .audit_pruned_before()
        .expect("the reader is made again");
    db.end_sessions();
    let each = store
        .audit_each(&mut |_| ())
        .expect_err("the reader is gone");
    assert!(
        each.to_string().starts_with("database:database: "),
        "{each}"
    );
    store
        .audit_pruned_before()
        .expect("the reader is made again");
    db.end_sessions();
    let pruned = store.audit_pruned_before().expect_err("the reader is gone");
    assert!(
        pruned.to_string().starts_with("database:database: "),
        "{pruned}"
    );
}

/// Proves: the time of the oldest audit line still held after a retention is the newest import's
/// mark, none when there is no import or the import deleted nothing, and none for a mark the
/// code cannot hold.
///
/// Catches: a mark of an older import answered for the newest, and a negative mark taken as a time.
#[test]
fn the_retention_mark_is_the_newest_imports() {
    let db = TestDatabase::new();
    let (store, _) = db.open("pruned").expect("the store opens");
    assert_eq!(store.audit_pruned_before().expect("a read"), None);
    db.run(
        "INSERT INTO imports (id, at_unix, manifest, audit_pruned_before) VALUES (1, 1, '{}', 77)",
    )
    .expect("an import");
    assert_eq!(store.audit_pruned_before().expect("a read"), Some(77));
    db.run("INSERT INTO imports (id, at_unix, manifest) VALUES (2, 2, '{}')")
        .expect("an import");
    assert_eq!(store.audit_pruned_before().expect("a read"), None);
    db.run(
        "INSERT INTO imports (id, at_unix, manifest, audit_pruned_before) VALUES (3, 3, '{}', -4)",
    )
    .expect("an import");
    assert_eq!(store.audit_pruned_before().expect("a read"), None);
}

/// Proves: a read the server refuses with a plain error is an error to its caller and keeps the
/// reading session (the same server process answers the next read).
///
/// Catches: a reader that is dropped, and made again, for a statement that only failed.
#[test]
fn a_refused_read_keeps_the_reading_session() {
    let db = TestDatabase::new();
    let (store, _) = db.open("reader").expect("the store opens");
    let pid = |store: &crate::store::Postgres| store.read(|session| session.backend_pid());
    let first = pid(&store).expect("a pid");
    let refused = store
        .read(|session| Ok(session.query_one("SELECT 1/0", &[])?.get::<_, i32>(0)))
        .expect_err("the statement is refused");
    assert_eq!(refused, "ERROR: division by zero (SQLSTATE 22012)");
    assert_eq!(pid(&store).expect("a pid"), first);
}

/// Proves: a start whose raising of the epoch the database refuses fails with the server's words
/// as a refusal, and leaves the lock to the next start.
///
/// Catches: a start that goes on without an epoch of its own.
#[test]
fn a_start_whose_epoch_the_database_refuses_is_refused() {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run(
        "CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'the test refuses this start'; END $$; \
         CREATE TRIGGER test_refuse BEFORE UPDATE ON lease FOR EACH ROW \
         EXECUTE FUNCTION test_refuse()",
    )
    .expect("the refusal is set");
    let error = db.open("second").err().expect("the start is refused");
    assert_eq!(
        error.to_string(),
        "the database refused the coordinator: ERROR: the test refuses this start (SQLSTATE P0001)"
    );
    db.run("DROP TRIGGER test_refuse ON lease")
        .expect("the refusal is lifted");
    let (third, _) = db.open("third").expect("the lock was given up");
    third.stop();
}
