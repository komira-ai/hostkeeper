use std::time::Duration;

use super::super::{Fleet, QUIET_START};
use super::{placed, unplaced};
use crate::store::decision::{BakeRow, OrderRow, Rows, TryRow, Writes};
use crate::testutil::{id, secs, version};

/// The database's clock when the rows below were read.
const NOW_UNIX: i64 = 2_000_000_000;

fn order(host: &str, target: &str, ago: i64, order_try: i64) -> OrderRow {
    OrderRow {
        host_id: host.to_owned(),
        service: "web".to_owned(),
        target: target.to_owned(),
        uninstall_id: None,
        at_unix: NOW_UNIX - ago,
        order_try,
    }
}

fn rows(orders: Vec<OrderRow>) -> Rows {
    Rows {
        now_unix: NOW_UNIX,
        quiet_until: NOW_UNIX - 1,
        orders,
        ..Rows::default()
    }
}

/// Proves: a time of the database's clock is placed as far from the coordinator's `now` as it is
/// from the database's, ahead or behind, and placed back where it was; a time before the
/// coordinator's start is its start.
/// Catches: a quiet time ahead of the rows' clock placed behind `now` (the gap's quiet would
/// never hold), or an order's time placed back off by the offset of the two clocks.
#[test]
fn a_rows_time_is_placed_on_the_coordinators_clock_and_back() {
    let now = secs(1000);
    assert_eq!(placed(NOW_UNIX + 30, now, NOW_UNIX), secs(1030));
    assert_eq!(placed(NOW_UNIX - 30, now, NOW_UNIX), secs(970));
    assert_eq!(placed(NOW_UNIX - 5000, now, NOW_UNIX), Duration::ZERO);
    assert_eq!(unplaced(secs(970), now, NOW_UNIX), NOW_UNIX - 30);
    assert_eq!(
        unplaced(secs(1030), now, NOW_UNIX),
        NOW_UNIX,
        "a time ahead is now"
    );
}

/// Proves: the rows' orders, bakes and tries become the fleet's; an order memory held that the
/// rows do not is no longer outstanding and the next order of it numbers past its try; a row
/// that does not read is left out.
/// Catches: a fleet that keeps an order another coordinator's beat ended (its slot held for
/// good), forgets the try of an order it dropped (the host refuses the next order as stale), or
/// fails on a row it cannot parse, or answers that row as read (a decision would then end an
/// order a newer release gave).
#[test]
fn the_rows_orders_bakes_and_tries_become_the_fleets() {
    let now = secs(1000);
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.order("web", id("h4"), version("1.0"), now);
    let held_try = fleet.next_try("web", &id("h9"));
    let mut read = rows(vec![
        order("h1", "2.0", 10, 3),
        order("not a host!", "2.0", 10, 1),
        order("h2", "not a version", 10, 1),
    ]);
    read.bakes = vec![
        BakeRow {
            host_id: "h2".to_owned(),
            service: "web".to_owned(),
            target: "2.0".to_owned(),
            beats: 4,
            at_unix: NOW_UNIX - 5,
        },
        BakeRow {
            host_id: "h3".to_owned(),
            service: "web".to_owned(),
            target: "bad version".to_owned(),
            beats: 4,
            at_unix: NOW_UNIX - 5,
        },
    ];
    read.tries = vec![
        TryRow {
            host_id: "h3".to_owned(),
            service: "web".to_owned(),
            order_try: 7,
        },
        TryRow {
            host_id: "not a host!".to_owned(),
            service: "web".to_owned(),
            order_try: 9,
        },
    ];
    let kept = fleet.take_rows(&read, now);
    assert_eq!(
        kept,
        [order("h1", "2.0", 10, 3)],
        "only the rows it read are answered"
    );
    let given = fleet.outstanding("web", &id("h1"), now);
    assert_eq!(
        given.map(|o| (o.order_try, o.target)),
        Some((3, version("2.0")))
    );
    assert!(
        !fleet.is_ordered("web", &id("h4"), now),
        "the rows hold no order of h4"
    );
    assert_eq!(
        fleet.next_try("web", &id("h4")),
        2,
        "past the try of h4's order"
    );
    assert_eq!(fleet.next_try("web", &id("h3")), 8, "past the rows' try");
    assert_eq!(fleet.next_try("web", &id("h9")), held_try);
    assert_eq!(fleet.bake_beats("web", &id("h2"), &version("2.0")), 4);
    assert!(
        !fleet.is_ordered("web", &id("h2"), now),
        "a row that does not read"
    );
}

/// Proves: once the rows were read, their quiet time decides whether the fleet is quiet, not
/// the coordinator's own start; before, the start does.
/// Catches: a coordinator that started a moment ago quiet for its own start while the fleet's
/// beats show no gap (a deploy would quiet the fleet), or one that never reads the gap's quiet.
#[test]
fn the_rows_quiet_time_replaces_the_fleets_own_start() {
    let mut fleet = Fleet::new(secs(1000), QUIET_START);
    let now = secs(1001);
    assert!(fleet.is_quiet(now), "within its own start, before the rows");
    fleet.take_rows(&rows(Vec::new()), now);
    assert!(!fleet.is_quiet(now), "the rows show no gap");
    let mut gap = rows(Vec::new());
    gap.quiet_until = NOW_UNIX + 20;
    fleet.take_rows(&gap, now);
    assert!(fleet.is_quiet(now + secs(19)));
    assert!(!fleet.is_quiet(now + secs(20)));
    fleet.store_lost = true;
    assert!(
        fleet.is_quiet(now + secs(20)),
        "a lost store is quiet whatever the rows say"
    );
}

/// Proves: what a decision writes back is the difference between the orders it read and those
/// memory holds: an order replaced is written over the row as read, a new one inserted, an aged
/// or ended one deleted with its try kept, and a try memory raised kept; nothing for an order
/// unchanged.
/// Catches: a decision that rewrites every order (a beat's release of a slot undone), forgets to
/// delete an aged order (its slot held for good), or loses the try of an order that ended.
#[test]
fn a_decision_writes_back_only_the_orders_that_differ() {
    let now = secs(1000);
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    let read = vec![
        order("h1", "2.0", 10, 3),
        order("h5", "2.0", 20, 2),
        order("h6", "2.0", 130, 4),
        order("h7", "2.0", 30, 1),
        order("h8", "2.0", 30, 1),
    ];
    fleet.take_rows(&rows(read.clone()), now);
    let sent = fleet.sent_tries();
    fleet.order("web", id("h1"), version("3.0"), now);
    fleet.order("web", id("h2"), version("2.0"), now);
    fleet.drop_in("web", &id("h7"));
    fleet.drop_in("web", &id("h8"));
    fleet.order("db", id("h8"), version("5.0"), now);
    let writes = fleet.writes_since(&read, &sent, now, NOW_UNIX);
    let mut h1 = order("h1", "3.0", 0, 4);
    h1.at_unix = NOW_UNIX;
    let mut h2 = order("h2", "2.0", 0, 1);
    h2.at_unix = NOW_UNIX;
    let mut h8 = order("h8", "5.0", 0, 1);
    (h8.at_unix, h8.service) = (NOW_UNIX, "db".to_owned());
    let ended_try = |host: &str, order_try| TryRow {
        host_id: host.to_owned(),
        service: "web".to_owned(),
        order_try,
    };
    assert_eq!(
        writes,
        Writes {
            ended: vec![order("h6", "2.0", 130, 4), order("h7", "2.0", 30, 1)],
            replaced: vec![
                (order("h1", "2.0", 10, 3), h1),
                (order("h8", "2.0", 30, 1), h8)
            ],
            given: vec![h2],
            tries: vec![ended_try("h6", 4), ended_try("h7", 1), ended_try("h1", 3)],
        }
    );
}

/// Proves: a second order of one host in memory is written as given too, so the host's key
/// refuses the decision rather than one of the two orders being dropped unseen.
/// Catches: a write-back that keeps the first order of a host and silently drops a second one
/// the reply still carries.
#[test]
fn a_second_order_of_one_host_is_written_for_its_key_to_refuse() {
    let now = secs(1000);
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.take_rows(&rows(vec![order("h1", "2.0", 10, 3)]), now);
    let sent = fleet.sent_tries();
    fleet.order("db", id("h1"), version("5.0"), now);
    let writes = fleet.writes_since(&[order("h1", "2.0", 10, 3)], &sent, now, NOW_UNIX);
    assert_eq!(writes.given.len(), 1);
    assert_eq!(writes.given[0].service, "db");
    assert!(writes.replaced.is_empty() && writes.ended.is_empty());
}

/// Proves: a host's row is taken in when it is new or changed, and not again while unchanged.
/// Catches: a fleet that notes every row of every host at every decision (the cost grows with
/// the fleet), or never notes a row another coordinator changed.
#[test]
fn a_hosts_row_is_taken_in_once_until_it_changes() {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    assert!(fleet.row_is_new("h1", 10, 9, "a"));
    assert!(!fleet.row_is_new("h1", 10, 9, "a"));
    assert!(fleet.row_is_new("h1", 25, 24, "a"));
    assert!(fleet.row_is_new("h1", 25, 24, "b"));
}

/// Proves: an order row older than the coordinator's own start, taken in and left as it was, is
/// written back as nothing: its time is the row's own, not the start it was placed at.
/// Catches: a decision that makes every old order younger at each beat after a restart (an
/// order that should age out kept, and its row rewritten every beat).
#[test]
fn an_order_older_than_the_coordinators_start_keeps_its_own_time() {
    let now = secs(5);
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    let read = fleet.take_rows(&rows(vec![order("h1", "2.0", 60, 3)]), now);
    let sent = fleet.sent_tries();
    assert_eq!(
        fleet.writes_since(&read, &sent, now, NOW_UNIX),
        Writes::default()
    );
}
