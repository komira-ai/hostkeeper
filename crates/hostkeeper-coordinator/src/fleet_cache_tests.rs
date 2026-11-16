//! The fleet as a cache of the rows ([`crate::fleet`], `fleet_cache.rs`): the orders, the bakes
//! and the quiet time it takes from rows, what it leaves out, and the orders a beat ended. Every
//! time is a number the test passes in.

use std::time::Duration;

use hostkeeper_wire::wire::{Heartbeat, ServiceBeat};
use hostkeeper_wire::{HostId, Platform};

use crate::fleet::{Fleet, ORDER_AGE};
use crate::marks_support::{WEB, runs};
use crate::store::postgres::fleet_rows::{BakeRow, OrderRow};
use crate::testutil::{id, role, version};

/// The Unix time of the fleet's `now` in these tests.
const UNIX: i64 = 1_000_000;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn order(host: &str, target: &str, at_unix: i64, uninstall: Option<&str>) -> OrderRow {
    OrderRow {
        host_id: host.to_owned(),
        service: WEB.to_owned(),
        target: target.to_owned(),
        uninstall_id: uninstall.map(str::to_owned),
        at_unix,
        order_try: 3,
    }
}

/// Proves: the orders of the rows are the fleet's: a host the rows hold an order for is ordered,
/// at the age the row says and at the try it says, an uninstall as one; an order the rows no
/// longer hold is gone, and a row the fleet cannot read (an id that is no host, a version that is
/// none) or that is older than the age of an order is left out.
///
/// Catches: a fleet that keeps an order another coordinator saw ended, or counts one that has
/// aged out, against the slots of a role.
#[test]
fn the_orders_of_the_rows_are_the_orders_of_the_fleet() {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let now = secs(500);
    let host: HostId = id("d1");
    fleet.order(WEB, host.clone(), version("1.5"), secs(490));
    fleet.order(WEB, id("d9"), version("1.5"), secs(490));
    let aged = UNIX - i64::try_from(ORDER_AGE.as_secs()).expect("seconds");
    fleet.sync_orders(
        &[
            order("d1", "2.0", UNIX - 10, None),
            order("d2", "2.0", UNIX - 5, Some("u1")),
            order("d3", "2.0", aged, None),
            order("Not A Host", "2.0", UNIX, None),
            order("d4", "not a version", UNIX, None),
        ],
        now,
        UNIX,
    );
    assert!(fleet.is_ordered(WEB, &host, now));
    assert!(fleet.is_ordered(WEB, &id("d2"), now));
    assert!(!fleet.is_ordered(WEB, &id("d3"), now), "aged out");
    assert!(!fleet.is_ordered(WEB, &id("d4"), now), "no version");
    assert!(!fleet.is_ordered(WEB, &id("d9"), now), "the rows hold none");
    let outstanding = fleet.outstanding(WEB, &host, now).expect("the order");
    assert_eq!(outstanding.target, version("2.0"));
    assert_eq!(outstanding.order_try, 3, "the try of the row");
    fleet.sync_orders(&[], now, UNIX);
    assert!(!fleet.is_ordered(WEB, &host, now));
}

/// Proves: the bakes of the rows are the fleet's bakes, and a bake the fleet had that the rows do
/// not hold is gone; a row the fleet cannot read is left out.
///
/// Catches: a bake count that two coordinators each keep a half of.
#[test]
fn the_bakes_of_the_rows_are_the_bakes_of_the_fleet() {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let bake = |host: &str, target: &str, beats: i64| BakeRow {
        host_id: host.to_owned(),
        service: WEB.to_owned(),
        target: target.to_owned(),
        beats,
        at_unix: UNIX - 3,
    };
    fleet.sync_bakes(
        &[
            bake("d1", "2.0", 4),
            bake("d2", "2.0", i64::MAX),
            bake("Not A Host", "2.0", 1),
            bake("d3", "?", 1),
        ],
        secs(100),
        UNIX,
    );
    assert_eq!(fleet.bake_beats(WEB, &id("d1"), &version("2.0")), 4);
    assert_eq!(fleet.bake_beats(WEB, &id("d1"), &version("3.0")), 0);
    assert_eq!(fleet.bake_beats(WEB, &id("d2"), &version("2.0")), u32::MAX);
    assert_eq!(fleet.bake_beats(WEB, &id("d3"), &version("2.0")), 0);
    fleet.sync_bakes(&[], secs(100), UNIX);
    assert_eq!(fleet.bake_beats(WEB, &id("d1"), &version("2.0")), 0);
}

/// Proves: the fleet is quiet until the time the rows say, and not from its own start; a time the
/// rows say is past, or none (0), is no quiet.
///
/// Catches: a coordinator that starts beside a running one and holds back for its own quiet
/// start, or that orders while the rows say the fleet has not been heard.
#[test]
fn the_quiet_time_of_the_rows_replaces_the_fleets_own() {
    let mut fleet = Fleet::new(secs(100), secs(30));
    assert!(
        fleet.is_quiet(secs(110)),
        "its own start, until the rows are read"
    );
    fleet.sync_quiet(0, secs(110), UNIX);
    assert!(!fleet.is_quiet(secs(110)), "the rows say none");
    fleet.sync_quiet(UNIX + 20, secs(110), UNIX);
    assert!(fleet.is_quiet(secs(129)));
    assert!(!fleet.is_quiet(secs(130)));
    fleet.sync_quiet(UNIX - 20, secs(110), UNIX);
    assert!(!fleet.is_quiet(secs(110)), "past");
    assert_eq!(fleet.quiet(), secs(30));
}

/// Proves: the orders a beat ended are named once, by host and service, in the order they ended:
/// the beat that shows the host updating ends its order, and a second take names none.
///
/// Catches: an order the rows keep after the host took it up, holding a slot of the role.
#[test]
fn the_orders_a_beat_ended_are_taken_once() {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let host = id("d1");
    fleet.order(WEB, host.clone(), version("2.0"), secs(10));
    let mut one: ServiceBeat = runs(WEB, "1.0");
    one.updating = Some(version("2.0"));
    let beat: Heartbeat = one.as_heartbeat(Platform::LinuxX86_64);
    fleet.record_in(WEB, host.clone(), role("daemon"), beat, secs(11));
    assert_eq!(fleet.take_ended_orders(), vec![(host, WEB.to_owned())]);
    assert!(fleet.take_ended_orders().is_empty());
}
