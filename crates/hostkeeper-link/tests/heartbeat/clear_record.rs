//! The `clear_record` order: one of the loop's orders, run one at a time and reported as theirs
//! are, but no update: its verdict names the version the service runs, its service is not
//! reported `updating` while it runs, and a deferral neither holds it back nor is ended by it.

use hostkeeper_link::deferral::DEFERRAL_FILE;
use hostkeeper_link::testing::{FakeSink, START_UNIX, World};
use hostkeeper_link::{Feature, Order, OrderResult, Outcome, Reason, Verb};

use crate::{Rig, clear_record, recorded, reply, uninstall, update, version};

fn reason(text: &str) -> Option<Reason> {
    Some(Reason::new(text).expect("a reason"))
}

/// The answer of an executor that set the record aside: succeeded, `record_cleared`, naming
/// the version the loop gave.
fn cleared(order: &Order, target: &hostkeeper_config::names::Version) -> OrderResult {
    let mut result = hostkeeper_link::testing::verdict(order, target, Outcome::Succeeded);
    result.reason = reason("record_cleared");
    result
}

/// Catches a `clear_record` dropped by the loop, run on the loop's own thread, given a version
/// the service does not run, reported as an update of that version while it waits, or answered
/// with no verdict: it goes to a worker with the version the step records list; the heartbeat
/// while it waits there lists the service with nothing `updating` and no verdict; the next
/// heartbeat carries its verdict under its own id and try, and nothing is logged.
#[test]
fn a_clear_record_runs_as_an_order_and_its_verdict_names_the_version_the_service_runs() {
    let rig = Rig::with_world(
        "clear-record-verdict",
        World::with_orders(FakeSink::new(cleared)),
    );
    rig.install(vec![recorded("x", "1.3")]);
    let mut heartbeater = rig.heartbeater();
    let order = clear_record("clear-record-4", "x");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.orders.orders().is_empty());

    heartbeater.beat_once();
    let waiting = rig.world.link.beats()[1].services[0].clone();
    assert_eq!(waiting.updating, None);
    assert_eq!(waiting.last_update, None);

    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), std::slice::from_ref(&order));
    assert_eq!(rig.world.orders.targets(), [version("1.3")]);
    heartbeater.beat_once();
    let reported = rig.world.link.beats()[2].services[0].clone();
    assert_eq!(reported.updating, None);
    assert_eq!(reported.last_update, Some(cleared(&order, &version("1.3"))));
    assert_eq!(rig.world.log.lines(), Vec::<String>::new());
}

/// Catches a `clear_record` started beside an order that runs, or given to the executor for a
/// service the host does not list: one that arrives while an update of the service runs is not
/// started and is answered `busy` at the version listed, which the next heartbeat carries; one of
/// a service the records do not list is not run, and each is logged.
#[test]
fn a_clear_record_waits_its_turn_and_is_not_run_for_a_service_the_host_does_not_list() {
    let rig = Rig::new("clear-record-busy");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let running = update("o-1", 1, "x", "2.0");
    let behind = clear_record("clear-record-5", "x");
    rig.world.link.answer(Ok(reply(Some(running), &[])));
    rig.world.link.answer(Ok(reply(Some(behind.clone()), &[])));
    rig.world
        .link
        .answer(Ok(reply(Some(clear_record("clear-record-6", "web")), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    heartbeater.beat_once();

    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.orders.orders().is_empty());
    assert_eq!(
        rig.world.link.beats()[2].services[0].last_update,
        Some(OrderResult::busy(&behind, version("1.0")))
    );
    assert_eq!(
        rig.world.log.lines(),
        [
            "order clear-record-5 try 1 of x is not started: busy with order o-1 try 1 of x",
            "order clear-record-6 try 1 of web is not run: web is not installed on this host",
        ]
    );
}

/// Catches a `clear_record` held back by a deferral of the version the service runs, and its
/// verdict taken as the end of that deferral: with an uninstall of `x` deferred (its version is
/// the one `x` runs), a `clear_record` of `x` still runs at once; its verdict leaves the
/// deferral's retry time on disk; and the uninstall sent again is still held back until it.
#[test]
fn a_deferral_neither_holds_back_a_clear_record_nor_is_ended_by_its_verdict() {
    let world = World::with_orders(FakeSink::new(|order, target| match order.verb {
        Verb::ClearRecord(_) => cleared(order, target),
        _ => {
            let deferred = Outcome::Deferred {
                retry_after_secs: 600,
            };
            let mut result = hostkeeper_link::testing::verdict(order, target, deferred);
            result.reason = reason("drain_bound");
            result
        }
    }));
    let rig = Rig::with_world("clear-record-deferral", world);
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = uninstall("u-1", 1, "x");
    let clear = clear_record("clear-record-2", "x");
    let deferred = [Feature::DeferredV1];
    rig.world
        .link
        .answer(Ok(reply(Some(first.clone()), &deferred)));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());

    rig.world
        .link
        .answer(Ok(reply(Some(clear.clone()), &deferred)));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [first, clear]);

    rig.world
        .link
        .answer(Ok(reply(Some(uninstall("u-1", 2, "x")), &deferred)));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders().len(), 2);
    let file = std::fs::read(rig.service_dir("x").join(DEFERRAL_FILE)).expect("the file");
    let file: serde_json::Value = serde_json::from_slice(&file).expect("JSON");
    assert_eq!(file["retry_after_unix"].as_u64(), Some(START_UNIX + 600));
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "order u-1 try 2 of x is not run: deferred until unix second {}",
            START_UNIX + 600
        )]
    );
}
