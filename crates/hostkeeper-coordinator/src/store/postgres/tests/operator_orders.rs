//! The operator's queued orders over the fleet's rows (`service_operator`), with two coordinators on
//! one database: an `abort` and a `clear_record` queued through one are sent, once, by whichever
//! hears the host's next heartbeat, and the rows that keep them are the schema's.
//!
//! No test sleeps or measures time. A failure of the rows is made by taking a table away or by a
//! trigger that refuses a delete, never by a race.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};

use super::coordinator::DAEMON;
use super::heartbeat_rows::{Node, pair};
use super::{TestDatabase, with_session};
use crate::marks_support::{DB, NEW_DAEMON, WEB, ask_json, ask_typed, beat_of, operator, runs};
use crate::orders::Order;
use crate::store::postgres::fleet_rows::{
    OperatorKind, OperatorOrderRow, operator_orders_of, queue_operator_order, take_operator_order,
};

/// The feature of a daemon that takes a `clear_record` order.
const CLEAR_RECORD: &str = crate::clear_record::FEATURE;

/// A heartbeat of a host that runs both services at 2.0 (the desired version, so the rollout
/// gate has nothing to order) and announces the features of a daemon of today and `extra`.
fn host_at_2(extra: &[&str]) -> Heartbeat {
    let mut features = NEW_DAEMON.to_vec();
    features.extend_from_slice(extra);
    beat_of(&features, vec![runs(WEB, "2.0"), runs(DB, "2.0")])
}

/// A host that takes an `abort` and a `clear_record`.
fn clearer() -> Heartbeat {
    host_at_2(&[CLEAR_RECORD])
}

/// The reply and the operator's order to a heartbeat of `host` heard by `node`, over a transport
/// that carries typed orders.
fn typed(node: &Node, host: &str, beat: Heartbeat) -> (ServiceReply, Option<Order>) {
    ask_typed(&node.rig, host, DAEMON, beat)
}

fn abort_of(node: &str) -> proto::AbortRequest {
    proto::AbortRequest {
        service: WEB.to_owned(),
        node_id: node.to_owned(),
        reason: "stop it".to_owned(),
    }
}

fn clear_of(node: &str, service: &str) -> proto::ClearRecordRequest {
    proto::ClearRecordRequest {
        node_id: node.to_owned(),
        service: service.to_owned(),
    }
}

/// The order id `node` answers an `abort` of `web` on `host` with.
fn abort(node: &Node, host: &str) -> String {
    let answer = node
        .coordinator()
        .abort(&operator(), abort_of(host))
        .expect("an abort is queued");
    format!("abort-{}", answer.entry.expect("its audit entry").seq)
}

/// The order id `node` answers a `clear_record` of `service` on `host` with.
fn clear_service(node: &Node, host: &str, service: &str) -> String {
    node.coordinator()
        .clear_record(&operator(), &clear_of(host, service))
        .expect("a clear-record is queued")
        .order_id
}

/// [`clear_service`] of `web`.
fn clear(node: &Node, host: &str) -> String {
    clear_service(node, host, WEB)
}

/// The rows of the queued orders: host, kind, service and order id.
fn rows(db: &TestDatabase) -> Vec<[String; 4]> {
    db.rows(
        "SELECT host_id, kind, service, order_id FROM fleet_operator_orders \
         ORDER BY host_id, kind, service",
    )
    .into_iter()
    .map(|row| {
        let mut cells = row.into_iter().map(|cell| cell.expect("a cell"));
        std::array::from_fn(|_| cells.next().expect("four cells"))
    })
    .collect()
}

fn row(host: &str, kind: &str, order_id: &str) -> [String; 4] {
    row_of(host, kind, WEB, order_id)
}

fn row_of(host: &str, kind: &str, service: &str, order_id: &str) -> [String; 4] {
    [host, kind, service, order_id].map(str::to_owned)
}

fn abort_order(order_id: &str) -> Option<Order> {
    Some(Order::Abort {
        service: WEB.to_owned(),
        order_id: order_id.to_owned(),
    })
}

fn clear_order(order_id: &str) -> Option<Order> {
    clear_order_of(WEB, order_id)
}

fn clear_order_of(service: &str, order_id: &str) -> Option<Order> {
    Some(Order::ClearRecord {
        service: service.to_owned(),
        order_id: order_id.to_owned(),
    })
}

fn audit_lines(db: &TestDatabase, action: &str) -> String {
    db.rows(&format!(
        "SELECT count(*) FROM audit WHERE action = '{action}'"
    ))
    .remove(0)
    .remove(0)
    .expect("a count")
}

/// Proves: an `abort` the operator queued through coordinator A is a row; the host's next beat,
/// heard by coordinator B, is answered the abort with A's order id and the row is gone; the beats
/// after it, heard by B and by A (whose memory still held the abort it queued), are sent nothing.
///
/// Catches: an order queued in the memory of the coordinator that took the call, which a host
/// beating the other coordinator never gets, and one that A sends again from stale memory after B
/// sent it.
#[tokio::test]
async fn an_abort_queued_through_one_coordinator_is_sent_once_by_the_other() {
    let (db, _wall, a, b) = pair("abort-across").await;
    assert_eq!(
        typed(&a, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, None)
    );
    let order_id = abort(&a, "h1");
    assert_eq!(rows(&db), vec![row("h1", "abort", &order_id)]);

    assert_eq!(
        typed(&b, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, abort_order(&order_id))
    );
    assert_eq!(
        rows(&db),
        Vec::<[String; 4]>::new(),
        "sending deletes the row"
    );
    assert_eq!(
        typed(&b, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        typed(&a, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, None),
        "the coordinator that queued it does not send it again"
    );
}

/// Proves: a `clear_record` queued through coordinator A is a row, answered the order id
/// `clear-record-<n>` of its audit line; coordinator B sends it at the host's next beat, once, and
/// A, whose memory still held it, sends it no more.
///
/// Catches: a clear-record kept in the memory of the coordinator that took the call, and one sent
/// by both coordinators.
#[tokio::test]
async fn a_clear_record_queued_through_one_coordinator_is_sent_once_by_the_other() {
    let (db, _wall, a, b) = pair("clear-across").await;
    typed(&a, "h1", clearer());
    let order_id = clear(&a, "h1");
    assert!(order_id.starts_with("clear-record-"), "{order_id}");
    assert_eq!(rows(&db), vec![row("h1", "clear_record", &order_id)]);

    assert_eq!(
        typed(&b, "h1", clearer()),
        (ServiceReply::Nothing, clear_order(&order_id))
    );
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
    assert_eq!(typed(&b, "h1", clearer()), (ServiceReply::Nothing, None));
    assert_eq!(typed(&a, "h1", clearer()), (ServiceReply::Nothing, None));
}

/// Proves: asking `Hosts.ClearRecord` again through coordinator B while A's order waits answers
/// A's order id, writes no second audit line and leaves one row; once it is sent, asking again
/// queues a new order with a new id and a second audit line.
///
/// Catches: a check of the order that reads only the memory of the coordinator that is asked, so
/// each coordinator queues and audits an order of its own.
#[tokio::test]
async fn asking_clear_record_again_through_the_other_coordinator_queues_nothing_more() {
    let (db, _wall, a, b) = pair("clear-again").await;
    typed(&a, "h1", clearer());
    typed(&b, "h1", clearer());
    let first = clear(&a, "h1");
    assert_eq!(audit_lines(&db, "record_cleared"), "1");
    assert_eq!(clear(&b, "h1"), first, "B answers A's order");
    assert_eq!(clear(&a, "h1"), first);
    assert_eq!(
        audit_lines(&db, "record_cleared"),
        "1",
        "no second audit line"
    );
    assert_eq!(rows(&db), vec![row("h1", "clear_record", &first)]);

    assert_eq!(typed(&a, "h1", clearer()).1, clear_order(&first));
    // A beat on the JSON route, which sends no order, takes in the audit line A wrote.
    ask_json(&b.rig, "h1", DAEMON, clearer());
    let second = clear(&b, "h1");
    assert_ne!(second, first, "a sent order is not asked again");
    assert_eq!(audit_lines(&db, "record_cleared"), "2");
    assert_eq!(rows(&db), vec![row("h1", "clear_record", &second)]);
}

/// Proves: an `abort` asked twice, once through each coordinator, is one row with the first order
/// id, and the host is sent that order once.
///
/// Catches: an abort queued twice, or a second ask that replaces the order id of the first.
#[tokio::test]
async fn an_abort_asked_through_both_coordinators_is_one_order_with_the_first_id() {
    let (db, _wall, a, b) = pair("abort-twice").await;
    typed(&a, "h1", host_at_2(&[]));
    typed(&b, "h1", host_at_2(&[]));
    let first = abort(&a, "h1");
    // A beat on the JSON route, which sends no order, takes in the audit line A wrote.
    ask_json(&b.rig, "h1", DAEMON, host_at_2(&[]));
    let second = abort(&b, "h1");
    assert_ne!(first, second, "each call has its own audit line");
    assert_eq!(rows(&db), vec![row("h1", "abort", &first)]);
    assert_eq!(typed(&b, "h1", host_at_2(&[])).1, abort_order(&first));
    assert_eq!(typed(&a, "h1", host_at_2(&[])).1, None);
}

/// Proves: an abort and two clear-records (of `db` and of `web`) waiting for one host are sent one
/// to a beat, the abort first and then the clear-records by service name, whichever coordinators
/// queued them; the clear-record of `web`, asked of a coordinator that finds the others' rows (an
/// abort of the same service, a clear-record of another service), is a new order; the JSON route,
/// which carries no operator order, sends none and leaves every row.
///
/// Catches: a clear-record sent before an abort, two orders in one answer, a row deleted by a beat
/// whose answer cannot carry the order, and an ask answered with the order of another kind or of
/// another service.
#[tokio::test]
async fn an_abort_goes_before_a_clear_record_and_each_beat_sends_one() {
    let (db, _wall, a, b) = pair("both-kinds").await;
    typed(&a, "h1", clearer());
    typed(&b, "h1", clearer());
    let db_cleared = clear_service(&a, "h1", DB);
    let aborted = abort(&a, "h1");
    // A beat on the JSON route sends no order, and takes in the audit lines A wrote.
    assert_eq!(
        ask_json(&b.rig, "h1", DAEMON, clearer()),
        ServiceReply::Nothing
    );
    let web_cleared = clear(&b, "h1");
    assert_ne!(web_cleared, db_cleared);
    assert_eq!(rows(&db).len(), 3, "the JSON route sends no operator order");

    assert_eq!(typed(&b, "h1", clearer()).1, abort_order(&aborted));
    assert_eq!(
        rows(&db),
        vec![
            row_of("h1", "clear_record", DB, &db_cleared),
            row("h1", "clear_record", &web_cleared)
        ]
    );
    assert_eq!(
        typed(&a, "h1", clearer()).1,
        clear_order_of(DB, &db_cleared)
    );
    assert_eq!(typed(&b, "h1", clearer()).1, clear_order(&web_cleared));
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
    assert_eq!(typed(&a, "h1", clearer()).1, None);
}

/// Proves: an abort for a host whose daemon did not announce `abort-v1` stays a row through every
/// beat of both coordinators, and is sent once the daemon announces the feature.
///
/// Catches: a row deleted, or an order sent, to a daemon that cannot read it.
#[tokio::test]
async fn an_abort_for_a_daemon_that_did_not_announce_it_stays_queued_in_the_rows() {
    let (db, _wall, a, b) = pair("abort-feature").await;
    let old = || beat_of(&["heartbeat-v2"], vec![runs(WEB, "2.0"), runs(DB, "2.0")]);
    typed(&a, "h1", old());
    let order_id = abort(&a, "h1");
    for node in [&b, &a] {
        assert_eq!(typed(node, "h1", old()), (ServiceReply::Nothing, None));
        assert_eq!(rows(&db), vec![row("h1", "abort", &order_id)]);
    }
    assert_eq!(typed(&b, "h1", host_at_2(&[])).1, abort_order(&order_id));
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
}

/// Proves: when the rows cannot be written the operator's calls still answer: the abort is queued,
/// the clear-record answers its order id, and asking the clear-record again, with the rows out of
/// reach and then back, answers the same id and audits it once. A beat decided here while the rows
/// are still out of reach sends nothing and keeps the orders. The coordinator that took the
/// calls writes both rows at the next beat it decides under the gate, whichever host that beat is
/// of, and the other coordinator then sends them, one to a beat; the rows are not written twice.
///
/// Catches: an order lost because its row could not be written when it was queued, an abort
/// refused for the store, a second clear-record queued while the first waits unwritten, and orders
/// written again by every later beat.
#[tokio::test]
async fn an_order_whose_row_cannot_be_written_waits_for_the_next_gated_beat_here() {
    super::logging();
    let (db, _wall, a, b) = pair("unwritten").await;
    typed(&a, "h1", clearer());
    typed(&b, "h1", clearer());
    db.run("ALTER TABLE fleet_operator_orders RENAME TO fleet_operator_orders_away")
        .expect("renamed");
    let aborted = abort(&a, "h1");
    let cleared = clear(&a, "h1");
    assert_eq!(
        clear(&a, "h1"),
        cleared,
        "asked again with the rows out of reach"
    );
    assert_eq!(
        typed(&a, "h2", clearer()),
        (ServiceReply::Nothing, None),
        "a decision that cannot write them sends nothing, and keeps them"
    );
    db.run("ALTER TABLE fleet_operator_orders_away RENAME TO fleet_operator_orders")
        .expect("renamed back");
    assert_eq!(rows(&db), Vec::<[String; 4]>::new(), "nothing was written");
    assert_eq!(
        clear(&a, "h1"),
        cleared,
        "the order waits in the memory of the coordinator that took the call, and is not asked twice"
    );
    assert_eq!(audit_lines(&db, "record_cleared"), "1");
    assert_eq!(
        typed(&b, "h1", clearer()),
        (ServiceReply::Nothing, None),
        "the other coordinator cannot send what is not in the rows"
    );

    // A beat of another host, decided here, writes them.
    assert_eq!(typed(&a, "h2", clearer()), (ServiceReply::Nothing, None));
    assert_eq!(
        rows(&db),
        vec![
            row("h1", "abort", &aborted),
            row("h1", "clear_record", &cleared)
        ]
    );
    assert_eq!(typed(&b, "h1", clearer()).1, abort_order(&aborted));
    assert_eq!(typed(&b, "h1", clearer()).1, clear_order(&cleared));
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
    // Written once: a later beat here does not write what B has sent.
    assert_eq!(typed(&a, "h2", clearer()), (ServiceReply::Nothing, None));
    assert_eq!(typed(&a, "h1", clearer()), (ServiceReply::Nothing, None));
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
}

/// Proves: a decision whose transaction fails after it took the order from the rows (the delete
/// is refused) sends nothing and leaves the row, and the host's next beat, with the rows back, is
/// sent the order: the coordinator's memory, which gave the order up, is not what decides.
///
/// Catches: an order sent in an answer whose transaction rolled back (sent twice), and an order
/// lost with the memory of a decision that failed.
#[tokio::test]
async fn a_decision_whose_transaction_fails_leaves_the_row_and_the_order_goes_later() {
    super::logging();
    let (db, _wall, a, b) = pair("failed-decision").await;
    typed(&a, "h1", host_at_2(&[]));
    let order_id = abort(&a, "h1");
    db.run(
        "CREATE FUNCTION refuse_delete() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'no delete'; END $$; \
         CREATE TRIGGER refuse BEFORE DELETE ON fleet_operator_orders \
         FOR EACH ROW EXECUTE FUNCTION refuse_delete()",
    )
    .expect("deletes are refused");
    assert_eq!(
        typed(&b, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(rows(&db), vec![row("h1", "abort", &order_id)]);
    db.run("DROP TRIGGER refuse ON fleet_operator_orders")
        .expect("deletes are taken");
    assert_eq!(typed(&b, "h1", host_at_2(&[])).1, abort_order(&order_id));
    assert_eq!(rows(&db), Vec::<[String; 4]>::new());
}

/// Proves: a decision that cannot read the rows of the operator's orders (the table is out of
/// reach) sends the host nothing, and the host's next beat, with the rows back, is sent the order
/// that waited: nothing was lost by the failed decision.
///
/// Catches: an order sent, or dropped, by a decision that did not read what the rows hold.
#[tokio::test]
async fn a_decision_that_cannot_read_the_operator_orders_sends_nothing() {
    super::logging();
    let (db, _wall, a, b) = pair("unreadable").await;
    typed(&a, "h1", host_at_2(&[]));
    let order_id = abort(&a, "h1");
    db.run("ALTER TABLE fleet_operator_orders RENAME TO fleet_operator_orders_away")
        .expect("renamed");
    assert_eq!(
        typed(&b, "h1", host_at_2(&[])),
        (ServiceReply::Nothing, None)
    );
    db.run("ALTER TABLE fleet_operator_orders_away RENAME TO fleet_operator_orders")
        .expect("renamed back");
    assert_eq!(rows(&db), vec![row("h1", "abort", &order_id)]);
    assert_eq!(typed(&b, "h1", host_at_2(&[])).1, abort_order(&order_id));
}

/// Proves: the rows of the queued orders as a session reads and writes them: queueing answers the
/// order id that stands (the first), one row for each host, kind and service; a host's orders are
/// read aborts first and then by service name; taking one deletes that row alone and answers how
/// many were deleted.
///
/// Catches: a second order that replaces the first, a read that mixes hosts or sends a
/// clear-record before an abort, and a take that deletes more than the order it names.
#[test]
fn the_rows_queue_one_order_for_a_host_kind_and_service_and_read_in_the_order_sent() {
    let db = TestDatabase::new();
    let (_store, _) = db.open("operator-order-rows").expect("the store opens");
    let (answers, read, taken, left) = with_session(&db, |session| {
        let mut answers = Vec::new();
        for (host, kind, service, id) in [
            ("h1", OperatorKind::ClearRecord, "web", "clear-record-3"),
            ("h1", OperatorKind::Abort, "web", "abort-4"),
            ("h1", OperatorKind::Abort, "db", "abort-5"),
            ("h1", OperatorKind::Abort, "web", "abort-6"),
            ("h2", OperatorKind::Abort, "web", "abort-7"),
        ] {
            answers.push(queue_operator_order(session, host, kind, service, id).expect("queued"));
        }
        let read = operator_orders_of(session, "h1").expect("read");
        let taken = (
            take_operator_order(session, "h1", OperatorKind::Abort, "web").expect("taken"),
            take_operator_order(session, "h1", OperatorKind::Abort, "web").expect("again"),
        );
        let left = operator_orders_of(session, "h1").expect("read");
        (answers, read, taken, left)
    });
    assert_eq!(
        answers,
        ["clear-record-3", "abort-4", "abort-5", "abort-4", "abort-7"]
    );
    let order = |kind, service: &str, order_id: &str| OperatorOrderRow {
        kind,
        service: service.to_owned(),
        order_id: order_id.to_owned(),
    };
    assert_eq!(
        read,
        vec![
            order(OperatorKind::Abort, "db", "abort-5"),
            order(OperatorKind::Abort, "web", "abort-4"),
            order(OperatorKind::ClearRecord, "web", "clear-record-3"),
        ]
    );
    assert_eq!(taken, (1, 0));
    assert_eq!(
        left,
        vec![
            order(OperatorKind::Abort, "db", "abort-5"),
            order(OperatorKind::ClearRecord, "web", "clear-record-3"),
        ]
    );
    assert_eq!(rows(&db).len(), 3, "the other host's order stands");
}

/// Proves: the migration is applied: the table `fleet_operator_orders` is there with its four
/// text columns, logged, keyed by host, kind and service, and it refuses a kind that is neither
/// `abort` nor `clear_record` and a second order of one host, kind and service.
///
/// Catches: a table that is unlogged (an order the operator was told is queued lost with a crash
/// of the database), a missing key that lets two orders of one kind queue, and a kind the
/// coordinator does not read.
#[test]
fn the_operator_orders_table_is_in_the_schema() {
    let db = TestDatabase::new();
    let (_store, _) = db.open("operator-orders-schema").expect("the store opens");
    assert_eq!(
        db.rows(
            "SELECT column_name, data_type, is_nullable FROM information_schema.columns \
             WHERE table_schema = 'hostkeeper' AND table_name = 'fleet_operator_orders' \
             ORDER BY ordinal_position"
        ),
        ["host_id", "kind", "service", "order_id"]
            .map(|name| vec![
                Some(name.to_owned()),
                Some("text".to_owned()),
                Some("NO".to_owned())
            ])
            .to_vec()
    );
    assert_eq!(
        db.rows(
            "SELECT relpersistence FROM pg_class WHERE oid = 'hostkeeper.fleet_operator_orders'::regclass"
        ),
        vec![vec![Some("p".to_owned())]],
        "logged"
    );
    assert_eq!(
        db.rows(
            "SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid \
             AND a.attnum = ANY (i.indkey) WHERE i.indisprimary \
             AND i.indrelid = 'hostkeeper.fleet_operator_orders'::regclass ORDER BY a.attnum"
        ),
        ["host_id", "kind", "service"]
            .map(|name| vec![Some(name.to_owned())])
            .to_vec()
    );
    db.run("INSERT INTO fleet_operator_orders VALUES ('h1', 'abort', 'web', 'abort-1')")
        .expect("an abort");
    assert!(
        db.run("INSERT INTO fleet_operator_orders VALUES ('h1', 'abort', 'web', 'abort-2')")
            .is_err(),
        "one order of a kind for a host and service"
    );
    assert!(
        db.run("INSERT INTO fleet_operator_orders VALUES ('h1', 'force', 'web', 'force-1')")
            .is_err(),
        "a kind the coordinator does not send"
    );
}
