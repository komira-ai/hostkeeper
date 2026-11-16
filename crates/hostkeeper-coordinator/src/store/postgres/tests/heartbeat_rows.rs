//! Heartbeats decided over the fleet's rows, with two coordinators on one database
//! (`service_rows`): each stores the beats it hears, and decides under the rollout gate on what it
//! has just read, so the orders of both never exceed what the desired state lets be updating and
//! a stage is entered once.
//!
//! No test sleeps or measures time. The moment between a coordinator's read and its turn at the
//! gate, which the race is about, is a hook the test runs there
//! ([`Coordinator::pause_before_gate`]): the other coordinator decides in it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_wire::wire::ServiceReply;

use super::coordinator::{DAEMON, daemon, start_on_postgres_at};
use super::{AT, TestDatabase};
use crate::marks_support::{DB, WEB, ask, services_with, set, update};
use crate::service::{Coordinator, RowRules};
use crate::store::Postgres;
use crate::testutil::Rig;

/// The rules two coordinators on one database read the rows by.
fn rules() -> RowRules {
    RowRules {
        beat_reorder: std::time::Duration::from_secs(30),
        queue_idle: std::time::Duration::from_secs(3600),
    }
}

/// One coordinator on the database, its clock at 2000 (past the quiet start).
struct Node {
    rig: Rig,
    store: Postgres,
}

/// The seconds a test has let pass since the coordinators started, which both read as wall clock
/// and as the time since their start.
const START: u64 = 2_000;

impl Node {
    fn start(db: &TestDatabase, name: &str, wall: &Arc<AtomicU64>, extra: &str) -> Node {
        let (store, loaded) = db.open(name).expect("the store opens");
        let mut rig = Rig::new(name);
        let marker = rig.dir.path().join("halt.marker");
        let wall = Arc::clone(wall);
        rig.coordinator = start_on_postgres_at(
            &rig,
            &store,
            loaded,
            &marker,
            Arc::new(move || wall.load(Ordering::SeqCst)),
        )
        .with_services(services_with(extra))
        .with_rows(store.fleet_rows(name), rules());
        rig.at(START);
        Node { rig, store }
    }

    /// Lets the time of both clocks be `elapsed` seconds past the start.
    fn at(&self, wall: &AtomicU64, elapsed: u64) {
        wall.store(AT + elapsed, Ordering::SeqCst);
        self.rig.at(START + elapsed);
    }

    fn coordinator(&self) -> &Coordinator {
        &self.rig.coordinator
    }

    /// The reply to the beat of the host `host`, which runs 1.0 of both services.
    fn beat(&self, host: &str) -> ServiceReply {
        self.beat_with(host, daemon("1.0"))
    }

    /// The reply to the beat `beat` of the host `host`.
    fn beat_with(&self, host: &str, beat: hostkeeper_wire::wire::Heartbeat) -> ServiceReply {
        ask(&self.rig, host, DAEMON, beat)
    }
}

/// Two coordinators on a database in which `web` and `db` are desired at 2.0 (1.0 before).
async fn pair(test: &str) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
    pair_with(test, "stages = [100]").await
}

/// [`pair`] over `web` defined with the extra lines `extra`.
async fn pair_with(test: &str, extra: &str) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
    let db = TestDatabase::new();
    let wall = Arc::new(AtomicU64::new(AT));
    let first = Node::start(&db, &format!("{test}-a"), &wall, extra);
    for service in [WEB, DB] {
        set(&first.rig, service, "1.0", 1).await;
        set(&first.rig, service, "2.0", 2).await;
    }
    let second = Node::start(&db, &format!("{test}-b"), &wall, extra);
    (db, wall, first, second)
}

fn orders(db: &TestDatabase) -> Vec<(String, String)> {
    db.rows("SELECT host_id, service FROM fleet_orders ORDER BY host_id")
        .into_iter()
        .map(|row| {
            (
                row[0].clone().expect("a host"),
                row[1].clone().expect("a service"),
            )
        })
        .collect()
}

/// Proves: a coordinator that heard only the third host before the gate, while the other
/// coordinator ordered the role's two update slots to the two it heard, is refused the third: it
/// reads the rows under the gate, finds the other's hosts and orders, and tells its host nothing.
/// The orders of both are the fleet's rows, two of them.
///
/// Catches: a coordinator that decides from the fleet it holds in memory, which has no word of
/// the other's hosts, and orders a third host of a role whose desired state lets two update at
/// once.
#[tokio::test]
async fn a_coordinator_that_read_before_the_gate_is_refused_the_slot_the_other_took() {
    let (db, wall, a, b) = pair("slots").await;
    // The first beats set the quiet time of a fleet nobody had heard, which is over 31 seconds
    // later.
    a.beat("h3");
    b.beat("h1");
    b.beat("h2");
    a.at(&wall, 31);
    b.at(&wall, 31);
    let b = Arc::new(b);
    let other = Arc::clone(&b);
    let others = Arc::new(std::sync::Mutex::new(Vec::new()));
    let said = Arc::clone(&others);
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = Arc::clone(&ran);
    a.coordinator().pause_before_gate(Arc::new(move || {
        // Once: the other coordinator's beats are not run under this hook.
        if !once.swap(true, Ordering::SeqCst) {
            let mut said = said.lock().expect("replies");
            said.push(other.beat("h1"));
            said.push(other.beat("h2"));
        }
    }));
    let third = a.beat("h3");
    assert!(
        ran.load(Ordering::SeqCst),
        "the other decided before the gate"
    );
    assert_eq!(
        *others.lock().expect("replies"),
        vec![update(WEB, "2.0"), update(WEB, "2.0")]
    );
    assert_eq!(third, ServiceReply::Nothing, "the slots are taken");
    assert_eq!(
        orders(&db),
        vec![
            ("h1".to_owned(), "web".to_owned()),
            ("h2".to_owned(), "web".to_owned())
        ]
    );
    let _ = (&a.store, &b.store);
}

/// A heartbeat of `host` that runs `web` at `web` and `db` at 1.0, updating `web` to 2.0 when
/// `updating`.
fn daemon_running(web: &str, updating: bool) -> hostkeeper_wire::wire::Heartbeat {
    let mut beat = daemon("1.0");
    let services = beat.services.as_mut().expect("services");
    services[0] = crate::marks_support::runs(WEB, web);
    if updating {
        services[0].updating = Some(crate::testutil::version("2.0"));
    }
    beat
}

/// A coordinator of a pair that has heard `hosts` and is past the quiet time.
async fn settled(test: &str, hosts: &[&str]) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
    let (db, wall, a, b) = pair(test).await;
    for host in hosts {
        a.beat(host);
    }
    a.at(&wall, 31);
    b.at(&wall, 31);
    (db, wall, a, b)
}

/// Proves: a beat that finds the rollout gate held by another transaction is answered with no
/// order and decides nothing (no order row), and the host's next beat, once the gate is free,
/// is ordered: the beat never waits for the gate.
///
/// Catches: a heartbeat that queues behind a decision for as long as it runs, or that orders
/// without holding the gate.
#[tokio::test]
async fn a_beat_that_finds_the_gate_held_is_told_nothing_and_the_next_one_is_ordered() {
    let (db, _wall, a, _b) = settled("busy", &["h1"]).await;
    let held = super::coordinator::Holding::start(
        &db,
        "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE",
        "COMMIT",
    );
    assert_eq!(a.beat("h1"), ServiceReply::Nothing);
    assert_eq!(orders(&db), vec![]);
    held.release();
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
    assert_eq!(orders(&db), vec![("h1".to_owned(), "web".to_owned())]);
}

/// Proves: a beat whose row cannot be written (the table is out of reach) is answered with no
/// order and writes no order, and once the rows are back the next beat of the host is ordered.
///
/// Catches: a coordinator that orders from its memory when the rows cannot be reached.
#[tokio::test]
async fn a_beat_whose_rows_cannot_be_written_orders_nothing() {
    let (db, _wall, a, _b) = settled("unreachable", &["h1"]).await;
    db.run("ALTER TABLE fleet_hosts RENAME TO fleet_hosts_away")
        .expect("renamed");
    assert_eq!(a.beat("h1"), ServiceReply::Nothing);
    db.run("ALTER TABLE fleet_hosts_away RENAME TO fleet_hosts")
        .expect("renamed back");
    assert_eq!(orders(&db), vec![]);
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
}

/// Proves: a row of another coordinator that does not read (a host id that is no host id, a beat
/// that is no beat) is left out and decides nothing, and the coordinator goes on ordering the
/// hosts it can read.
///
/// Catches: a row that stops every decision, or a coordinator that panics on a row it cannot read.
#[tokio::test]
async fn a_row_that_does_not_read_is_left_out() {
    let (db, _wall, a, b) = settled("garbage", &["h1"]).await;
    db.run(
        "INSERT INTO fleet_hosts (host_id, role, beat, seen_unix, sent_unix, up, instance) \
         VALUES ('Not A Host', 'daemon', 'x', 1, 0, true, 'elsewhere'), \
                ('h9', 'daemon', 'not json', 1, 0, true, 'elsewhere')",
    )
    .expect("rows");
    assert_eq!(b.beat("h1"), update(WEB, "2.0"));
    let _ = a;
}

/// Proves: a host that reports it is updating ends its order in the rows, and with every host at
/// the desired version of both services the queue of rollouts holds nobody: a service takes its
/// place when its desired version is set, in the order they were set, and leaves it when nothing
/// is left to roll.
///
/// Catches: an order that holds its slot after the host took it up, and a finished service that
/// keeps a place and holds the services set after it.
#[tokio::test]
async fn an_order_ends_when_the_host_updates_and_a_rolled_service_leaves_the_queue() {
    let (db, _wall, a, b) = settled("queue", &["h1"]).await;
    let queue = |db: &TestDatabase| -> Vec<String> {
        db.rows("SELECT service FROM rollout_queue ORDER BY seq")
            .into_iter()
            .map(|row| row[0].clone().expect("a service"))
            .collect()
    };
    assert_eq!(
        queue(&db),
        vec!["web", "db"],
        "the order the versions were set in"
    );
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
    assert_eq!(orders(&db).len(), 1);
    // The other coordinator hears the host update: the order is gone from the rows.
    assert_eq!(
        b.rig
            .coordinator
            .heartbeat_with_orders(
                crate::marks_support::who("h1", DAEMON),
                daemon_running("1.0", true),
                crate::trusted_root::TrustedRootSaid::default(),
            )
            .expect("answered")
            .0
            .reply,
        ServiceReply::Nothing
    );
    assert_eq!(orders(&db), vec![]);
    // Both services at 2.0 everywhere: nothing is left to roll.
    let mut done = daemon("2.0");
    done.agent_features = daemon("1.0").agent_features;
    assert_eq!(a.beat_with("h1", done), ServiceReply::Nothing);
    assert_eq!(queue(&db), Vec::<String>::new());
}

/// A heartbeat of a host that runs `web` at `version`, healthy, and `db` at 1.0.
fn web_at(version: &str) -> hostkeeper_wire::wire::Heartbeat {
    daemon_running(version, false)
}

fn count(db: &TestDatabase, sql: &str) -> String {
    db.rows(sql).remove(0).remove(0).expect("a count")
}

/// Proves: with two coordinators that both read the first stage of a plan baked, the one that
/// enters the second writes it, with its one audit line, and the other, which reaches the same
/// stage from the position it holds in memory (the plan at its first), finds the rows say it is
/// entered and writes nothing, and still orders the host the second stage lets go.
///
/// Catches: a stage entered twice, with two `rollout_advance` lines, by coordinators whose
/// memories both say the plan is at its first stage.
#[tokio::test]
async fn a_stage_both_coordinators_reach_is_entered_once() {
    let (db, wall, a, b) = pair_with("stage", "stages = [50, 100]\nbake_beats = 1").await;
    a.beat("h1");
    a.beat("h2");
    a.at(&wall, 31);
    b.at(&wall, 31);
    assert_eq!(
        a.beat("h1"),
        update(WEB, "2.0"),
        "the first stage holds one host"
    );
    assert_eq!(
        a.beat("h2"),
        ServiceReply::Nothing,
        "h2 waits for the second"
    );
    a.beat_with("h1", daemon_running("1.0", true));
    // h1 has taken the update and baked once: the first stage is done, and a enters the second.
    a.beat_with("h1", web_at("2.0"));
    let advances = "SELECT count(*) FROM audit WHERE action = 'rollout_advance'";
    assert_eq!(count(&db, advances), "1");
    assert_eq!(
        count(&db, "SELECT entered FROM rollouts WHERE service = 'web'"),
        "2"
    );
    // b, which has not seen the plan move, reaches the same stage and orders h2 under it.
    assert_eq!(b.beat("h2"), update(WEB, "2.0"));
    assert_eq!(
        count(&db, advances),
        "1",
        "the second stage was entered once"
    );
    assert_eq!(
        count(&db, "SELECT entered FROM rollouts WHERE service = 'web'"),
        "2"
    );
}

/// Proves: the rows being out of reach inside the decision (the table of the orders) orders
/// nothing and leaves nothing written; the next beat, with the rows back, is ordered. The same
/// for the queue's table inside the admission: no order is written, because the transaction is
/// rolled back with it.
///
/// Catches: an order written, or given, by a decision whose transaction failed.
#[tokio::test]
async fn a_decision_whose_rows_fail_orders_nothing_and_writes_nothing() {
    let (db, _wall, a, _b) = settled("fails", &["h1"]).await;
    for table in ["fleet_orders", "rollout_queue"] {
        db.run(&format!("ALTER TABLE {table} RENAME TO {table}_away"))
            .expect("renamed");
        assert_eq!(a.beat("h1"), ServiceReply::Nothing, "{table}");
        db.run(&format!("ALTER TABLE {table}_away RENAME TO {table}"))
            .expect("renamed back");
        assert_eq!(orders(&db), vec![], "{table}");
    }
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
}

/// Proves: a service a host reports absent settles no order of the host, and bakes nothing: its
/// order stays in the rows until the host runs the version.
///
/// Catches: an order that an absent copy of the version (staged, never run) ends.
#[tokio::test]
async fn an_absent_copy_of_the_version_does_not_end_the_order() {
    let (db, _wall, a, _b) = settled("absent", &["h1"]).await;
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
    let mut staged = daemon("1.0");
    let services = staged.services.as_mut().expect("services");
    services[0] = serde_json::from_value(serde_json::json!(
        {"name": WEB, "version": "2.0", "health": "absent"}
    ))
    .expect("a service beat");
    a.beat_with("h1", staged);
    assert_eq!(orders(&db), vec![("h1".to_owned(), "web".to_owned())]);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM fleet_bakes WHERE service = 'web'"
        ),
        "0"
    );
}
