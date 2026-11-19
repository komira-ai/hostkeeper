//! Two coordinators on one database deciding heartbeats (`docs/design/coordinator-multi-instance.md`,
//! sections 5.2, 5.3 and 15): each heartbeat decided in one transaction under the rollout gate,
//! from the fleet's rows and every part read afresh, whichever coordinator hears it.
//!
//! Two coordinators are two stores opened on one database, each under a coordinator with its own
//! clock and memory, as two tasks of one service. A race is made to happen: both coordinators
//! answer on threads released by one barrier, many rounds; what is asserted holds for every
//! interleaving the gate allows. The quiet time a gap in the beats sets is a row the test writes,
//! as time passing would. Nothing sleeps and no time is measured.

use std::sync::Barrier;
use std::thread;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::json;

use super::TestDatabase;
use super::coordinator::{Holding, start_on_postgres};
use crate::marks_support::{
    NEW_DAEMON, WEB, ask, beat_of, operator, runs, services_with, set, update,
};
use crate::store::{Backend, Postgres};
use crate::testutil::Rig;

/// What a session of the test runs to hold the gate, as another decision does.
const HOLD_GATE: &str = "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE";

/// One coordinator of the pair.
struct Task {
    rig: Rig,
    store: Postgres,
}

/// Two coordinators on one database, running `web` and `db` with the definition lines `extra` of
/// `web`.
struct Pair {
    a: Task,
    b: Task,
    database: TestDatabase,
}

fn task(database: &TestDatabase, name: &str, extra: &str) -> Task {
    let (store, loaded) = database
        .open_tuned(crate::store::postgres::TIMEOUTS, &super::pooled(2), name)
        .expect("the store opens");
    let mut rig = Rig::new(&format!("deciding-{name}"));
    let marker = rig.dir.path().join("halt.marker");
    rig.coordinator =
        start_on_postgres(&rig, &store, loaded, &marker).with_services(services_with(extra));
    rig.at(2_000);
    Task { rig, store }
}

impl Pair {
    /// The pair, `web` desired at 1.0 then 2.0 (set through `a`), and each of `hosts` (of the
    /// role `role`) heard at 1.0 by both; the quiet time of the first beats is over.
    async fn new(extra: &str, role: &str, hosts: &[&str]) -> Pair {
        let database = TestDatabase::new();
        let (a, b) = (task(&database, "a", extra), task(&database, "b", extra));
        set(&a.rig, WEB, "1.0", 1).await;
        set(&a.rig, WEB, "2.0", 2).await;
        let pair = Pair { a, b, database };
        pair.unquiet();
        for host in hosts {
            for task in [&pair.a, &pair.b] {
                assert_eq!(
                    ask(&task.rig, host, role, running("1.0")),
                    ServiceReply::Nothing
                );
            }
        }
        pair.unquiet();
        pair
    }

    /// Ends the quiet time after the gap the first beats found: what passing time does.
    fn unquiet(&self) {
        self.database
            .run("UPDATE cluster_quiet SET until_unix = 0")
            .expect("the quiet time ends");
    }

    /// The orders the rows hold, as (host, target), by host.
    fn orders(&self) -> Vec<(String, String)> {
        self.database
            .rows("SELECT host_id, target FROM fleet_orders WHERE service = 'web' ORDER BY host_id")
            .into_iter()
            .map(|row| {
                (
                    row[0].clone().unwrap_or_default(),
                    row[1].clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// How many audit lines of `action` whose reason holds `text` there are.
    fn lines(&self, action: &str, text: &str) -> usize {
        self.database
            .rows(&format!(
                "SELECT 1 FROM audit WHERE action = '{action}' AND coalesce(reason, '') LIKE '%{text}%'"
            ))
            .len()
    }
}

/// A heartbeat of a daemon that orders are typed for, running `web` at `version`.
fn running(version: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, version)])
}

/// [`running`] 1.0, its last update of `web` to 2.0 having failed (`halt`: in its halt check).
fn failing(halt: bool) -> Heartbeat {
    let mut report = json!({"target": "2.0", "outcome": "failed"});
    if halt {
        report["phase"] = json!("halt");
        report["reason"] = json!("disk_tainted");
    }
    let entry: ServiceBeat = serde_json::from_value(json!({
        "name": WEB, "version": "1.0", "health": "healthy", "last_update": report
    }))
    .expect("a service beat");
    beat_of(NEW_DAEMON, vec![entry])
}

/// Proves: two coordinators never give a role more updates at once than its `max_updating`: an
/// order one gave is read from the rows by the other, which orders no other host of the role
/// past the slot; and when both answer the role's hosts at once, round after round, the gate lets
/// one order through between them and the rows hold one order.
/// Catches: a coordinator that counts the slots from its own memory (each would give one, two in
/// all), or a decision that reads the orders before the gate.
#[tokio::test(flavor = "multi_thread")]
async fn two_coordinators_never_exceed_max_updating() {
    let hosts = ["s1", "s2", "s3", "s4"];
    let pair = Pair::new("", "storage", &hosts).await;
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0")
    );
    for host in ["s2", "s3", "s4"] {
        assert_eq!(
            ask(&pair.b.rig, host, "storage", running("1.0")),
            ServiceReply::Nothing
        );
    }
    assert_eq!(
        ask(&pair.b.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0"),
        "the same"
    );
    assert_eq!(pair.orders(), [("s1".to_owned(), "2.0".to_owned())]);
    pair.database
        .run("DELETE FROM fleet_orders")
        .expect("the slot is free again");
    for _round in 0..8 {
        let barrier = Barrier::new(2);
        let given: usize = thread::scope(|s| {
            let (a, b) = (&pair.a, &pair.b);
            let barrier = &barrier;
            let one = s.spawn(move || race(a, barrier, ["s1", "s2", "s3", "s4"]));
            let other = s.spawn(move || race(b, barrier, ["s4", "s3", "s2", "s1"]));
            one.join().expect("a racer ends") + other.join().expect("a racer ends")
        });
        let orders = pair.orders();
        assert!(orders.len() <= 1, "one slot: {orders:?}");
        assert!(
            given <= 2,
            "the one order, told at most once more to its host"
        );
        pair.database
            .run("DELETE FROM fleet_orders")
            .expect("the slot is free again");
    }
}

/// Asks `task` for each of `hosts` at 1.0 once `barrier` lets it go: how many it ordered.
fn race(task: &Task, barrier: &Barrier, hosts: [&str; 4]) -> usize {
    barrier.wait();
    hosts
        .iter()
        .filter(|host| ask(&task.rig, host, "storage", running("1.0")) != ServiceReply::Nothing)
        .count()
}

/// Proves: a beat that finds the gate held is told nothing new and writes no order; the next beat,
/// with the gate free, is ordered, a row of another coordinator's that does not read left out.
/// Catches: a beat that waits for the gate (every beat of the fleet behind one hold), or one
/// that orders without it.
#[tokio::test(flavor = "multi_thread")]
async fn gate_busy_answers_the_beat_with_no_new_order() {
    let pair = Pair::new("", "storage", &["s1"]).await;
    // A row another coordinator wrote that this build cannot read is left out.
    pair.database
        .run(
            "INSERT INTO fleet_hosts (host_id, role, beat, seen_unix, sent_unix, instance) \
             VALUES ('s9', 'storage', '\\x6e6f74206a736f6e', 1, 1, 'another')",
        )
        .expect("an unreadable row");
    let holder = Holding::start(&pair.database, HOLD_GATE, "COMMIT");
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        ServiceReply::Nothing
    );
    assert!(pair.orders().is_empty());
    holder.release();
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0")
    );
}

/// Proves: an order given by one coordinator is answered again, at the same try, by the other
/// when the host's next beat reaches it before the host ran it: the order row is the order.
/// Catches: an order lost with the coordinator that gave it (its reply lost in a crash after the
/// commit), or given again at a new try the host refuses.
#[tokio::test(flavor = "multi_thread")]
async fn order_after_crash_is_returned_again() {
    let pair = Pair::new("", "storage", &["s1"]).await;
    let (_, given) = crate::marks_support::ask_typed(&pair.a.rig, "s1", "storage", running("1.0"));
    let (reply, again) =
        crate::marks_support::ask_typed(&pair.b.rig, "s1", "storage", running("1.0"));
    assert_eq!(reply, update(WEB, "2.0"));
    assert_eq!(again, given);
    assert!(given.is_some());
}

/// Proves: a halt an operator raised through one coordinator, once acknowledged, is read by the
/// next decision of the other, which orders nothing of the service; a desired version set
/// through one is the version the other orders.
/// Catches: a decision made on the other coordinator's memory, which still holds no halt and the
/// version before (an order that outruns an acknowledged halt).
#[tokio::test(flavor = "multi_thread")]
async fn acknowledged_halt_and_desired_change_are_read_by_the_other_coordinators_next_decision() {
    let pair = Pair::new("", "storage", &["s1", "s2"]).await;
    set(&pair.a.rig, WEB, "3.0", 3).await;
    assert_eq!(
        ask(&pair.b.rig, "s1", "storage", running("1.0")),
        update(WEB, "3.0")
    );
    pair.database
        .run("DELETE FROM fleet_orders")
        .expect("the slot is free again");
    let halt = proto::HaltRequest {
        service: WEB.to_owned(),
        reason: "look first".to_owned(),
    };
    pair.a
        .rig
        .coordinator
        .halt(&operator(), halt)
        .expect("halted");
    for host in ["s1", "s2"] {
        assert_eq!(
            ask(&pair.b.rig, host, "storage", running("1.0")),
            ServiceReply::Nothing
        );
    }
    assert!(pair.orders().is_empty());
}

/// Proves: two coordinators that both hear a host's halt report raise one halt between them: one
/// `halt` line and one generation; the other reads the halt standing and records a report.
/// Catches: a halt raised on each coordinator's memory (two lines, two generations), or one
/// written apart from the decision that read it.
#[tokio::test(flavor = "multi_thread")]
async fn halt_raised_by_two_instances_is_one_generation_and_one_audit_line() {
    let pair = Pair::new("", "storage", &["s1", "s2"]).await;
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", failing(true)),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&pair.b.rig, "s2", "storage", failing(true)),
        ServiceReply::Nothing
    );
    assert_eq!(pair.lines("halt", ""), 1);
    assert_eq!(pair.lines("halt_report", ""), 1);
    assert_eq!(pair.database.rows("SELECT 1 FROM halts").len(), 1);
}

/// Proves: in the quiet time after a gap in the beats (the row a beat raised), neither coordinator
/// orders, marks a version bad or rolls back, whatever the hosts that came back first report;
/// once it ends, orders are given again.
/// Catches: a quiet rule that holds back orders but not verdicts (a partial fleet reverts a
/// healthy rollout), or one kept per coordinator rather than read from the rows.
#[tokio::test(flavor = "multi_thread")]
async fn partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing() {
    let hosts = ["d1", "d2", "d3", "d4"];
    let database = TestDatabase::new();
    let auto = "rollback = \"automatic\"";
    let (a, b) = (task(&database, "a", auto), task(&database, "b", auto));
    set(&a.rig, WEB, "1.0", 1).await;
    let pair = Pair { a, b, database };
    for _ in 0..2 {
        for host in hosts {
            ask(&pair.a.rig, host, "daemon", running("1.0"));
            ask(&pair.b.rig, host, "daemon", running("1.0"));
        }
        // The first beats found a gap; the second round, past it, records 1.0 as good.
        pair.unquiet();
    }
    assert_eq!(pair.lines("last_good", "1.0"), 1);
    set(&pair.a.rig, WEB, "2.0", 2).await;
    pair.database
        .run("UPDATE cluster_quiet SET until_unix = extract(epoch FROM now())::bigint + 3600")
        .expect("a gap in the beats");
    for host in hosts {
        for task in [&pair.a, &pair.b] {
            assert_eq!(
                ask(&task.rig, host, "daemon", failing(false)),
                ServiceReply::Nothing
            );
        }
    }
    assert_eq!(
        pair.lines("mark_bad", ""),
        0,
        "no verdict in the quiet time"
    );
    assert!(pair.orders().is_empty());
    let desired = pair
        .database
        .rows("SELECT version FROM desired WHERE service = 'web'");
    assert_eq!(desired, [[Some("2.0".to_owned())]], "nothing rolled back");
    pair.unquiet();
    let ordered = hosts
        .into_iter()
        .filter(|host| ask(&pair.b.rig, host, "daemon", running("1.0")) != ServiceReply::Nothing)
        .count();
    assert!(
        ordered > 0,
        "once the quiet time ends, orders are given again"
    );
}

/// Proves: a heartbeat that cannot reach the database orders nothing and writes nothing; once the
/// session is made again, the next beat is ordered.
/// Catches: a coordinator that decides from memory while the database is away (an order no other
/// coordinator can see, or one placed past a halt not yet read).
#[tokio::test(flavor = "multi_thread")]
async fn database_unreachable_orders_nothing() {
    let pair = Pair::new("", "storage", &["s1"]).await;
    pair.database.end_sessions();
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        ServiceReply::Nothing
    );
    assert!(pair.orders().is_empty());
    pair.a.store.settle().expect("the writers run");
    pair.a.store.revive();
    pair.a.store.settle().expect("the writers run");
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0")
    );
    assert_eq!(pair.orders(), [("s1".to_owned(), "2.0".to_owned())]);
    let _ = pair.b.store.health();
}

/// Proves: when a plan's stage is done, coordinators that both decide at once enter the next
/// stage once between them: one `rollout_advance` line for it, whichever beats race.
/// Catches: a stage entered by each coordinator that read it done (two lines, a position written
/// over another's), or one entered without the compare-and-set on the plan's row.
#[tokio::test(flavor = "multi_thread")]
async fn stage_advance_by_racing_instances_is_one_advance_and_one_audit_line() {
    let hosts = ["d1", "d2", "d3", "d4"];
    let pair = Pair::new("bake_beats = 1\nstages = [50, 100]", "daemon", &hosts).await;
    let first: Vec<&str> = hosts
        .into_iter()
        .filter(|host| ask(&pair.a.rig, host, "daemon", running("1.0")) != ServiceReply::Nothing)
        .collect();
    assert_eq!(first.len(), 2, "the first stage is half of the hosts");
    for host in &first {
        for task in [&pair.b, &pair.a] {
            ask(&task.rig, host, "daemon", running("2.0"));
        }
    }
    let rest: Vec<&str> = hosts
        .into_iter()
        .filter(|host| !first.contains(host))
        .collect();
    let barrier = Barrier::new(2);
    thread::scope(|s| {
        for (task, host) in [(&pair.a, rest[0]), (&pair.b, rest[1])] {
            let barrier = &barrier;
            s.spawn(move || {
                barrier.wait();
                ask(&task.rig, host, "daemon", running("1.0"));
            });
        }
    });
    for (task, host) in [(&pair.b, rest[0]), (&pair.a, rest[1])] {
        ask(&task.rig, host, "daemon", running("1.0"));
    }
    assert_eq!(pair.lines("rollout_advance", "stage 2 of 2"), 1);
    assert_eq!(pair.orders().len(), 2, "the second stage's hosts");
}

/// Proves: a decision whose order the rows refuse at its end lands nothing and the host is told
/// nothing; one whose halt the rows refuse in its middle lands nothing either, and the halt is
/// neither in the coordinator's memory nor pending there; once the rows take them, the next beat
/// is ordered.
/// Catches: an order answered although its decision rolled back, or a change of the decision left
/// in memory (a stage or a halt a coordinator then answers that no other can see).
#[tokio::test(flavor = "multi_thread")]
async fn a_decision_that_does_not_land_orders_nothing_and_keeps_nothing() {
    let pair = Pair::new("", "storage", &["s1"]).await;
    pair.database
        .run("ALTER TABLE fleet_orders ADD CONSTRAINT not_s1 CHECK (host_id <> 's1')")
        .expect("the rows refuse s1");
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        ServiceReply::Nothing
    );
    assert!(
        pair.orders().is_empty(),
        "the order the rows refused is not answered"
    );
    pair.database
        .run(
            "ALTER TABLE fleet_orders DROP CONSTRAINT not_s1; \
             ALTER TABLE halts ADD CONSTRAINT no_halt CHECK (service <> 'web') NOT VALID",
        )
        .expect("the rows take s1 and refuse a halt of web");
    let ops = pair.a.rig.coordinator.ops();
    let before = ops.state();
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", failing(true)),
        ServiceReply::Nothing
    );
    assert_eq!(
        pair.lines("halt", ""),
        0,
        "the halt raised in it did not land"
    );
    assert_eq!(ops.state(), before, "memory is put back");
    assert!(!ops.halt_unwritten(), "and nothing of it is pending");
    pair.database
        .run("ALTER TABLE halts DROP CONSTRAINT no_halt")
        .expect("the rows take a halt");
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0")
    );
}

/// Proves: while a change stands in memory and not in the rows (its commit failed, and so did its
/// retry at this beat), the coordinator decides nothing; once it lands, decisions go on.
/// Catches: a decision made beside a change only this coordinator's memory holds (another
/// coordinator would decide without it), or one that lands it in a decision that may roll back.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_not_yet_in_the_rows_holds_back_every_decision() {
    let pair = Pair::new("", "storage", &["s1"]).await;
    set(&pair.a.rig, "db", "1.0", 1).await;
    pair.database
        .run(
            "CREATE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS \
               $$BEGIN RAISE EXCEPTION 'refused by the test'; END$$; \
             CREATE TRIGGER refuse BEFORE INSERT ON audit FOR EACH ROW EXECUTE FUNCTION refuse()",
        )
        .expect("the trigger is made");
    let halt = proto::HaltRequest {
        service: "db".to_owned(),
        reason: "look first".to_owned(),
    };
    let raised = pair.a.rig.coordinator.halt(&operator(), halt);
    assert_eq!(raised.map(drop), Err(proto::ReleasesRefusal::StoreFailed));
    assert!(pair.a.rig.coordinator.ops().halt_unwritten());
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        ServiceReply::Nothing
    );
    assert!(pair.orders().is_empty());
    pair.database
        .run("DROP TRIGGER refuse ON audit")
        .expect("dropped");
    assert_eq!(
        ask(&pair.a.rig, "s1", "storage", running("1.0")),
        update(WEB, "2.0")
    );
    assert_eq!(
        pair.database.rows("SELECT 1 FROM halts").len(),
        1,
        "the halt landed first"
    );
}
