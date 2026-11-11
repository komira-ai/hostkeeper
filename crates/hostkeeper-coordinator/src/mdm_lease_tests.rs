//! The lease ([`crate::mdm_lease`]): what a store's health makes of it, and the seat the tests of
//! the timers use.

use std::sync::Arc;

use crate::mdm_lease::Lease;
use crate::mdm_lease::seat::Seat;
use crate::store::{Backend, Fake, Health};

/// Proves: the store's lease is held while the store takes writes, and not while its session is
/// lost or another coordinator led it, and it is held again when the session is back.
///
/// Catches: a node that pushes, syncs or renews while the database no longer lets it write, or one
/// that never goes on after a revival.
#[test]
fn the_stores_lease_is_held_while_the_store_takes_writes() {
    let fake = Fake::new();
    let lease = Lease::of_store(Arc::new(fake.clone()) as Arc<dyn Backend>);
    assert!(lease.held());
    fake.set_health(Health::Lost);
    assert!(!lease.held());
    fake.set_health(Health::Deposed { found: 7 });
    assert!(!lease.held());
    fake.set_health(Health::Up);
    assert!(lease.held());
}

/// Proves: a seat is held by one node at a time: the node it is given to holds it and no other,
/// and a lost seat is held by none.
///
/// Catches: a test double that lets two nodes hold the lease, which would make every two-node test
/// pass for the wrong reason.
#[test]
fn a_seat_has_one_holder_at_a_time() {
    let seat = Seat::empty();
    let (a, b) = (seat.node("a"), seat.node("b"));
    assert!(!a.held() && !b.held());
    seat.give_to("a");
    assert!(a.held() && !b.held());
    seat.give_to("b");
    assert!(!a.held() && b.held());
    seat.lose();
    assert!(!a.held() && !b.held());
    assert_eq!(format!("{a:?}"), "Lease { .. }");
}
