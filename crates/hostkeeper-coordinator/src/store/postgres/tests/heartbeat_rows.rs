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
pub(super) struct Node {
    pub(super) rig: Rig,
    pub(super) store: Postgres,
}

/// The seconds a test has let pass since the coordinators started, which both read as wall clock
/// and as the time since their start.
const START: u64 = 2_000;

impl Node {
    fn start(db: &TestDatabase, name: &str, wall: &Arc<AtomicU64>, extra: &str) -> Node {
        Self::start_with(db, name, wall, extra, |coordinator, store| {
            coordinator.with_rows(store.fleet_rows(name), rules())
        })
    }

    /// A coordinator over the rows as `serve` makes it from the flags `flags`
    /// (`config::with_rows`).
    fn start_from_flags(
        db: &TestDatabase,
        name: &str,
        wall: &Arc<AtomicU64>,
        flags: &[&str],
    ) -> Node {
        let crate::config::Command::Serve(flags) =
            <crate::config::Cli as clap::Parser>::try_parse_from(
                crate::audit_rotation_tests::serve_args(flags),
            )
            .expect("the flags")
            .command
        else {
            panic!("serve's flags");
        };
        Self::start_with(db, name, wall, "stages = [100]", |coordinator, store| {
            crate::config::with_rows(coordinator, &flags, Some(store))
        })
    }

    fn start_with(
        db: &TestDatabase,
        name: &str,
        wall: &Arc<AtomicU64>,
        extra: &str,
        rows: impl FnOnce(Coordinator, &Postgres) -> Coordinator,
    ) -> Node {
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
        .with_services(services_with(extra));
        rig.coordinator = rows(rig.coordinator, &store);
        rig.at(START);
        Node { rig, store }
    }

    /// Lets the time of both clocks be `elapsed` seconds past the start.
    pub(super) fn at(&self, wall: &AtomicU64, elapsed: u64) {
        wall.store(AT + elapsed, Ordering::SeqCst);
        self.rig.at(START + elapsed);
    }

    pub(super) fn coordinator(&self) -> &Coordinator {
        &self.rig.coordinator
    }

    /// The reply to the beat of the host `host`, which runs 1.0 of both services.
    pub(super) fn beat(&self, host: &str) -> ServiceReply {
        self.beat_with(host, daemon("1.0"))
    }

    /// The reply to the beat `beat` of the host `host`.
    pub(super) fn beat_with(
        &self,
        host: &str,
        beat: hostkeeper_wire::wire::Heartbeat,
    ) -> ServiceReply {
        ask(&self.rig, host, DAEMON, beat)
    }
}

/// Two coordinators on a database in which `web` and `db` are desired at 2.0 (1.0 before).
pub(super) async fn pair(test: &str) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
    pair_with(test, "stages = [100]").await
}

/// [`pair`] over `web` defined with the extra lines `extra`.
pub(super) async fn pair_with(
    test: &str,
    extra: &str,
) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
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
pub(super) async fn settled(
    test: &str,
    hosts: &[&str],
) -> (TestDatabase, Arc<AtomicU64>, Node, Node) {
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

/// Proves: a coordinator that reads the plan at its first stage, and then, before its turn at the
/// gate, finds the other coordinator has entered the second, does not enter it again: the rows say
/// the plan stands at the second, so it does not try to write the stage or its audit line (a try
/// is logged when it fails), and orders the host the second stage lets go.
///
/// Catches: a stage entered twice, with two `rollout_advance` lines, by a coordinator whose
/// memory says the plan is at its first stage while the rows say it is at the second.
#[tokio::test]
async fn a_stage_the_other_coordinator_entered_meanwhile_is_not_entered_again() {
    super::logging();
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
    let advances = "SELECT count(*) FROM audit WHERE action = 'rollout_advance'";
    assert_eq!(count(&db, advances), "0");
    let a = Arc::new(a);
    let first = Arc::clone(&a);
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = Arc::clone(&ran);
    // b has read the plan at its first stage when a's beat bakes h1 and enters the second.
    b.coordinator().pause_before_gate(Arc::new(move || {
        if !once.swap(true, Ordering::SeqCst) {
            first.beat_with("h1", web_at("2.0"));
        }
    }));
    assert_eq!(
        b.beat("h2"),
        update(WEB, "2.0"),
        "the rows say the second stage is reached: h2 is ordered"
    );
    assert!(
        ran.load(Ordering::SeqCst),
        "a entered the stage before b's gate"
    );
    assert!(
        !super::logged("the plan's next stage was not entered"),
        "b did not try to write the stage a had entered"
    );
    assert_eq!(
        count(&db, advances),
        "1",
        "the second stage was entered once"
    );
    assert_eq!(
        count(&db, "SELECT entered FROM rollouts WHERE service = 'web'"),
        "2"
    );
    assert_eq!(count(&db, "SELECT count(*) FROM fleet_orders"), "1");
}

/// Proves: a version set, and a hold made, through one coordinator after the other started are
/// seen by the other at its next beat: with the services held it orders nothing, and with the holds
/// released and 3.0 set it orders 3.0, not the 2.0 it loaded at start.
///
/// Catches: a coordinator that keeps deciding on the desired state and the operations state it
/// loaded at start, ordering a version that was set back or past a hold another coordinator made.
#[tokio::test]
async fn a_hold_and_a_version_set_through_the_other_coordinator_are_seen_at_the_next_beat() {
    let (db, _wall, a, b) = settled("seen", &["h1"]).await;
    for service in [WEB, DB] {
        crate::marks_support::hold(&a.rig, service, crate::marks_support::scope_fleet())
            .expect("held");
    }
    assert_eq!(b.beat("h1"), ServiceReply::Nothing, "the hold is seen");
    assert_eq!(orders(&db), vec![]);
    for service in [WEB, DB] {
        crate::marks_support::unhold(&a.rig, service, crate::marks_support::scope_fleet())
            .expect("released");
    }
    set(&a.rig, WEB, "3.0", 3).await;
    assert_eq!(
        b.beat("h1"),
        update(WEB, "3.0"),
        "the version set through a is ordered by b"
    );
}

/// Proves: a beat whose read of what the other coordinator committed fails, whether the audit
/// cannot be asked or the states cannot be read, still answers, and the coordinator keeps the
/// state it holds; the first beat after the rows are back takes in the other's change.
///
/// Catches: a read of the other coordinator's state that stops the beat when it fails, or that
/// takes in a state it read only in part.
#[tokio::test]
async fn a_read_of_the_others_state_that_fails_leaves_memory_as_it_was() {
    let (db, _wall, a, b) = settled("unread", &["h1", "h2", "h3"]).await;
    let behind = b.coordinator().ops.newest_seq();
    for service in [WEB, DB] {
        crate::marks_support::hold(&a.rig, service, crate::marks_support::scope_fleet())
            .expect("held");
    }
    let ahead = a.coordinator().ops.newest_seq();
    assert!(ahead > behind, "a committed changes b has not read");
    for (table, host) in [("audit", "h1"), ("holds", "h2")] {
        db.run(&format!("ALTER TABLE {table} RENAME TO {table}_away"))
            .expect("renamed");
        b.beat(host);
        db.run(&format!("ALTER TABLE {table}_away RENAME TO {table}"))
            .expect("renamed back");
        assert_eq!(b.coordinator().ops.newest_seq(), behind, "{table}");
    }
    assert_eq!(b.beat("h3"), ServiceReply::Nothing, "the holds are read");
    assert_eq!(b.coordinator().ops.newest_seq(), ahead);
}

/// Proves: a beat that arrives while another decision is inside the gate, after it read the rows
/// and before it wrote its order, is answered with no order and writes none; the decision inside
/// then gives its order, and the beat's next try, with the gate free, gives the second.
///
/// Catches: a read and a write that are not one step: a coordinator that decides between
/// another's read of the rows and its order, which the gate answers with busy.
#[tokio::test]
async fn a_beat_between_another_decisions_read_and_its_order_finds_the_gate_held() {
    let (db, _wall, a, b) = settled("inside", &["h1", "h2"]).await;
    let b = Arc::new(b);
    let other = Arc::clone(&b);
    let said = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inside = Arc::clone(&said);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rows = Arc::clone(&seen);
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = Arc::clone(&ran);
    let name = db.name().to_owned();
    a.coordinator().pause_in_gate(Arc::new(move || {
        if !once.swap(true, Ordering::SeqCst) {
            inside.lock().expect("replies").push(other.beat("h2"));
            let name = name.clone();
            let written: i64 = super::off_runtime(move || {
                super::admin(Some(&name))
                    .query_one("SELECT count(*) FROM hostkeeper.fleet_orders", &[])
                    .expect("count")
                    .get(0)
            });
            rows.lock().expect("orders").push(written);
        }
    }));
    assert_eq!(a.beat("h1"), update(WEB, "2.0"));
    assert!(ran.load(Ordering::SeqCst), "the beat came inside the gate");
    assert_eq!(
        *said.lock().expect("replies"),
        vec![ServiceReply::Nothing],
        "the gate is held"
    );
    assert_eq!(
        *seen.lock().expect("orders"),
        vec![0],
        "the beat inside the gate wrote no order"
    );
    assert_eq!(orders(&db), vec![("h1".to_owned(), "web".to_owned())]);
    assert_eq!(b.beat("h2"), update(WEB, "2.0"));
}

/// Proves: a decision whose read of the hosts succeeded and whose later read (the orders) failed
/// takes none of those hosts in: once the rows are back, the coordinator reads them again, finds
/// the two hosts of the other coordinator that are updating, and orders the third host nothing,
/// where the role lets two update.
///
/// Catches: a coordinator that counts the hosts as read before the decision that read them has
/// finished, never reads them again, and orders past the role's slots.
#[tokio::test]
async fn hosts_read_by_a_decision_that_failed_are_read_again() {
    let (db, wall, a, b) = pair("lost-hosts").await;
    for host in ["h1", "h2", "h3"] {
        a.beat(host);
    }
    a.at(&wall, 31);
    b.at(&wall, 31);
    for host in ["h1", "h2"] {
        assert_eq!(a.beat(host), update(WEB, "2.0"));
        a.beat_with(host, daemon_running("1.0", true));
    }
    assert_eq!(orders(&db), vec![], "the orders ended: the hosts update");
    // The beat's own statement does not read this column; the decision's read of the orders does.
    db.run("ALTER TABLE fleet_orders RENAME COLUMN try TO try_away")
        .expect("renamed");
    assert_eq!(b.beat("h3"), ServiceReply::Nothing);
    db.run("ALTER TABLE fleet_orders RENAME COLUMN try_away TO try")
        .expect("renamed back");
    assert_eq!(
        b.beat("h3"),
        ServiceReply::Nothing,
        "the two updating hosts hold the role's slots"
    );
    assert_eq!(orders(&db), vec![]);
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

/// Proves: an install the fleet in memory would order is refused by the rows: `db` has a halt
/// row written straight into the rows, which the fleet in memory does not hold, so the host that runs `web`
/// at 2.0 and lacks `db` is ordered nothing and no order row is written; with the halt row gone
/// its next beat is ordered the install.
///
/// Catches: an install ordered from memory past a halt the rows hold.
#[tokio::test]
async fn an_install_the_rows_refuse_is_not_ordered() {
    use crate::assign_support::{order_to, why_of};
    use crate::marks_support::runs;

    let (db, _wall, a, _b) = settled("install-halted", &["h3"]).await;
    db.run(
        "INSERT INTO halts (service, node_id, check_name, target, origin) \
         VALUES ('db', 'h9', 'health', '2.0', 'updater')",
    )
    .expect("a halt row");
    assert_eq!(order_to(&a.rig, "h3", vec![runs(WEB, "2.0")]), None);
    assert_eq!(
        why_of(&a.rig, DB, "h3"),
        None,
        "the fleet in memory would order the install"
    );
    assert_eq!(orders(&db), vec![]);
    db.run("DELETE FROM halts").expect("the halt row gone");
    assert!(matches!(
        order_to(&a.rig, "h3", vec![runs(WEB, "2.0")]),
        Some(crate::orders::Order::Install { .. })
    ));
    assert_eq!(orders(&db), vec![("h3".to_owned(), "db".to_owned())]);
}

/// Proves: an uninstall whose order row cannot be written orders nothing and leaves no row; with
/// the rows writable again the host's next beat is ordered the uninstall, and its row names it.
///
/// Catches: an uninstall given to a host when the rows that keep other coordinators from ordering
/// the same host did not take it.
#[tokio::test]
async fn an_uninstall_the_rows_do_not_take_is_not_ordered() {
    use crate::assign_support::{order_to, unassign};
    use crate::marks_support::runs;

    let (db, wall, a, b) = settled("uninstall-rows", &["h1"]).await;
    let both = || vec![runs(WEB, "1.0"), runs(DB, "2.0")];
    unassign(&a.rig, WEB, "", "daemon", true).expect("taken off the daemons");
    a.at(&wall, 32);
    b.at(&wall, 32);
    db.run(
        "ALTER TABLE fleet_orders ADD CONSTRAINT no_uninstall \
         CHECK (uninstall_id IS NULL) NOT VALID",
    )
    .expect("uninstalls refused");
    assert_eq!(order_to(&a.rig, "h1", both()), None);
    assert_eq!(orders(&db), vec![]);
    db.run("ALTER TABLE fleet_orders DROP CONSTRAINT no_uninstall")
        .expect("uninstalls taken");
    assert!(matches!(
        order_to(&a.rig, "h1", both()),
        Some(crate::orders::Order::Uninstall { .. })
    ));
    assert_eq!(
        count(
            &db,
            "SELECT uninstall_id FROM fleet_orders WHERE host_id = 'h1'"
        ),
        "uninstall-1"
    );
}

/// A coordinator made from `serve`'s flags `flags` on a database in which `web` and `db` are
/// desired at 2.0 (1.0 before).
async fn from_flags(test: &str, flags: &[&str]) -> (TestDatabase, Arc<AtomicU64>, Node) {
    let db = TestDatabase::new();
    let wall = Arc::new(AtomicU64::new(AT));
    let node = Node::start_from_flags(&db, test, &wall, flags);
    for service in [WEB, DB] {
        set(&node.rig, service, "1.0", 1).await;
        set(&node.rig, service, "2.0", 2).await;
    }
    (db, wall, node)
}

/// A heartbeat of a host that runs both services at 2.0, sent at `sent_unix` by its own clock.
fn current_sent_at(sent_unix: u64) -> hostkeeper_wire::wire::Heartbeat {
    let mut beat = daemon("2.0");
    beat.agent_features = daemon("1.0").agent_features;
    beat.sent_unix = Some(sent_unix);
    beat
}

/// Proves: `--beat-reorder-secs 5`, given to `serve`, is the window of the rows' beat: a beat
/// older by the host's clock than the stored one is dropped while the stored row is 2 seconds
/// old, and believed once it is 9 seconds old (the default of 30 would still drop it).
///
/// Catches: the flag not reaching the rows (`config::with_rows`), so a host whose clock stepped
/// back is ignored for the default time whatever the operator set.
#[tokio::test]
async fn the_beat_reorder_flag_sets_how_long_a_late_beat_is_dropped() {
    let (db, wall, a) = from_flags("reorder-flag", &["--beat-reorder-secs", "5"]).await;
    let sent =
        |db: &TestDatabase| count(db, "SELECT sent_unix FROM fleet_hosts WHERE host_id = 'h2'");
    a.at(&wall, 31);
    a.beat_with("h2", current_sent_at(1000));
    assert_eq!(sent(&db), "1000");
    a.at(&wall, 33);
    a.beat_with("h2", current_sent_at(900));
    assert_eq!(sent(&db), "1000", "a late beat within 5 seconds is dropped");
    a.at(&wall, 40);
    a.beat_with("h2", current_sent_at(900));
    assert_eq!(sent(&db), "900", "past 5 seconds the host is believed");
}

/// Proves: `--queue-idle-secs 100`, given to `serve`, is how long a service keeps its place in
/// the queue without asking: a service that is gone (no host asks for it), asked before `db`,
/// holds `db` back while its place is less than 100 seconds idle, and `db` is ordered once the
/// place is 101 seconds idle (the default of 3600 would still hold it).
///
/// Catches: the flag not reaching the rows' admission (`config::with_rows`), so a service that is
/// gone holds the services behind it for the default hour whatever the operator set.
#[tokio::test]
async fn the_queue_idle_flag_sets_how_long_an_idle_place_holds_the_queue() {
    let (db, wall, a) = from_flags("queue-flag", &["--queue-idle-secs", "100"]).await;
    db.run(&format!(
        "INSERT INTO rollout_queue (service, seq, asked_unix) VALUES ('gone', 0, {})",
        AT + 31
    ))
    .expect("a place asked at 31");
    // The first beat of the fleet is in the quiet time; the host runs `web` at 2.0 and `db` at
    // 1.0, so the fleet in memory orders `db`, and the queue holds it back.
    a.at(&wall, 31);
    assert_eq!(a.beat_with("h1", web_at("2.0")), ServiceReply::Nothing);
    for elapsed in [70, 110] {
        a.at(&wall, elapsed);
        assert_eq!(
            a.beat_with("h1", web_at("2.0")),
            ServiceReply::Nothing,
            "at {elapsed}: the place was asked {} seconds ago",
            elapsed - 31
        );
    }
    assert_eq!(orders(&db), vec![]);
    a.at(&wall, 132);
    assert_eq!(a.beat_with("h1", web_at("2.0")), update(DB, "2.0"));
    assert_eq!(orders(&db), vec![("h1".to_owned(), "db".to_owned())]);
}
