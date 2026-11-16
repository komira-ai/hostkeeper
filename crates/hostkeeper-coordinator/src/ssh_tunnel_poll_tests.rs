//! The registry, the poll and the drain of the tunnels of an instance, and the dial: what
//! `ssh_tunnel_tests.rs` has the relay do, seen from what stops it. The rig is the one of that
//! file. No test asserts a time: the timers run on tokio's paused clock, and a test waits for the
//! events the tunnels tell.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use super::tests::{Client, INSTANCE, Rig, caller, frame, limits, open, secs, until_close};
use super::{CLOSE_GRACE, Event, NoSlot, Outgoing, Tunnels};
use crate::accounts::ApiToken;
use crate::ssh_book::{Book, Cause, Opening, close_detail};
use crate::ssh_ledger::{Ledger, Revocation};
use crate::ssh_sign::tests::OPERATORS;

/// Proves: the slots of an instance: `max_tunnels` of them are given and the next is refused
/// `Full`; a slot that is dropped is given again; a slot given counts as held for the drain, and
/// once the drain began none is given (`Draining`).
///
/// Catches: a cap that tunnels being opened slip past, and a slot that is never given back.
#[tokio::test]
async fn the_slots_of_an_instance_are_counted_and_given_back_and_stop_at_the_drain() {
    let rig = Rig::new("tunnel-slots", |limits| limits.max_tunnels = 2).await;
    let first = rig.tunnels.reserve().expect("a slot");
    let second = rig.tunnels.reserve().expect("a slot");
    assert_eq!(rig.tunnels.reserve().err(), Some(NoSlot::Full));
    drop(first);
    let third = rig.tunnels.reserve().expect("a slot is free again");
    let drained = rig.tunnels.drain();
    tokio::pin!(drained);
    assert!(futures::poll!(drained.as_mut()).is_pending());
    assert_eq!(rig.tunnels.reserve().err(), Some(NoSlot::Draining));
    assert!(rig.tunnels.draining());
    drop(second);
    assert!(futures::poll!(drained.as_mut()).is_pending());
    drop(third);
    drained.await;
}

/// Proves: with nothing open the drain returns at once.
///
/// Catches: a drain that waits out its whole time for tunnels that are not there.
#[tokio::test(start_paused = true)]
async fn the_drain_returns_at_once_when_nothing_is_open() {
    let rig = Rig::new("tunnel-drain-none", |_| {}).await;
    tokio::time::timeout(Duration::ZERO, rig.tunnels.drain())
        .await
        .expect("nothing is open: the drain returns at once");
    assert!(rig.tunnels.draining());
}

/// Proves: a tunnel held when the drain begins ends on its own when its host closes, with the
/// cause it ended with, while the one that outlasts the drain is stopped `coordinator_stopping`
/// and closed in the book before the drain returns.
///
/// Catches: a drain that stops tunnels that were ending anyway, one that waits for a tunnel that
/// never ends, and one that returns before the book holds the closes.
#[tokio::test(start_paused = true)]
async fn the_drain_lets_tunnels_end_on_their_own_and_stops_the_rest() {
    let mut rig = Rig::new("tunnel-drain", |limits| {
        limits.drain = secs(600);
        limits.idle = secs(100_000);
        limits.max_session = secs(100_000);
    })
    .await;
    let mut own = rig.tunnel("operator", "node-07").await;
    let mut stubborn = rig.tunnel("operator", "node-08").await;
    let tunnels = Arc::clone(&rig.tunnels);
    let draining = tokio::spawn(async move { tunnels.drain().await });
    while !rig.tunnels.draining() {
        tokio::task::yield_now().await;
    }
    drop(own.host);
    assert_eq!(rig.closed(own.id).await, Cause::HostClosed);
    assert_eq!(until_close(&mut own.out).await.1, "host_closed");
    tokio::time::advance(secs(601)).await;
    draining.await.expect("the drain ends");
    assert_eq!(
        until_close(&mut stubborn.out).await.1,
        "coordinator_stopping"
    );
    assert_eq!(rig.tunnels.count(), 0);
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(
        rig.tunnel_lines().last().map(|line| line.3.clone()),
        Some(close_detail(
            stubborn.id,
            Cause::CoordinatorStopping,
            Some((0, 0)),
            0
        ))
    );
}

/// Proves: a tunnel that registers after the drain gave up waiting is stopped at once
/// (`coordinator_stopping`), so the drain cannot be outrun by a tunnel that was being opened.
///
/// Catches: a tunnel opened across the end of the drain that then runs until its host or timers
/// end it, past the stop of the instance.
#[tokio::test(start_paused = true)]
async fn a_tunnel_being_opened_when_the_drain_gives_up_is_stopped_when_it_registers() {
    let rig = Rig::new("tunnel-late", |limits| limits.drain = secs(5)).await;
    let slot = rig.tunnels.reserve().expect("a slot");
    let tunnels = Arc::clone(&rig.tunnels);
    let draining = tokio::spawn(async move { tunnels.drain().await });
    // The drain's first turn sets its timer: the clock moves after it.
    while !rig.tunnels.draining() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(secs(6)).await;
    while !rig.tunnels.registry().stopping {
        tokio::task::yield_now().await;
    }
    let serial = rig.cert("operator", "node-07");
    let opened = rig
        .book
        .inner
        .open(&Opening {
            operator: "operator".to_owned(),
            host_id: "node-07".to_owned(),
            serial,
            instance: INSTANCE.to_owned(),
            live_secs: 120,
            per_operator: 4,
        })
        .expect("recorded");
    let host = TcpStream::connect(rig.listener.local_addr().expect("an address"))
        .await
        .expect("connected");
    let (_accepted, _) = rig.listener.accept().await.expect("accepted");
    let (_client, incoming) = Client::new();
    let mut out = rig.tunnels.start(
        slot,
        &opened,
        ("operator", "node-07"),
        "root",
        (host, incoming),
        None,
    );
    assert_eq!(
        until_close_after_opened(&mut out).await,
        "coordinator_stopping"
    );
    draining.await.expect("the drain ends");
}

/// The cause of the `close` that ends a stream whose first frame is `opened`.
async fn until_close_after_opened(out: &mut Outgoing) -> String {
    assert!(matches!(frame(out).await, Frame::Opened(_)));
    until_close(out).await.1
}

/// Proves: the poll stops a tunnel `token_revoked` when its operator has no token (a token of the
/// tokens file, or an API token, keeps it), `not_permitted` when the operators file no longer
/// lists the operator, no longer permits the host or lists a host that is no longer enrolled, and `revoked` when its certificate was revoked; a tunnel of an operator whose
/// file entry and token stand is left; and it asks the book with this instance's own ids and
/// three times the poll period as the stale time.
///
/// Catches: a revoked token, an entry taken out of the operators file, a denied host or a revoked
/// certificate that leaves its tunnel open, and a poll that stops a tunnel that is still allowed.
#[tokio::test]
async fn the_poll_stops_the_tunnels_that_are_no_longer_allowed() {
    let mut rig = Rig::new("tunnel-poll", |_| {}).await;
    // bob has no token: the first poll stops his tunnel.
    let mut bob = rig.tunnel("bob", "node-08").await;
    // carol holds an API token, and then does not.
    rig.coordinator
        .accounts()
        .change(|accounts| {
            accounts.with_token(ApiToken {
                name: "carol".to_owned(),
                sha256: "0".repeat(64),
                created_by: "operator".to_owned(),
                created_at: 1,
            })
        })
        .expect("an API token");
    rig.hear("nuc");
    let mut carol = rig.tunnel("carol", "nuc").await;
    let mut kept = rig.tunnel("operator", "node-07").await;
    rig.poll().await;
    assert_eq!(rig.closed(bob.id).await, Cause::TokenRevoked);
    assert_eq!(until_close(&mut bob.out).await.1, "token_revoked");
    assert_eq!(rig.tunnels.count(), 2);
    let polls = rig.book.polls();
    assert_eq!(
        polls.last(),
        Some(&(INSTANCE.to_owned(), vec![bob.id, carol.id, kept.id], 30))
    );

    rig.coordinator
        .accounts()
        .change(|accounts| accounts.without_token("carol"))
        .expect("the token is removed");
    rig.poll().await;
    assert_eq!(rig.closed(carol.id).await, Cause::TokenRevoked);
    assert_eq!(until_close(&mut carol.out).await.1, "token_revoked");

    // The file lists no one for the host any more.
    std::fs::write(
        &rig.operators,
        "[[operator]]\nname = \"operator\"\nhosts = [\"node-08\"]\n",
    )
    .expect("a new operators file");
    rig.poll().await;
    assert_eq!(rig.closed(kept.id).await, Cause::NotPermitted);
    assert_eq!(until_close(&mut kept.out).await.1, "not_permitted");

    // The entry is gone, or the host denied.
    for how in 0..2 {
        std::fs::write(&rig.operators, OPERATORS).expect("the operators file as it was");
        let mut live = rig.tunnel("operator", "node-07").await;
        match how {
            0 => std::fs::write(&rig.operators, "").expect("an empty file"),
            _ => {
                let host = hostkeeper_enrollment::names::HostId::parse("node-07").expect("an id");
                assert!(
                    rig.coordinator
                        .enrollment()
                        .expect("enrollment")
                        .store()
                        .deny(&host)
                        .expect("denied")
                );
            }
        }
        rig.poll().await;
        assert_eq!(rig.closed(live.id).await, Cause::NotPermitted, "{how}");
        assert_eq!(until_close(&mut live.out).await.1, "not_permitted");
    }
}

/// Proves: a poll that cannot read the operators file keeps the tunnels (a host that is still
/// enrolled and an operator who still has a token), and the next poll that can read a file that
/// no longer lists the operator stops them `not_permitted`.
///
/// Catches: a file that is missing for a moment (a rename of the file that replaces it) that
/// stops every tunnel of the instance.
#[tokio::test]
async fn a_poll_that_cannot_read_the_operators_file_keeps_the_tunnels() {
    let mut rig = Rig::new("tunnel-poll-unreadable", |_| {}).await;
    let mut live = rig.tunnel("operator", "node-07").await;
    std::fs::remove_file(&rig.operators).expect("the file is removed");
    rig.poll().await;
    // The relay still carries bytes: a stopped one would answer its `close` first.
    live.host.write_all(b"x").await.expect("the host writes");
    assert_eq!(frame(&mut live.out).await, Frame::Data(b"x".to_vec()));
    assert_eq!(rig.tunnels.count(), 1);

    std::fs::write(&rig.operators, "").expect("a file that lists no one");
    rig.poll().await;
    assert_eq!(rig.closed(live.id).await, Cause::NotPermitted);
}

/// Proves: the poll gives the book the ids of the tunnels held and of the row of a tunnel that
/// is being opened (recorded, not registered), once each also when it registers, and not the
/// row of one whose caller gave up.
///
/// Catches: a row of a tunnel being opened that the poll does not refresh, which another
/// instance's reaper could close while the host is dialled, and an id that stays after the row
/// is closed.
#[tokio::test]
async fn the_poll_passes_the_row_of_a_tunnel_being_opened_with_the_tunnels_held() {
    let mut rig = Rig::new("tunnel-poll-pending", |_| {}).await;
    let live = rig.tunnel("operator", "node-07").await;
    let opening = |serial| Opening {
        operator: "operator".to_owned(),
        host_id: "node-08".to_owned(),
        serial,
        instance: INSTANCE.to_owned(),
        live_secs: 120,
        per_operator: 4,
    };
    let serial = rig.cert("operator", "node-08");
    let slot = rig.tunnels.reserve().expect("a slot");
    let dialed = Arc::clone(&rig.tunnels)
        .open_and_dial(opening(serial), slot)
        .await
        .expect("recorded and dialled");
    let pending = dialed.recorded.opened.id;
    rig.poll().await;
    let expected = (INSTANCE.to_owned(), vec![live.id, pending], 30);
    assert_eq!(rig.book.polls().last(), Some(&expected));

    let opened = dialed.recorded.keep();
    let (_client, incoming) = Client::new();
    let _out = rig.tunnels.start(
        dialed.slot,
        &opened,
        ("operator", "node-08"),
        "root",
        (dialed.host, incoming),
        None,
    );
    rig.poll().await;
    assert_eq!(rig.book.polls().last(), Some(&expected));

    // A row whose caller gave up is closed and is not passed again.
    let again = rig.cert("operator", "node-08");
    rig.book
        .inner
        .close(&super::closing_of(pending, Cause::Eof))
        .expect("closed");
    let slot = rig.tunnels.reserve().expect("a slot");
    let dialed = Arc::clone(&rig.tunnels)
        .open_and_dial(opening(again), slot)
        .await
        .expect("recorded and dialled");
    let second = dialed.recorded.opened.id;
    drop(dialed);
    rig.poll().await;
    let passed = rig.book.polls().last().cloned().expect("a poll").1;
    assert!(!passed.contains(&second), "{passed:?}");
}

/// Proves: the row of a tunnel whose host could not be dialled and whose close the book refused
/// is no longer passed to the poll, so the poll's reaper closes it (`instance_lost`) once it
/// went stale; the same for the row of a relay whose close the book refused.
///
/// Catches: a row left by a failed close that nothing reaps, which keeps the operator from the
/// host until the instance stops.
#[tokio::test]
async fn a_row_whose_close_failed_is_reaped_by_the_poll_of_its_own_instance() {
    let closed_port = {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        closed.local_addr().expect("an address").port()
    };
    let rig = Rig::new("tunnel-poll-own-stale", |limits| limits.port = closed_port).await;
    rig.book.fail("close");
    let serial = rig.cert("operator", "node-07");
    let answer = rig
        .try_open(&caller("operator"), open("node-07", serial))
        .await
        .0;
    assert!(answer.is_err());
    assert!(rig.tunnels.registry().recorded.is_empty());
    let left = rig.book.inner.tunnel_ids();
    assert_eq!(left.len(), 1);
    rig.book.inner.age_tunnel(left[0], 1000);
    rig.poll().await;
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(
        rig.tunnel_lines().last().map(|line| line.3.clone()),
        Some(close_detail(left[0], Cause::InstanceLost, None, 1000))
    );

    // A relay whose close was refused.
    let mut other = Rig::new("tunnel-poll-own-stale-relay", |_| {}).await;
    other.book.fail("close");
    let mut live = other.tunnel("operator", "node-07").await;
    drop(live.host);
    assert_eq!(until_close(&mut live.out).await.1, "host_closed");
    assert_eq!(other.book.inner.tunnel_ids(), vec![live.id]);
    other.book.inner.age_tunnel(live.id, 1000);
    other.poll().await;
    assert_eq!(other.book.inner.tunnel_ids(), Vec::<u64>::new());
}

/// Proves: a drain whose stopped tunnels cannot be closed in the book (a close that does not
/// return) gives up waiting for the closes after a grace and returns; the close is written when
/// the book answers.
///
/// Catches: a stop that never ends because the book is slow, so that the runtime kills the
/// process and every row is left for the reaper.
#[tokio::test(start_paused = true)]
async fn the_drain_returns_when_the_book_does_not_take_the_closes() {
    let mut rig = Rig::new("tunnel-drain-hung", |limits| {
        limits.drain = secs(5);
        limits.idle = secs(100_000);
        limits.max_session = secs(100_000);
    })
    .await;
    let live = rig.tunnel("operator", "node-07").await;
    let hold = rig.book.hold();
    let tunnels = Arc::clone(&rig.tunnels);
    let draining = tokio::spawn(async move { tunnels.drain().await });
    // A book call in flight keeps the clock from moving by itself: the test moves it.
    while !rig.tunnels.draining() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(secs(6)).await;
    while !rig.tunnels.registry().stopping {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(CLOSE_GRACE + secs(1)).await;
    draining.await.expect("the drain ends");
    assert_eq!(
        rig.tunnels.count(),
        1,
        "the close is still waiting at the book"
    );
    drop(hold);
    assert_eq!(rig.closed(live.id).await, Cause::CoordinatorStopping);
    assert_eq!(rig.tunnels.count(), 0);
}

/// Proves: the poll stops a tunnel `revoked` when the book says its certificate was revoked, and
/// leaves it when the host's record cannot be read (the next poll reads it again) or the book
/// cannot be polled.
///
/// Catches: a revocation that does not close the operator's open tunnels, a store failure that
/// closes tunnels, and a book failure that stops the poll loop from going on.
#[tokio::test]
async fn the_poll_stops_a_tunnel_whose_certificate_was_revoked_and_keeps_those_it_cannot_judge() {
    let mut rig = Rig::new("tunnel-poll-revoked", |_| {}).await;
    let mut live = rig.tunnel("operator", "node-07").await;
    rig.records.fail_at(rig.records.count() + 1);
    rig.poll().await;
    assert_eq!(rig.tunnels.count(), 1);
    rig.book.fail("poll");
    rig.ledger
        .revoke("operator", Revocation::Serial(live.serial))
        .expect("revoked");
    rig.poll().await;
    assert_eq!(rig.tunnels.count(), 1);
    rig.book.mend("poll");
    rig.book.fail("panic_poll");
    rig.poll().await;
    assert_eq!(rig.tunnels.count(), 1);
    rig.book.mend("panic_poll");
    rig.poll().await;
    assert_eq!(rig.closed(live.id).await, Cause::Revoked);
    assert_eq!(until_close(&mut live.out).await.1, "revoked");
}

/// Proves: a tunnel whose row another instance's reaper closed (this instance stopped refreshing
/// it for the stale time) is stopped `instance_lost` by its own instance's next poll, with no
/// close written by this one; and a poll closes the rows of other instances that are stale.
///
/// Catches: a relay that goes on for a tunnel the audit says is closed, and a second close line
/// for it.
#[tokio::test]
async fn a_tunnel_another_instance_closed_is_stopped_instance_lost() {
    let mut rig = Rig::new("tunnel-lost", |_| {}).await;
    let mut live = rig.tunnel("operator", "node-07").await;
    let other = Tunnels::new(
        Arc::clone(&rig.book) as Arc<dyn Book>,
        "ffffffffffffffff".to_owned(),
        limits(1),
    );
    rig.book.inner.age_tunnel(live.id, 1000);
    other.poll_once(&rig.coordinator).await;
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    let lines = rig.tunnel_lines();
    assert_eq!(
        lines.last().map(|line| line.3.clone()),
        Some(close_detail(live.id, Cause::InstanceLost, None, 1000))
    );
    rig.poll().await;
    assert_eq!(rig.closed(live.id).await, Cause::InstanceLost);
    assert_eq!(until_close(&mut live.out).await.1, "instance_lost");
    assert_eq!(rig.tunnel_lines().len(), lines.len());
}

/// Proves: the poll loop polls the book from the first tick and again each period for as long
/// as it is polled (a tunnel opened after the first poll is stopped by a later one), and
/// `run_poll` of a coordinator without tunnels waits for ever.
///
/// Catches: a loop that polls once, and one that returns for a coordinator that has no tunnels.
#[tokio::test(start_paused = true)]
async fn the_poll_loop_polls_every_period() {
    let mut rig = Rig::new("tunnel-poll-loop", |limits| limits.poll = secs(10)).await;
    let first = rig.tunnel("bob", "node-08").await;
    let looping = tokio::spawn(Arc::clone(&rig.tunnels).poll_loop(Arc::clone(&rig.coordinator)));
    assert_eq!(rig.closed(first.id).await, Cause::TokenRevoked);
    let second = rig.tunnel("bob", "node-08").await;
    assert_eq!(rig.closed(second.id).await, Cause::TokenRevoked);
    looping.abort();

    let dir = crate::testutil::scratch("tunnel-run-poll");
    let plain = Arc::new(crate::testutil::Rig::start(
        &dir,
        &Arc::new(std::sync::atomic::AtomicU64::new(1_000)),
    ));
    let waiting = super::run_poll(plain);
    tokio::pin!(waiting);
    assert!(futures::poll!(waiting.as_mut()).is_pending());
}

/// Proves: a poll of a coordinator that holds no SSH signing does nothing, not even a call of the
/// book.
///
/// Catches: a poll that reaps or stops tunnels without the rules to judge them.
#[tokio::test]
async fn a_poll_without_signing_leaves_the_book_alone() {
    let rig = Rig::new("tunnel-poll-plain", |_| {}).await;
    let dir = crate::testutil::scratch("tunnel-poll-plain-coordinator");
    let plain =
        crate::testutil::Rig::start(&dir, &Arc::new(std::sync::atomic::AtomicU64::new(1_000)));
    rig.tunnels.poll_once(&plain).await;
    assert_eq!(rig.book.polls(), Vec::new());
}

/// Proves: a connection that is not made in the dial time, a host that refuses the connection and
/// an address that is no IP address each give no connection, and a connection that is made has
/// Nagle's algorithm off.
///
/// Catches: a dial that waits for ever, or panics on an address that is no address.
#[tokio::test(start_paused = true)]
async fn a_host_that_cannot_be_dialled_gives_no_connection() {
    let rig = Rig::new("tunnel-dial", |limits| limits.dial_timeout = secs(5)).await;
    let to = rig.listener.local_addr().expect("an address");
    assert!(
        rig.tunnels
            .connected(to, std::future::pending())
            .await
            .is_none()
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let gone = closed.local_addr().expect("an address");
    drop(closed);
    assert!(
        rig.tunnels
            .connected(gone, TcpStream::connect(gone))
            .await
            .is_none()
    );
    assert!(rig.tunnels.dial("not an address").await.is_none());
    let socket = rig.tunnels.dial("127.0.0.1").await.expect("connected");
    assert!(socket.nodelay().expect("the option"));
}

/// Proves: tunnels that nobody watches tell no event and do not fail for it.
///
/// Catches: an event hook that assumes a reader.
#[tokio::test]
async fn tunnels_with_no_watcher_tell_their_events_to_no_one() {
    let rig = Rig::new("tunnel-no-watcher", |_| {}).await;
    let unwatched = Tunnels::new(
        Arc::clone(&rig.book) as Arc<dyn Book>,
        "eeeeeeeeeeeeeeee".to_owned(),
        limits(1),
    );
    unwatched.tell(Event::Opened(1));
}
