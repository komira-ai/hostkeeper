//! The orders of the gRPC route beside the update: `install`, `uninstall` and `abort`, as the
//! loop hands them to the executor and reports them.

use hostkeeper_config::names::Version;
use hostkeeper_hook::Phase;
use hostkeeper_link::testing::{FakeSink, World};
use hostkeeper_link::{
    Feature, Health, Liveness, Observed, Order, OrderResult, Outcome, Reason, ServiceFacts, Verb,
};

use crate::{Rig, abort, install, recorded, reply, service, uninstall, update, verdict, version};

fn reason(text: &str) -> Option<Reason> {
    Some(Reason::new(text).expect("a reason"))
}

/// Every verdict the heartbeats carried, in order.
fn verdicts(rig: &Rig) -> Vec<OrderResult> {
    rig.world
        .link
        .beats()
        .iter()
        .flat_map(|beat| beat.services.iter())
        .filter_map(|facts| facts.last_update.clone())
        .collect()
}

/// An executor under which an `abort` answers `abort_answer` and every other order `outcome`
/// in the halt phase, each naming the version the loop gave.
fn executor(abort_answer: fn(&Order, &Version) -> OrderResult) -> FakeSink {
    FakeSink::new(move |order, target| match order.verb {
        Verb::Abort(_) => abort_answer(order, target),
        _ => {
            let mut result = hostkeeper_link::testing::verdict(order, target, Outcome::Failed);
            result.phase = Some(Phase::Halt);
            result.reason = reason("aborted_by_operator");
            result
        }
    })
}

/// Catches an install refused for its service not being installed (an install is for a service
/// the host does not run), one whose verdict names another version, and a verdict kept unsent
/// because the service was not listed when it ended: it runs on a worker with its own target,
/// and its verdict goes out in the next heartbeat, under an entry of the service absent at the
/// install's target, then under the service's own entry once the records list it.
#[test]
fn an_install_runs_for_a_service_the_host_does_not_list_and_its_verdict_is_sent_at_once() {
    let rig = Rig::new("typed-install");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = install("install-1", 1, "web", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), std::slice::from_ref(&order));
    assert_eq!(rig.world.orders.targets(), [version("2.0")]);

    // The ended install is not run again for the same order, and its verdict is sent at once.
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        rig.world.link.beats()[1].services,
        [
            ServiceFacts {
                name: service("x"),
                ..rig.world.link.beats()[1].services[0].clone()
            },
            ServiceFacts {
                name: service("web"),
                version: version("2.0"),
                health: Health::Absent,
                updating: None,
                last_update: Some(verdict(&order, Outcome::Succeeded)),
                deferred: None,
                health_detail: None,
                observed: Some(Observed {
                    liveness: Liveness::Absent,
                    version: None,
                }),
                record_try: None,
                installer: None,
                drift: None,
            },
        ]
    );
    rig.install(vec![recorded("web", "2.0"), recorded("x", "1.0")]);
    heartbeater.beat_once();
    let listed = &rig.world.link.beats()[2].services;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].name, service("web"));
    assert_ne!(listed[0].health, Health::Absent);
    assert_eq!(
        listed[0].last_update,
        Some(verdict(&order, Outcome::Succeeded))
    );
    assert_eq!(
        rig.world.log.lines(),
        [
            "order install-1 try 1 of web, which is not installed, ended succeeded: it is sent \
             with web listed absent"
        ]
    );
}

/// Catches an uninstall whose verdict names no version or another one than the service runs, an
/// uninstall that succeeded but is still reported (the service is gone from the heartbeat, which
/// is the report), and earlier verdicts of the gone service sent when it comes back: the
/// executor is given the listed version, the success is logged and leaves no verdict, and the
/// failed update before it is dropped with the service.
#[test]
fn an_uninstall_that_succeeded_leaves_no_verdict_and_takes_the_services_verdicts_with_it() {
    let world = World::with_orders(FakeSink::new(|order, target| {
        let outcome = match order.verb {
            Verb::Uninstall(_) => Outcome::Succeeded,
            _ => Outcome::Failed,
        };
        hostkeeper_link::testing::verdict(order, target, outcome)
    }));
    let rig = Rig::with_world("typed-uninstall", world);
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let failed = update("o-1", 1, "x", "2.0");
    let removal = uninstall("uninstall-1", 1, "x");
    rig.world.link.answer(Ok(reply(Some(failed.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world.link.answer(Ok(reply(Some(removal.clone()), &[])));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.link.beats()[1].services[0].last_update,
        Some(verdict(&failed, Outcome::Failed))
    );
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [failed, removal]);
    assert_eq!(rig.world.orders.targets(), [version("2.0"), version("1.0")]);

    rig.install(Vec::new());
    heartbeater.beat_once();
    assert_eq!(rig.world.link.beats()[2].services, []);
    rig.install(vec![recorded("x", "3.0")]);
    heartbeater.beat_once();
    assert_eq!(rig.world.link.beats()[3].services[0].last_update, None);
    assert_eq!(
        rig.world.log.lines(),
        ["order uninstall-1 try 1 uninstalled x"]
    );
}

/// Catches a failed uninstall reported as done, or with a version the service does not run: its
/// verdict is sent, failed, naming the version the heartbeat listed.
#[test]
fn a_failed_uninstall_is_reported_at_the_version_the_service_runs() {
    let world = World::with_orders(FakeSink::new(|order, target| {
        hostkeeper_link::testing::verdict(order, target, Outcome::Failed)
    }));
    let rig = Rig::with_world("typed-uninstall-failed", world);
    rig.install(vec![recorded("x", "1.4")]);
    let mut heartbeater = rig.heartbeater();
    let removal = uninstall("uninstall-1", 1, "x");
    rig.world.link.answer(Ok(reply(Some(removal.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    let report = rig.world.link.beats()[1].services[0].last_update.clone();
    let expected = hostkeeper_link::testing::verdict(&removal, &version("1.4"), Outcome::Failed);
    assert_eq!(report, Some(expected));
    assert!(rig.world.log.lines().is_empty());
}

/// Catches an uninstall of a service the host does not list given to the executor (it has no
/// version to report), and one sent while another order runs started beside it: the first is
/// not run and is logged, with no verdict; the second is `busy` at the version listed.
#[test]
fn an_uninstall_of_an_unlisted_service_is_not_run_and_one_while_busy_is_busy() {
    let rig = Rig::new("typed-uninstall-refused");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "4.2")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(uninstall("uninstall-1", 1, "web")), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);

    let running = update("o-1", 1, "x", "2.0");
    let removal = uninstall("uninstall-2", 1, "y");
    rig.world.link.answer(Ok(reply(Some(running.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(removal.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert_eq!(
        rig.world.link.beats()[3].services[1].last_update,
        Some(OrderResult::busy(&removal, version("4.2")))
    );
    assert_eq!(
        rig.world.log.lines(),
        [
            "order uninstall-1 try 1 of web is not run: web is not installed on this host",
            "order uninstall-2 try 1 of y is not started: busy with order o-1 try 1 of x",
        ]
    );
}

/// Catches an abort answered `busy` while the update it is for runs (it could then never stop
/// one), an abort run on the loop's thread (it waits for the update to end), and an abort's own
/// answer sent as a verdict in place of the update's: the abort goes to a worker of its own at
/// once, with the running update's target; its answer is logged; the update's halt is the
/// verdict the next heartbeat carries, and no heartbeat carries one of the abort.
#[test]
fn an_abort_runs_beside_the_update_it_stops_and_the_update_reports_the_halt() {
    let world = World::with_orders(executor(|order, target| {
        let mut answer = hostkeeper_link::testing::verdict(order, target, Outcome::Failed);
        answer.reason = reason("aborted_by_operator");
        answer
    }));
    let rig = Rig::with_world("typed-abort", world);
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let running = update("o-1", 1, "x", "2.0");
    let stop = abort("abort-7", "x");
    rig.world.link.answer(Ok(reply(Some(running.clone()), &[])));
    rig.world
        .link
        .answer(Ok(reply(Some(stop.clone()), &[Feature::HaltV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 2);
    assert!(rig.world.orders.orders().is_empty());
    assert!(rig.world.log.lines().is_empty());

    assert!(rig.world.dispatch.run_next());
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [running.clone(), stop]);
    assert_eq!(rig.world.orders.targets(), [version("2.0"), version("2.0")]);
    heartbeater.beat_once();
    let mut halted = verdict(&running, Outcome::Failed);
    halted.phase = Some(Phase::Halt);
    halted.reason = reason("aborted_by_operator");
    assert_eq!(verdicts(&rig), [halted]);
    assert_eq!(
        rig.world.log.lines(),
        ["order abort-7 try 1 of x answered failed: aborted_by_operator"]
    );
}

/// Catches an abort with no order of the loop running dropped (the executor may still hold a
/// step of the service), given a version the service does not run, or sent as a verdict; and an
/// abort of a service the loop neither runs nor lists given to the executor: the first goes to
/// the executor at the listed version, and its answer, with its detail, is only logged; the
/// second is not run and is logged.
#[test]
fn an_abort_with_nothing_running_goes_to_the_executor_unless_the_service_is_unknown() {
    let world = World::with_orders(executor(|order, target| {
        let mut answer = hostkeeper_link::testing::verdict(order, target, Outcome::Failed);
        answer.reason = reason("nothing_running");
        answer.detail = Some("no order of x runs".to_owned());
        answer
    }));
    let rig = Rig::with_world("typed-abort-idle", world);
    rig.install(vec![recorded("x", "1.3")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(abort("abort-1", "x")), &[])));
    rig.world
        .link
        .answer(Ok(reply(Some(abort("abort-2", "web")), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.targets(), [version("1.3")]);
    heartbeater.beat_once();
    assert_eq!(verdicts(&rig), Vec::<OrderResult>::new());
    assert_eq!(
        rig.world.log.lines(),
        [
            "order abort-2 try 1 of web is not run: no order of web runs and it is not \
             installed on this host",
            "order abort-1 try 1 of x answered failed: nothing_running (no order of x runs)",
        ]
    );
}

/// Catches an abort whose plain answer is logged with words it does not have: an answer with no
/// reason and no detail is logged as its outcome alone.
#[test]
fn an_abort_answered_with_no_reason_is_logged_as_its_outcome() {
    let rig = Rig::new("typed-abort-plain");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(abort("abort-1", "x")), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    assert_eq!(
        rig.world.log.lines(),
        ["order abort-1 try 1 of x answered succeeded"]
    );
}

/// Catches an install held back by the rule for orders of services not installed when its
/// service is listed (the executor decides whether that install is a resend or a refusal), and an
/// install that arrives while another order runs started beside it: the first goes to the
/// executor at its own version; the second is `busy` at its own version.
#[test]
fn an_install_of_a_listed_service_runs_and_one_while_busy_is_busy() {
    let rig = Rig::new("typed-install-listed");
    rig.install(vec![recorded("web", "1.0"), recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let listed = install("install-1", 1, "web", "2.0");
    let busy = install("install-2", 1, "x", "3.0");
    rig.world.link.answer(Ok(reply(Some(listed.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(busy.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), [listed]);
    assert_eq!(rig.world.orders.targets(), [version("2.0")]);
    assert_eq!(
        rig.world.link.beats()[2].services[1].last_update,
        Some(OrderResult::busy(&busy, version("3.0")))
    );
}

/// Catches a deferred uninstall dropped like a succeeded one, or one sent again before its
/// retry time: its verdict is sent at the version the service runs, and the same uninstall sent
/// again while the deferral stands is not run.
#[test]
fn a_deferred_uninstall_is_reported_and_not_run_again_before_its_retry_time() {
    let world = World::with_orders(FakeSink::new(|order, target| {
        let mut result = hostkeeper_link::testing::verdict(
            order,
            target,
            Outcome::Deferred {
                retry_after_secs: 600,
            },
        );
        result.reason = reason("drain_bound");
        result
    }));
    let rig = Rig::with_world("typed-uninstall-deferred", world);
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let removal = uninstall("uninstall-1", 1, "x");
    let again = uninstall("uninstall-1", 2, "x");
    rig.world
        .link
        .answer(Ok(reply(Some(removal.clone()), &[Feature::DeferredV1])));
    rig.world
        .link
        .answer(Ok(reply(Some(again), &[Feature::DeferredV1])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders(), [removal]);
    let report = rig.world.link.beats()[1].services[0].deferred.clone();
    assert_eq!(report.map(|d| d.target), Some(version("1.0")));
    let until = hostkeeper_link::testing::START_UNIX + 600;
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "order uninstall-1 try 2 of x is not run: deferred until unix second {until}"
        )]
    );
}

/// Catches an abort of one service given the target of another service's running order, and
/// an abort sent twice run once only by the loop (the executor owns the answer to a resend):
/// an abort of `y` while an update of `x` runs is given `y`'s listed version, and each of the
/// two arrivals goes to the executor.
#[test]
fn an_abort_of_another_service_names_its_own_version_and_each_arrival_runs() {
    let rig = Rig::new("typed-abort-other");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "4.2")]);
    let mut heartbeater = rig.heartbeater();
    let stop = abort("abort-1", "y");
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 1, "x", "2.0")), &[])));
    rig.world.link.answer(Ok(reply(Some(stop.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(stop), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 3);
    while rig.world.dispatch.run_next() {}
    assert_eq!(
        rig.world.orders.targets(),
        [version("2.0"), version("4.2"), version("4.2")]
    );
}
