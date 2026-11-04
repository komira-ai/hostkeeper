//! Orders: to the executor once, on a worker, one at a time.

use std::sync::mpsc;

use hostkeeper_link::testing::verdict;
use hostkeeper_link::{Dispatch, Feature, OrderResult, Outcome, ThreadDispatch};

use crate::{Rig, forced, recorded, reply, update, version};

/// Catches an order run on the loop's own thread (the loop would stop beating for the whole
/// step), an order not reported as `updating` while it runs, an order run twice, and a verdict
/// missing from the heartbeat after it ends.
#[test]
fn an_update_order_reaches_the_executor_once_and_the_verdict_is_in_the_next_beat() {
    let rig = Rig::new("orders-once");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    // Handed to a worker, not run on the loop's thread.
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.orders.orders().is_empty());

    // While it runs the loop beats, and says what it is updating to.
    heartbeater.beat_once();
    let running = &rig.world.link.beats()[1].services[0];
    assert_eq!(running.updating, Some(version("2.0")));
    assert_eq!(running.last_update, None);

    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    let done = &rig.world.link.beats()[2].services[0];
    assert_eq!(done.updating, None);
    assert_eq!(done.last_update, Some(verdict(&order, Outcome::Succeeded)));
    assert_eq!(rig.world.orders.orders(), [order]);
    assert!(!rig.world.dispatch.run_next());
}

/// Catches the same order (id and try) started again, while it runs or after it ended, and a
/// new try of it that is not started.
#[test]
fn the_same_order_in_two_replies_is_not_run_twice() {
    let rig = Rig::new("orders-twice");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    for _ in 0..3 {
        rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    }
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders(), [order]);
    assert!(rig.world.log.lines().is_empty());

    // A new try of the order is a new order.
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 2, "x", "2.0")), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a second order started while one runs (two steps at once on one host), and one
/// dropped without a word: it is not started, its verdict is `busy`, and the line says which
/// order holds the host.
#[test]
fn a_second_order_while_one_runs_is_not_started_and_is_reported_busy() {
    let rig = Rig::new("orders-busy");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = update("o-1", 1, "x", "2.0");
    let second = update("o-2", 1, "y", "3.0");
    rig.world.link.answer(Ok(reply(Some(first.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(second.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(
        rig.world.log.lines(),
        ["order o-2 try 1 of y is not started: busy with order o-1 try 1 of x"]
    );
    heartbeater.beat_once();
    let beat = &rig.world.link.beats()[2];
    assert_eq!(beat.services[0].updating, Some(version("2.0")));
    assert_eq!(beat.services[1].updating, None);
    let busy = OrderResult::busy(&second);
    assert_eq!(busy.reason.as_ref().map(|r| r.as_str()), Some("busy"));
    assert_eq!(busy.outcome, Outcome::Failed);
    assert_eq!(beat.services[1].last_update, Some(busy));
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [first]);
}

/// Catches a worker that died with its job holding the loop forever as busy, or a verdict
/// made up for it: the loop says so, sends no verdict, and takes the next try.
#[test]
fn an_order_whose_worker_dies_has_no_verdict_and_the_loop_goes_on() {
    let rig = Rig::new("orders-dropped");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 1, "x", "2.0")), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.drop_next());
    assert!(!rig.world.dispatch.drop_next());
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 2, "x", "2.0")), &[])));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.log.lines(),
        ["error: order o-1 try 1 of x ended with no verdict"]
    );
    let beat = &rig.world.link.beats()[1].services[0];
    assert_eq!(beat.last_update, None);
    assert_eq!(beat.updating, None);
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a forced order refused although the reply advertises `forced-v1`, or reported as a
/// hooked one.
#[test]
fn a_forced_order_runs_while_forced_v1_is_advertised() {
    let rig = Rig::new("orders-forced");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = forced("o-1", 1, "x", "1.5");
    rig.world
        .link
        .answer(Ok(reply(Some(order.clone()), &[Feature::ForcedV1])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    let shown = rig.world.link.beats()[1].services[0]
        .last_update
        .clone()
        .expect("the verdict");
    assert!(shown.forced);
    assert_eq!(shown.target, version("1.5"));
    assert_eq!(rig.world.orders.orders(), [order]);
}

/// Catches the production dispatcher running the job on the caller's thread, which would stop
/// the heartbeat for the whole step.
#[test]
fn the_thread_dispatch_runs_the_job_on_a_thread_of_its_own() {
    let (sender, receiver) = mpsc::channel();
    ThreadDispatch.start(Box::new(move || {
        let name = std::thread::current().name().map(str::to_owned);
        sender.send(name).expect("the test waits");
    }));
    assert_eq!(
        receiver.recv().expect("the job ran"),
        Some("hostkeeper-order".to_owned())
    );
}
