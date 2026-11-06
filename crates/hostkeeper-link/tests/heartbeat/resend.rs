//! The verdict of an order: in every heartbeat until the coordinator acknowledges it by order id
//! and try, and again after a restart.

use crate::verdict;
use hostkeeper_link::{
    ACKED_KEPT, Acknowledged, Feature, OrderResult, Outcome, RecordedService, Reply,
};

use crate::{Rig, recorded, reference, reply, service, update, version};

/// A reply acknowledging `refs`, each as a verdict about service `x`.
fn acknowledging(refs: Vec<hostkeeper_link::OrderRef>) -> Reply {
    acknowledging_of("x", refs)
}

fn acknowledging_of(name: &str, refs: Vec<hostkeeper_link::OrderRef>) -> Reply {
    Reply {
        acknowledged: refs
            .into_iter()
            .map(|order| Acknowledged {
                service: service(name),
                order,
            })
            .collect(),
        ..Reply::default()
    }
}

fn last_update(rig: &Rig, beat: usize) -> Option<OrderResult> {
    rig.world.link.beats()[beat].services[0].last_update.clone()
}

/// Catches a verdict that stops on any acknowledgement, on one of another try or another order,
/// or never stops: it is sent until the reply names its order id and its try.
#[test]
fn a_verdict_is_resent_until_acknowledged_by_order_id_and_try() {
    let rig = Rig::new("resend-ack");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    let expected = verdict(&order, Outcome::Succeeded);
    rig.world.link.answer(Ok(reply(Some(order), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());

    // Beat 1 carries it; its reply acknowledges another try of the order.
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-1", 2)])));
    heartbeater.beat_once();
    // Beat 2 carries it; its reply acknowledges another order with the same try.
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-2", 1)])));
    heartbeater.beat_once();
    // Beat 3 carries it; its reply acknowledges it.
    rig.world.link.answer(Ok(acknowledging(vec![
        reference("o-2", 1),
        reference("o-1", 1),
    ])));
    heartbeater.beat_once();
    // Beats 4 and 5 do not.
    heartbeater.beat_once();
    heartbeater.beat_once();
    for beat in 1..=3 {
        assert_eq!(
            last_update(&rig, beat),
            Some(expected.clone()),
            "beat {beat}"
        );
    }
    assert_eq!(last_update(&rig, 4), None);
    assert_eq!(last_update(&rig, 5), None);
}

/// Catches an acknowledgement matched by order id and try alone: two services can each have
/// `o-1` try 1, and an acknowledgement naming `y` must not stop the verdict of `x`; the one
/// naming `x` does.
#[test]
fn an_acknowledgement_of_another_service_does_not_stop_this_services_verdict() {
    let rig = Rig::new("resend-ack-service");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");
    let expected = verdict(&order, Outcome::Succeeded);
    rig.world.link.answer(Ok(reply(Some(order), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());

    rig.world
        .link
        .answer(Ok(acknowledging_of("y", vec![reference("o-1", 1)])));
    heartbeater.beat_once();
    rig.world
        .link
        .answer(Ok(acknowledging_of("x", vec![reference("o-1", 1)])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(last_update(&rig, 1), Some(expected.clone()));
    assert_eq!(last_update(&rig, 2), Some(expected));
    assert_eq!(last_update(&rig, 3), None);
}

/// Catches a verdict lost with the daemon's memory: a new loop over the same step records
/// resends the verdict the record kept, and stops once it is acknowledged, though the record
/// still holds it.
#[test]
fn a_verdict_survives_a_daemon_restart_and_is_resent() {
    let rig = Rig::new("resend-restart");
    let order = update("o-1", 1, "x", "2.0");
    let kept = verdict(&order, Outcome::Failed);
    rig.install(vec![RecordedService {
        last_result: Some(kept.clone()),
        ..recorded("x", "1.0")
    }]);
    let mut first = rig.heartbeater();
    first.beat_once();
    drop(first);

    let mut second = rig.heartbeater();
    rig.world.link.answer(Ok(Reply {
        order: Some(order),
        acknowledged: vec![Acknowledged {
            service: service("x"),
            order: reference("o-1", 1),
        }],
        ..Reply::default()
    }));
    second.beat_once();
    second.beat_once();
    assert_eq!(last_update(&rig, 0), Some(kept.clone()));
    assert_eq!(last_update(&rig, 1), Some(kept));
    assert_eq!(last_update(&rig, 2), None);
    // The coordinator's resend of the order goes to the executor, which answers it from its
    // record (the try rules are the executor's); the loop has no memory of it to answer with.
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a verdict lost because another came for the same service before it was counted: a
/// `busy` answer then the verdict of the running order, in one heartbeat period; and two
/// verdicts of finished orders. The newest is sent; once it is counted the one before it is.
#[test]
fn every_verdict_of_a_service_is_sent_until_acknowledged() {
    let rig = Rig::new("resend-several");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = update("o-1", 1, "x", "2.0");
    let second = update("o-2", 1, "x", "3.0");
    rig.world.link.answer(Ok(reply(Some(first.clone()), &[])));
    rig.world.link.answer(Ok(reply(Some(second.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    // The busy answer to the second, then the first's verdict, before the next heartbeat.
    assert!(rig.world.dispatch.run_next());
    let done = verdict(&first, Outcome::Succeeded);
    let busy = OrderResult::busy(&second, version("3.0"));
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-1", 1)])));
    heartbeater.beat_once();
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-2", 1)])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(last_update(&rig, 2), Some(done));
    assert_eq!(last_update(&rig, 3), Some(busy));
    assert_eq!(last_update(&rig, 4), None);

    // Two finished orders, neither counted: the newer first, then the older.
    let third = update("o-3", 1, "x", "3.0");
    let fourth = update("o-4", 1, "x", "4.0");
    rig.world.link.answer(Ok(reply(Some(third.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world.link.answer(Ok(reply(Some(fourth.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-4", 1)])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(
        last_update(&rig, 6),
        Some(verdict(&third, Outcome::Succeeded))
    );
    assert_eq!(
        last_update(&rig, 7),
        Some(verdict(&fourth, Outcome::Succeeded))
    );
    assert_eq!(
        last_update(&rig, 8),
        Some(verdict(&third, Outcome::Succeeded))
    );
}

/// A reply of a coordinator that reads the verdict it acknowledged as standing
/// (`verdict-acks-v1`), with `order` and acknowledging `refs` of service `x`.
fn reading(order: Option<hostkeeper_link::Order>, refs: Vec<hostkeeper_link::OrderRef>) -> Reply {
    Reply {
        order,
        features: vec![Feature::VerdictAcksV1],
        ..acknowledging(refs)
    }
}

/// Catches the newest verdict heard before an older one by a coordinator that keeps the last
/// verdict it acknowledged as the host's report: there, an older verdict would stand in place of
/// the newest. To a coordinator that advertises `verdict-acks-v1`, the verdicts of a service go
/// oldest first, each until it is acknowledged, so the newest is the last heard.
#[test]
fn to_a_coordinator_that_keeps_the_last_ack_verdicts_go_oldest_first() {
    let rig = Rig::new("resend-oldest-first");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let third = update("o-3", 1, "x", "3.0");
    let fourth = update("o-4", 1, "x", "4.0");
    rig.world
        .link
        .answer(Ok(reading(Some(third.clone()), vec![])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world
        .link
        .answer(Ok(reading(Some(fourth.clone()), vec![])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world
        .link
        .answer(Ok(reading(None, vec![reference("o-3", 1)])));
    heartbeater.beat_once();
    rig.world
        .link
        .answer(Ok(reading(None, vec![reference("o-4", 1)])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(
        last_update(&rig, 2),
        Some(verdict(&third, Outcome::Succeeded))
    );
    assert_eq!(
        last_update(&rig, 3),
        Some(verdict(&fourth, Outcome::Succeeded))
    );
    assert_eq!(last_update(&rig, 4), None);
}

/// Catches an older verdict sent after the newest was acknowledged by a coordinator that keeps
/// the last ack: the newest goes first while no reply has advertised `verdict-acks-v1`, and
/// the reply that first advertises it and acknowledges the newest ends the older one too, so
/// it is never heard after it. A verdict of another service stays.
#[test]
fn an_ack_of_the_newest_ends_the_older_verdicts_of_its_service() {
    let rig = Rig::new("resend-older-ended");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let third = update("o-3", 1, "x", "3.0");
    let fourth = update("o-4", 1, "x", "4.0");
    rig.world.link.answer(Ok(reply(Some(third), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world.link.answer(Ok(reply(Some(fourth.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world
        .link
        .answer(Ok(reading(None, vec![reference("o-4", 1)])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(
        last_update(&rig, 2),
        Some(verdict(&fourth, Outcome::Succeeded))
    );
    assert_eq!(last_update(&rig, 3), None);
}

/// Runs `order` and the heartbeat that takes its verdict, whose reply acknowledges
/// `acknowledged`.
fn run_then(
    rig: &Rig,
    heartbeater: &mut hostkeeper_link::Heartbeater,
    order: &hostkeeper_link::Order,
    acknowledged: Vec<hostkeeper_link::OrderRef>,
) {
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world.link.answer(Ok(acknowledging(acknowledged)));
    heartbeater.beat_once();
}

/// Catches an acknowledgement of an older verdict replacing the one remembered for the step
/// record's newer verdict: acknowledged newest first, neither is sent again when the service
/// leaves and comes back with its record. Past [`ACKED_KEPT`] acknowledgements the oldest is
/// forgotten, and a record still holding it is sent once more (the coordinator counts it once).
#[test]
fn verdicts_acknowledged_newest_first_are_not_sent_again_when_the_service_returns() {
    let rig = Rig::new("resend-ack-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = update("o-1", 1, "x", "2.0");
    let second = update("o-2", 1, "x", "3.0");
    run_then(&rig, &mut heartbeater, &first, Vec::new());
    run_then(&rig, &mut heartbeater, &second, vec![reference("o-2", 1)]);
    rig.world
        .link
        .answer(Ok(acknowledging(vec![reference("o-1", 1)])));
    heartbeater.beat_once();
    let back = RecordedService {
        last_result: Some(verdict(&second, Outcome::Succeeded)),
        ..recorded("x", "3.0")
    };
    rig.world.records.set(Vec::new());
    heartbeater.beat_once();
    rig.world.records.set(vec![back]);
    heartbeater.beat_once();
    assert_eq!(
        last_update(&rig, 3),
        Some(verdict(&second, Outcome::Succeeded))
    );
    assert_eq!(
        last_update(&rig, 4),
        Some(verdict(&first, Outcome::Succeeded))
    );
    assert!(rig.world.link.beats()[5].services.is_empty());
    assert_eq!(last_update(&rig, 6), None);

    // Past the bound: the oldest acknowledgement is forgotten.
    let rig = Rig::new("resend-ack-bound");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let orders: Vec<_> = (1..=ACKED_KEPT + 1)
        .map(|n| update(&format!("o-{n}"), 1, "x", "2.0"))
        .collect();
    for order in &orders {
        run_then(&rig, &mut heartbeater, order, vec![order.reference()]);
    }
    let kept = |order: &hostkeeper_link::Order| RecordedService {
        last_result: Some(verdict(order, Outcome::Succeeded)),
        ..recorded("x", "2.0")
    };
    for (order, sent) in [
        (&orders[1], None),
        (&orders[0], Some(verdict(&orders[0], Outcome::Succeeded))),
    ] {
        rig.world.records.set(Vec::new());
        heartbeater.beat_once();
        rig.world.records.set(vec![kept(order)]);
        heartbeater.beat_once();
        let beats = rig.world.link.beats();
        assert_eq!(beats[beats.len() - 1].services[0].last_update, sent);
    }
}
