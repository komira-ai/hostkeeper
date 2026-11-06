//! Orders: to the executor once, on a worker, one at a time.

use std::sync::mpsc;

use hostkeeper_link::testing::identity;
use hostkeeper_link::{
    BeatOutcome, CertState, Dispatch, Feature, LinkError, OrderResult, Outcome, PackageDigest,
    ThreadDispatch, Verb,
};

use crate::verdict;
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
    let busy = OrderResult::busy(&second, version("3.0"));
    assert_eq!(busy.reason.as_ref().map(|r| r.as_str()), Some("busy"));
    assert_eq!(busy.outcome, Outcome::Failed);
    assert_eq!(beat.services[1].last_update, Some(busy));
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [first]);
}

/// Catches a worker that died with its job holding the loop forever as busy, a verdict made up
/// for it, or its order marked as started: the loop says so, sends no verdict, and runs the
/// same order (same id and try) when the coordinator sends it again.
#[test]
fn an_order_whose_worker_dies_has_no_verdict_and_runs_when_sent_again() {
    let rig = Rig::new("orders-dropped");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.drop_next());
    assert!(!rig.world.dispatch.drop_next());
    heartbeater.beat_once();
    assert_eq!(
        rig.world.log.lines(),
        ["error: order o-1 try 1 of x ended with no verdict"]
    );
    let beat = &rig.world.link.beats()[1].services[0];
    assert_eq!(beat.last_update, None);
    assert_eq!(beat.updating, None);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [order]);
}

/// Catches an order that differs from the last one only in its fields (version, digest, verb or
/// service) under the same id and try being dropped as a resend: it goes to the executor, which
/// answers `try_conflict`; while another runs it is answered `busy` and not started.
#[test]
fn an_order_with_the_same_id_and_try_but_other_fields_goes_to_the_executor() {
    let rig = Rig::new("orders-conflict");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = update("o-1", 1, "x", "2.0");
    let other_version = update("o-1", 1, "x", "3.0");
    let mut other_digest = first.clone();
    if let Verb::Update(target) = &mut other_digest.verb {
        target.digest = PackageDigest::new(&"0b".repeat(32)).expect("a digest");
    }
    let other_verb = forced("o-1", 1, "x", "2.0");
    let other_service = update("o-1", 1, "y", "2.0");
    rig.world.link.answer(Ok(reply(Some(first.clone()), &[])));
    rig.world
        .link
        .answer(Ok(reply(Some(other_version.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(
        rig.world.log.lines(),
        ["order o-1 try 1 of x is not started: busy with order o-1 try 1 of x"]
    );
    assert!(rig.world.dispatch.run_next());
    for order in [other_version, other_digest, other_verb, other_service] {
        rig.world
            .link
            .answer(Ok(reply(Some(order), &[Feature::ForcedV1])));
        heartbeater.beat_once();
        assert_eq!(rig.world.dispatch.pending(), 1);
        assert!(rig.world.dispatch.run_next());
    }
    assert_eq!(rig.world.orders.orders().len(), 5);
}

/// Catches an order run for a service the host does not have (an order names a service the
/// records list, never one the loop invents): it is not run and the line says so.
#[test]
fn an_order_for_a_service_not_installed_is_not_run() {
    let rig = Rig::new("orders-absent");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 1, "y", "2.0")), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        rig.world.log.lines(),
        ["order o-1 try 1 of y is not run: y is not installed on this host"]
    );
}

/// Catches the verdict of an order whose service left the records while it ran being lost: it
/// is kept, said, and sent once the service is listed again.
#[test]
fn a_verdict_of_a_service_that_left_is_kept_until_it_is_listed_again() {
    let rig = Rig::new("orders-left");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    rig.world.records.set(Vec::new());
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert!(rig.world.link.beats()[2].services.is_empty());
    assert_eq!(
        rig.world.log.lines(),
        ["the verdict of order o-1 try 1 is kept until x is listed again"]
    );
    rig.world.records.set(vec![recorded("x", "2.0")]);
    heartbeater.beat_once();
    assert_eq!(
        rig.world.link.beats()[3].services[0].last_update,
        Some(verdict(&order, Outcome::Succeeded))
    );
}

/// Catches a step whose verdict is lost, or that is run again, when the coordinator cannot be
/// reached, the certificate goes away or the records cannot be read while it runs and as it
/// ends: the verdict is sent at the first heartbeat that goes out, the order ran once.
#[test]
fn a_running_order_survives_link_certificate_and_record_failures() {
    let rig = Rig::new("orders-partition");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    rig.world.link.answer(Err(LinkError::Transport(
        std::io::ErrorKind::ConnectionReset,
    )));
    heartbeater.beat_once();
    assert_eq!(
        heartbeater.beat_once(),
        BeatOutcome::NotSent(LinkError::Transport(std::io::ErrorKind::ConnectionReset))
    );
    rig.world.certs.set(CertState::NotEnrolled {
        waiting_for: rig.state().join("pki"),
    });
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    rig.world.certs.set(CertState::Enrolled(identity("a")));
    rig.world.records.fail(Some(std::io::ErrorKind::Other));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NoFacts);
    rig.world.records.fail(None);
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let beats = rig.world.link.beats();
    assert_eq!(beats[1].services[0].updating, Some(version("2.0")));
    let last = beats.last().expect("a beat");
    assert_eq!(last.services[0].updating, None);
    assert_eq!(
        last.services[0].last_update,
        Some(verdict(&order, Outcome::Succeeded))
    );
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders(), [order]);
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
