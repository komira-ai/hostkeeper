//! The audit numbers and the facts generations over Postgres
//! (`docs/design/coordinator-multi-instance.md`, section 6, "Audit numbers"): the counter numbers
//! the lines of two coordinators in the order of their commits with no gap, a start continues past
//! the lines a release before this one wrote, a change made while no edit could be begun is
//! numbered by the counter when it lands, and a host's facts generation is a column raised with
//! the change that raises it, which every coordinator answers.
//!
//! An edit is held open by its own closure, which waits for the test on a channel; an edit that
//! waits for the counter is seen waiting in `pg_stat_activity`. Nothing sleeps and no time is
//! measured.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use hostkeeper_enrollment::enroll::records::{Record, Records, pb};

use super::coordinator::Holding;
use super::edits::{Refused, audit_numbers, counter, instance, label};
use super::migrate_ddl::seen_waiting;
use super::{AT, TestDatabase, options, with_session};
use crate::audit_actions::AuditOnly;
use crate::ops::{AuditEntry, OpsStore, Write};
use crate::store::Backend;
use crate::store::postgres::writer::try_lock;
use crate::store::postgres::{PgRecords, migrate};

/// Proves: two coordinators' audit lines are numbered in the order of their commits, with no gap
/// across both: a line whose edit waits for the counter while another coordinator's edit holds it
/// takes the number after that edit's line, once it committed; an edit refused by its change takes
/// no number; the lines are 1, 2, 3 and so on with none twice.
///
/// Catches: numbers taken from each coordinator's own memory (the same number twice, one commit
/// refused on the key), numbers from a sequence (taken before the commit, so a reader paging by
/// number misses a line that commits later with a lower one), and a refused change that leaves a
/// gap.
#[test]
fn audit_numbers_follow_commit_order() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    let (inside, entered) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let (first, second) = std::thread::scope(|scope| {
        let ops = &a.ops;
        let holding = scope.spawn(move || {
            ops.change(
                Write::BeforeMemory,
                AuditEntry::new("alice", "exclude"),
                |_| {
                    inside.send(()).expect("the test waits");
                    gone.recv().expect("the test lets it go");
                },
            )
        });
        entered.recv().expect("a's edit holds the counter");
        let other = &b.ops;
        let waiting = scope.spawn(move || {
            other.append_audit_only(AuditOnly::ConsoleOpen, "bob", Some("host-1"), None)
        });
        seen_waiting(&db, "audit_counter");
        go.send(()).expect("a's edit waits");
        (
            holding.join().expect("a's edit ends").expect("committed"),
            waiting.join().expect("b's line ends").expect("appended"),
        )
    });
    assert_eq!((first.seq, second.seq), (1, 2));

    let refused = a.ops.try_change(
        Write::BeforeMemory,
        AuditEntry::new("alice", "exclude"),
        |_| {
            Err(crate::ops::OpsError::AuditUnavailable(
                std::io::Error::other("no"),
            ))
        },
    );
    assert!(refused.is_err());
    let third = label(&b.ops, "host-1", "rack", "r1");
    assert_eq!(third.seq, 3);
    assert_eq!(audit_numbers(&db), [1, 2, 3]);
    assert_eq!(counter(&db), 4);
}

/// Proves: a coordinator that starts on a database a release before this one wrote to after this
/// release last ran (the release before numbers its lines from `max(seq)` and never moves the
/// counter on) sets the counter past the newest line before it serves, and numbers its next line
/// after it: no number is held by two lines, and every line after the release before's is numbered
/// above it.
///
/// Catches: a start that keeps a counter the release before left behind, whose next line would take
/// the number of the release before's last line and be refused on the key, every audited change
/// with it.
#[test]
fn new_release_again_continues_audit_numbers_after_the_release_before_ran() {
    let db = TestDatabase::new();
    let first = instance(&db, "new");
    assert_eq!(label(&first.ops, "host-1", "rack", "r1").seq, 1);
    first.store.stop();
    db.run(
        "INSERT INTO audit (seq, at_unix, by_name, action) VALUES \
         ((SELECT max(seq) + 1 FROM audit), 1, 'carol', 'exclude')",
    )
    .expect("the release before writes a line");
    assert_eq!(
        counter(&db),
        2,
        "the release before left the counter behind"
    );

    let again = instance(&db, "new-again");
    assert_eq!(counter(&db), 3);
    assert_eq!(label(&again.ops, "host-2", "rack", "r2").seq, 3);
    assert_eq!(audit_numbers(&db), [1, 2, 3]);
}

/// The store `a` of a coordinator on `db` whose edits wait 50 ms at most for the gate, its ops
/// store, and the store's handle.
fn impatient(db: &TestDatabase) -> (crate::store::postgres::Postgres, OpsStore) {
    let short = crate::store::postgres::Options {
        gate_wait: Duration::from_millis(50),
        ..options()
    };
    let (store, loaded) = db.open_options(&short, "a").expect("the store opens");
    let ops = OpsStore::over(Arc::new(store.clone()), loaded.ops).with_unix(Arc::new(|| AT));
    (store, ops)
}

/// Raises an operator's halt of `web` on `ops` while a session of the test holds the gate past
/// the store's wait: the halt stands in memory, pending.
fn halt_while_the_gate_is_held(db: &TestDatabase, ops: &OpsStore) {
    let holder = Holding::start(
        db,
        "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE",
        "COMMIT",
    );
    let target = crate::testutil::version("1.0");
    let raised = crate::halt::raise_by_operator(ops, "web", &target, "alice", "stop", Refused);
    assert!(raised.is_err(), "the halt stands in memory, not written");
    assert!(ops.halt_unwritten());
    holder.release();
}

/// Proves: a memory-first change (an operator's halt) made while no edit can be begun (the gate
/// is held past `--operator-gate-wait-ms`) stands in memory, pending, under a number of its own
/// making; when another coordinator took that number meanwhile, the change lands at the next
/// heartbeat's retry with the counter's next number instead, and memory's ring carries it beside
/// the entries before it: the lines stay dense, none twice.
///
/// Catches: a pending change committed under the number it was made with, which another
/// coordinator's line took, so its commit is refused on the key at every heartbeat and the halt
/// never lands.
#[test]
fn a_change_pending_in_memory_lands_under_the_counter_number() {
    super::logging();
    let db = TestDatabase::new();
    let (store, ops) = impatient(&db);
    let other = instance(&db, "b");
    assert_eq!(label(&ops, "host-1", "rack", "r1").seq, 1);
    halt_while_the_gate_is_held(&db, &ops);
    let made: Vec<u64> = ops.state().audit.iter().map(|entry| entry.seq).collect();
    assert_eq!(made, [1, 2]);
    let line = other
        .ops
        .append_audit_only(AuditOnly::ConsoleOpen, "bob", None, None)
        .expect("b's line takes no gate");
    assert_eq!(line.seq, 2);

    ops.retry_unwritten(Duration::ZERO);
    assert!(!ops.halt_unwritten());
    assert_eq!(audit_numbers(&db), [1, 2, 3]);
    assert_eq!(
        db.rows("SELECT action FROM audit WHERE seq = 3")[0][0].as_deref(),
        Some("halt")
    );
    let ring: Vec<u64> = ops.state().audit.iter().map(|entry| entry.seq).collect();
    assert_eq!(ring, [1, 3]);
    assert!(super::logged(
        "changes made while no edit could be begun are written under the audit counter's numbers"
    ));
    assert_eq!(store.stale(), [], "memory holds what the rows hold");
}

/// Proves: a storage-first change made while a memory-first change is pending commits the pending
/// one first, in an edit of its own, then itself: the halt's line is numbered before the label's,
/// and nothing stays pending.
///
/// Catches: a change committed ahead of an earlier one that stands in memory, and an edit begun
/// while another transaction of the same store holds the gate (which would wait for itself).
#[test]
fn a_change_made_while_one_is_pending_commits_the_pending_one_first() {
    let db = TestDatabase::new();
    let (_store, ops) = impatient(&db);
    halt_while_the_gate_is_held(&db, &ops);
    let labelled = label(&ops, "host-1", "rack", "r1");
    assert!(!ops.halt_unwritten());
    assert_eq!(labelled.seq, 2);
    assert_eq!(
        db.rows("SELECT seq, action FROM audit ORDER BY seq"),
        [
            [Some("1".to_owned()), Some("halt".to_owned())],
            [Some("2".to_owned()), Some("set_labels".to_owned())]
        ]
    );
}

/// Proves (B8, both ways, for `0023`): the release before's runner (migrations `0001` to `0022`)
/// accepts a database this release migrated and wrote through, runs nothing, and takes the
/// coordinator's lock exclusively once this release has stopped; this release, started on a
/// database the release before migrated and wrote audit lines to, runs `0023`, whose counter starts
/// past those lines, and numbers its next line after them.
///
/// Catches: a `0023` marked `refuse`, which would turn a rollback into a restore, and a counter
/// seeded below the lines already written.
#[test]
fn the_release_before_runs_on_a_database_with_0023_and_this_one_on_its() {
    let db = TestDatabase::new();
    let this = instance(&db, "this release");
    label(&this.ops, "host-1", "rack", "r1");
    this.store.stop();
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..22]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(
        locked,
        "the release before has the lock once this release stopped"
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..22]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    old.run(
        "INSERT INTO audit (seq, at_unix, by_name, action) VALUES \
         (1, 1, 'carol', 'exclude'), (2, 1, 'carol', 'include')",
    )
    .expect("the release before writes lines");
    let again = instance(&old, "this release");
    assert_eq!(counter(&old), 3);
    assert_eq!(label(&again.ops, "host-1", "rack", "r1").seq, 3);
    assert_eq!(audit_numbers(&old), [1, 2, 3]);
}

/// A host's record of the enrollment store, with its facts generation `generation`.
fn host(generation: Option<u64>) -> pb::Host {
    pb::Host {
        facts_generation: generation,
        host_group: String::new(),
        moving_from: None,
        schema: 1,
        host_id: "host-1".to_owned(),
        key_hash: vec![1; 32],
        machine_id: "m-1".to_owned(),
        role: "web".to_owned(),
        services: vec!["web".to_owned()],
        duplicate: false,
        first_heartbeat_unix: None,
        first_serving_unix: None,
        running: None,
    }
}

/// The facts generation the database holds for `host-1`.
fn column(db: &TestDatabase) -> Option<String> {
    db.rows("SELECT facts_generation FROM enrollment_hosts WHERE host_id = 'host-1'")[0][0].clone()
}

/// Proves: a change of a host's labels raises the host's facts generation in the database to the
/// change's audit number, in the change's transaction; the coordinator that made it answers that
/// number, and another coordinator answers it too once it read the operations state again (0
/// before, as for every host no change raised); a second raise moves it on to the second change's
/// number.
///
/// Catches: a generation kept in each coordinator's memory, which two coordinators answer
/// differently so that a host whose beats land on either is sent its facts at every beat, and a
/// raise committed apart from its change.
#[test]
fn facts_generation_is_raised_with_the_change_and_answered_by_every_instance() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let records = PgRecords::over(&a.store);
    records
        .commit(&[], &[Record::Host(host(None))])
        .expect("the host is approved");
    let b = instance(&db, "b");
    assert_eq!(b.ops.facts_generation("host-1"), 0);
    let labelled = label(&a.ops, "host-1", "rack", "r1");
    assert_eq!(column(&db), Some(labelled.seq.to_string()));
    assert_eq!(a.ops.facts_generation("host-1"), labelled.seq);
    assert_eq!(b.ops.facts_generation("host-1"), 0);
    b.ops.refresh();
    assert_eq!(b.ops.facts_generation("host-1"), labelled.seq);

    let again = label(&b.ops, "host-1", "rack", "r2");
    assert_eq!(column(&db), Some(again.seq.to_string()));
    a.ops.refresh();
    assert_eq!(a.ops.facts_generation("host-1"), again.seq);
    assert_eq!(a.ops.facts_generation("host-2"), 0);
}
