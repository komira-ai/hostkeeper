//! The push sender under the lease (`docs/design/coordinator-mdm.md`, section 4.9, CM19): two
//! nodes' queues over one set of tables and one fake APNs, of which only the node that holds the
//! lease pushes or returns a timed-out command to the queue. The clock is the rig's; nothing waits.

use hostkeeper_mdm_gate::CommandQueue as _;

use crate::mdm_lease::seat::Seat;
use crate::mdm_server::support::{Rig, SERIAL, enrollment_id};
use crate::store::mdm::{CommandState, MdmStore};

/// The seconds of the default floor (`push_retry_s`).
const FLOOR: u64 = 3600;

/// The seconds of a command's timeout in a rig's queues.
const TIMEOUT: u64 = 86_400;

/// A command of the live enrollment `e1`, left queued by a Mac that answered `NotNow`, so that the
/// floor decides the next push.
fn left_queued(rig: &Rig) {
    rig.live("e1", SERIAL, true);
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
}

/// Proves: of two nodes' queues over the same tables, the one that holds the lease pushes at the
/// floor and the other pushes nothing; when the lease moves, the new holder starts its floor at
/// its next sweep and pushes one floor later, and the old holder stays quiet; with the lease lost
/// by both nobody pushes.
///
/// Catches: two coordinators both waking every Mac, or a node whose link was lost going on pushing
/// beside the one that took over.
#[tokio::test]
async fn only_the_node_that_holds_the_lease_pushes_at_the_floor() {
    let rig = Rig::new("lease-queue-floor");
    left_queued(&rig);
    let seat = Seat::empty();
    let (a, b) = (
        rig.queue_under(seat.node("a")),
        rig.queue_under(seat.node("b")),
    );
    seat.give_to("a");
    for node in [&a, &b] {
        node.sweep().await.expect("swept");
    }
    rig.time.advance(FLOOR);
    b.sweep().await.expect("swept");
    assert!(
        rig.apns.sent().is_empty(),
        "the node without the lease pushes nothing"
    );
    a.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "the holder pushes at the floor");

    seat.give_to("b");
    rig.time.advance(FLOOR);
    a.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "the old holder is quiet");
    b.sweep().await.expect("swept");
    assert_eq!(
        rig.apns.sent().len(),
        1,
        "the new holder's floor starts at its first sweep"
    );
    rig.time.advance(FLOOR);
    b.sweep().await.expect("swept");
    assert_eq!(
        rig.apns.sent().len(),
        2,
        "the new holder pushes at its floor"
    );

    seat.lose();
    rig.time.advance(FLOOR);
    for node in [&a, &b] {
        node.sweep().await.expect("swept");
    }
    assert_eq!(rig.apns.sent().len(), 2, "a lost lease pushes nothing");
}

/// Proves: a command queued through a node that holds no lease is stored and not pushed, and the
/// same command queued through the holder is pushed at once.
///
/// Catches: a node that lost its lease waking a Mac for a command the holder will push.
#[tokio::test]
async fn a_command_queued_without_the_lease_is_kept_and_not_pushed() {
    let rig = Rig::new("lease-queue-enqueue");
    rig.live("e1", SERIAL, true);
    let seat = Seat::empty();
    let node = rig.queue_under(seat.node("a"));
    let command = |n: u8| {
        hostkeeper_mdm_gate::Command::device_information(
            hostkeeper_mdm_gate::CommandUuid::from_random([n; 16]),
        )
    };
    let id = enrollment_id("e1");
    node.enqueue(&id, &command(1)).await.expect("queued");
    assert!(rig.apns.sent().is_empty());
    assert_eq!(
        rig.store
            .command(&Rig::command_id(1))
            .expect("read")
            .expect("kept")
            .state,
        CommandState::Queued
    );
    seat.give_to("a");
    node.enqueue(&id, &command(2)).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 1);
}

/// Proves: a node that holds no lease does not return a command whose timeout passed to the queue,
/// and the holder does, and pushes for it.
///
/// Catches: two nodes both re-sending one command, or a node cut off from the database changing
/// the commands of the node that leads.
#[tokio::test]
async fn only_the_holder_returns_a_timed_out_command_to_the_queue() {
    let rig = Rig::new("lease-queue-timeout");
    left_queued(&rig);
    rig.queue.claim("e1", &[]).expect("sent");
    let seat = Seat::empty();
    let (a, b) = (
        rig.queue_under(seat.node("a")),
        rig.queue_under(seat.node("b")),
    );
    seat.give_to("a");
    rig.time.advance(TIMEOUT + 1);
    b.sweep().await.expect("swept");
    let state = || {
        rig.store
            .command(&Rig::command_id(1))
            .expect("read")
            .expect("the command")
            .state
    };
    assert_eq!(
        state(),
        CommandState::Sent,
        "the node without the lease left it"
    );
    assert!(rig.apns.sent().is_empty());
    a.sweep().await.expect("swept");
    assert_eq!(state(), CommandState::Queued, "the holder returned it");
    assert_eq!(rig.apns.sent().len(), 1, "and pushed for it");
}

/// Proves: a node whose lease was lost and is held again starts the floor of a waiting Mac anew at
/// its first sweep, and does not push at the floor it had before the gap.
///
/// Catches: a node pushing once at a floor that was set before it lost the lease, from what it
/// knew of the Macs then.
#[tokio::test]
async fn a_node_that_regains_the_lease_starts_its_floor_anew() {
    let rig = Rig::new("lease-queue-regain");
    left_queued(&rig);
    let seat = Seat::empty();
    let node = rig.queue_under(seat.node("a"));
    seat.give_to("a");
    node.sweep().await.expect("swept");
    seat.lose();
    rig.time.advance(FLOOR - 10);
    node.sweep().await.expect("swept");
    seat.give_to("a");
    node.sweep().await.expect("swept");
    rig.time.advance(20);
    node.sweep().await.expect("swept");
    assert!(
        rig.apns.sent().is_empty(),
        "the floor of the first hold is gone"
    );
    rig.time.advance(FLOOR);
    node.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "the new floor pushes");
}

/// Proves: a node that holds no push identity and no lease sweeps without error, and once it holds
/// the lease it sweeps as before and pushes nothing.
///
/// Catches: the sweep of a node with nothing to push with failing while the lease is out.
#[tokio::test]
async fn a_node_without_a_push_identity_sweeps_through_a_gap_in_the_lease() {
    let rig = Rig::new("lease-queue-no-identity");
    left_queued(&rig);
    let seat = Seat::empty();
    let node = rig.queue_without_push_identity(seat.node("a"));
    node.sweep().await.expect("swept without the lease");
    seat.give_to("a");
    rig.time.advance(FLOOR);
    node.sweep().await.expect("swept with the lease");
    assert!(rig.apns.sent().is_empty(), "there is nothing to push with");
}
