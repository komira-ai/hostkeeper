//! The rollout gate (`docs/design/coordinator-multi-instance.md`, section 5.2) against a server:
//! an operator's edit queues for it behind the one transaction that holds it; a heartbeat's write
//! asks for it without waiting and is refused while it is held or asked for; a wait that runs out
//! refuses the edit and keeps the session; a holder that goes silent is ended by the server and the
//! gate freed; and a halt raised on two coordinators at once is one halt.
//!
//! The holder of the gate is a session of the test's; an edit that waits is seen waiting in
//! `pg_stat_activity`. Nothing sleeps and no time is measured: the waits that run out are the
//! server's own timeouts.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{OrderId, Outcome, UpdateResult};

use super::coordinator::Holding;
use super::edits::{held, instance, label, rows_of_labels, version};
use super::migrate_ddl::seen_waiting;
use super::{TestDatabase, options};
use crate::ops::{OpsError, OpsStore};
use crate::store::postgres::{Postgres, TIMEOUTS, Timeouts};
use crate::store::{Backend, Health, Part};
use crate::testutil::{id, version as release};
use crate::verdict_file::{self, VerdictFile};

/// The statements a test session holds the gate with, and frees it with.
const HOLD_GATE: &str = "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE";

/// Why a halt was not raised.
#[derive(Debug, PartialEq)]
enum NotRaised {
    /// A halt stands already.
    Already,
    /// The store did not take it.
    Store(String),
}

impl From<OpsError> for NotRaised {
    fn from(why: OpsError) -> NotRaised {
        NotRaised::Store(why.to_string())
    }
}

/// An operator's halt of `web` at 1.0 on `ops`.
fn halt(ops: &OpsStore) -> Result<u64, NotRaised> {
    let target: Version = release("1.0");
    crate::halt::raise_by_operator(ops, "web", &target, "alice", "stop", NotRaised::Already)
        .map(|entry| entry.seq)
}

/// The verdicts of a coordinator over `store`.
fn verdicts(store: &Postgres) -> VerdictFile {
    VerdictFile::over(Arc::new(store.clone()), verdict_file::Held::default())
}

/// A heartbeat of `host-1` that reports a failed update of `web`: a verdict to acknowledge.
fn beat(verdicts: &VerdictFile) -> Result<(), verdict_file::VerdictsError> {
    let report = UpdateResult {
        order_id: OrderId::parse("order-1"),
        order_try: Some(1),
        ..UpdateResult::new(release("2.0"), Outcome::Failed)
    };
    verdicts.record(&id("host-1"), &[("web".to_owned(), Some(report))], &[])
}

/// How many verdicts the database holds.
fn verdict_rows(db: &TestDatabase) -> usize {
    db.rows("SELECT service FROM verdict_standing").len()
}

/// Proves: while an operator's edit waits for the gate behind the transaction that holds it, a
/// heartbeat's write asks for the gate, is refused at once with the gate held, and writes nothing;
/// the operator's edit is still waiting after it, and is granted the gate and committed once the
/// holder ends; the heartbeat's next write is then taken.
///
/// Catches: a heartbeat's write that queues for the gate (it would wait behind the holder and
/// then ahead of, or beside, the operator, and hold up the heartbeat), and one that takes the gate
/// while an operator's edit is queued for it.
#[test]
fn beat_never_overtakes_a_queued_operator_edit() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let beats = verdicts(&one.store);
    let holder = Holding::start(&db, HOLD_GATE, "COMMIT");
    std::thread::scope(|scope| {
        let ops = &one.ops;
        let operator = scope.spawn(move || halt(ops));
        seen_waiting(&db, "rollout_gate");
        let refused = beat(&beats).expect_err("the gate is held");
        assert!(
            refused.to_string().contains("the rollout gate is held"),
            "{refused}"
        );
        assert_eq!(verdict_rows(&db), 0);
        assert_eq!(
            one.store.health(),
            Health::Up,
            "a busy gate loses no session"
        );
        seen_waiting(&db, "rollout_gate");
        holder.release();
        let raised = operator.join().expect("the operator's edit ends");
        assert!(raised.is_ok(), "{raised:?}");
    });
    assert_eq!(
        db.rows("SELECT action FROM audit")[0][0].as_deref(),
        Some("halt")
    );
    beat(&beats).expect("the gate is free");
    assert_eq!(verdict_rows(&db), 1);
}

/// Proves: an operator's halt raised while a transaction holds the gate and heartbeats keep asking
/// for it waits for that one holder only: every heartbeat's write meanwhile is refused at once and
/// none holds the gate, so none commits ahead of the halt; when the holder ends the halt is
/// committed, then the heartbeats write again.
///
/// Catches: a heartbeat that queues for the gate, so that an operator who comes while many beat
/// waits behind every one of them, and a halt that waits for anything but the one holder.
#[test]
fn operator_halt_waits_for_one_holder_not_for_beats() {
    let db = TestDatabase::new();
    let one = instance(&db, "one");
    let beats = verdicts(&one.store);
    let verdicts_then = version(&db, Part::Verdicts);
    let holder = Holding::start(&db, HOLD_GATE, "COMMIT");
    std::thread::scope(|scope| {
        let ops = &one.ops;
        let operator = scope.spawn(move || halt(ops));
        seen_waiting(&db, "rollout_gate");
        for _ in 0..20 {
            assert!(
                beat(&beats).is_err(),
                "a heartbeat never waits for the gate"
            );
        }
        assert_eq!(version(&db, Part::Verdicts), verdicts_then);
        holder.release();
        assert!(operator.join().expect("the halt ends").is_ok());
    });
    assert_eq!(version(&db, Part::Verdicts), verdicts_then);
    beat(&beats).expect("the gate is free");
    assert_eq!(version(&db, Part::Verdicts), verdicts_then + 1);
}

/// Proves: an operator's edit that is still waiting for the gate when `--operator-gate-wait-ms`
/// runs out is refused with nothing written, and the store keeps its session (it stays up, and its
/// next edit is taken once the gate is free); the refusal names the flag.
///
/// Catches: a wait that runs out taken as a lost session, which would make the whole coordinator
/// refuse every write and order nothing until a revival, and a refused edit that writes.
#[test]
fn an_edit_whose_wait_for_the_gate_runs_out_is_refused_and_keeps_the_session() {
    let db = TestDatabase::new();
    let short = crate::store::postgres::Options {
        gate_wait: Duration::from_millis(50),
        ..options()
    };
    let (store, loaded) = db
        .open_options(&short, "impatient")
        .expect("the store opens");
    let ops = OpsStore::over(Arc::new(store.clone()), loaded.ops);
    let holder = Holding::start(&db, HOLD_GATE, "COMMIT");
    let mut entry = crate::ops::AuditEntry::new("alice", "set_labels");
    entry.node_id = Some("host-1".to_owned());
    let refused = ops
        .change(crate::ops::Write::BeforeMemory, entry, |state| {
            state.hosts.entry("host-1".to_owned()).or_default();
        })
        .expect_err("the gate stays held");
    assert!(
        refused.to_string().contains("--operator-gate-wait-ms"),
        "{refused}"
    );
    assert_eq!(store.health(), Health::Up);
    assert!(db.rows("SELECT seq FROM audit").is_empty());
    holder.release();
    label(&ops, "host-1", "rack", "r1");
    assert_eq!(rows_of_labels(&db).len(), 1);
}

/// Proves: a coordinator that holds the gate in an open edit and then goes silent (its edit is
/// neither committed nor dropped) is ended by the server once its transaction has been idle for the
/// session's bound, which frees the gate: an operator's edit of another coordinator that waited
/// for it is then committed. The silent coordinator finds its session gone when it resumes: its
/// commit is refused, nothing of it written, and its store is lost until a revival.
///
/// Catches: a hold that no bound ends, which would make every edit and decision of every
/// coordinator wait for a coordinator that hung, and a resumed edit that lands after the gate was
/// given to another.
#[test]
fn stalled_holder_is_terminated_and_gate_is_released() {
    let db = TestDatabase::new();
    let quick = Timeouts {
        statement: Duration::from_millis(300),
        ..TIMEOUTS
    };
    let (silent, loaded) = db.open_with(quick, "silent").expect("the store opens");
    let silent_ops = OpsStore::over(Arc::new(silent.clone()), loaded.ops);
    let other = instance(&db, "other");
    let stalled = std::thread::scope(|scope| {
        let (inside, entered) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        let ops = &silent_ops;
        let stalled = scope.spawn(move || {
            let mut entry = crate::ops::AuditEntry::new("mallory", "set_labels");
            entry.node_id = Some("host-9".to_owned());
            ops.change(crate::ops::Write::BeforeMemory, entry, |state| {
                inside.send(()).expect("the test waits");
                resumed.recv().expect("the test resumes it");
                state.hosts.entry("host-9".to_owned()).or_default();
            })
        });
        entered.recv().expect("the silent edit holds the gate");
        label(&other.ops, "host-1", "rack", "r1");
        resume.send(()).expect("the edit waits");
        stalled.join().expect("the silent edit ends")
    });
    assert!(stalled.is_err(), "the silent edit's session was ended");
    assert_eq!(silent.health(), Health::Lost);
    assert_eq!(
        rows_of_labels(&db),
        [("host-1".to_owned(), "rack".to_owned(), "r1".to_owned())]
    );
    assert_eq!(held(&db), []);
}

/// Proves: an operator's halt of one service raised on two coordinators, one after the other, is
/// one halt and one audit line: the second coordinator's copy did not hold the first's halt, and
/// its edit, made on the rows under the gate, answers that a halt stands.
///
/// Catches: a halt raised on a stale copy, which would be raised again with a second audit line
/// and a second generation.
#[test]
fn a_halt_raised_on_two_instances_is_one_halt_and_one_audit_line() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    let first = halt(&a.ops).expect("a raises the halt");
    assert_eq!(halt(&b.ops), Err(NotRaised::Already));
    assert_eq!(
        db.rows("SELECT seq, action FROM audit"),
        [[Some(first.to_string()), Some("halt".to_owned())]]
    );
    assert_eq!(b.store.stale(), [], "b read the rows it was refused on");
}
