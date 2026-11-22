//! What the coordinator keeps, shows and forgets of a host's Full Disk Access state.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;

use super::{Reported, show};
use crate::fleet::Fleet;
use crate::testutil::{id, secs};

/// The state `show` fills in for the host `name`.
fn shown(fleet: &Fleet, name: &str) -> i32 {
    let mut host = operator::Host::default();
    show(fleet, &id(name), &mut host);
    host.full_disk_access
}

/// Proves: `show` answers the state kept for the host, and UNSPECIFIED for a host with none kept
/// and for no other host's state.
///
/// Catches: a state shown on another host, and an unknown host shown as granted.
#[test]
fn show_fills_in_the_state_of_the_host_and_of_no_other() {
    let mut fleet = Fleet::new(secs(0), secs(0));
    fleet
        .full_disk_access
        .0
        .insert(id("m1"), agent::FullDiskAccess::NotGranted);
    assert_eq!(
        shown(&fleet, "m1"),
        i32::from(agent::FullDiskAccess::NotGranted)
    );
    assert_eq!(
        shown(&fleet, "m2"),
        i32::from(agent::FullDiskAccess::Unspecified)
    );
}

/// Proves: `Reported::forget` drops the state of the host named and no other host's.
///
/// Catches: a forgotten host's state still shown, and another host's forgotten with it.
#[test]
fn forget_drops_the_state_of_that_host_alone() {
    let mut reported = Reported::default();
    for name in ["m1", "m2"] {
        reported.0.insert(id(name), agent::FullDiskAccess::Granted);
    }
    reported.forget(&id("m1"));
    reported.forget(&id("m3"));
    assert_eq!(reported.0.keys().cloned().collect::<Vec<_>>(), [id("m2")]);
}

/// Proves: a host the fleet releases (`Fleet::release_if_final`) has its state forgotten, and
/// another host's stays.
///
/// Catches: a retired host's state held in memory for good, and another host's forgotten with it.
#[test]
fn a_released_host_has_its_state_forgotten() {
    let mut fleet = Fleet::new(secs(0), secs(0));
    for name in ["m1", "m2"] {
        fleet
            .full_disk_access
            .0
            .insert(id(name), agent::FullDiskAccess::Granted);
    }
    assert!(fleet.release_if_final(&id("m1"), secs(10)));
    assert_eq!(
        shown(&fleet, "m1"),
        i32::from(agent::FullDiskAccess::Unspecified)
    );
    assert_eq!(
        shown(&fleet, "m2"),
        i32::from(agent::FullDiskAccess::Granted)
    );
}
