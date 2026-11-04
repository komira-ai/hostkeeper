//! The verdict of an order: in every heartbeat until the coordinator acknowledges it by order id
//! and try, and again after a restart.

use hostkeeper_link::testing::verdict;
use hostkeeper_link::{OrderResult, Outcome, RecordedService, Reply};

use crate::{Rig, recorded, reference, reply, update};

fn acknowledging(refs: Vec<hostkeeper_link::OrderRef>) -> Reply {
    Reply {
        acknowledged: refs,
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

/// Catches a verdict lost with the daemon's memory: a new loop over the same step records
/// resends the verdict the record kept, does not run its order again when the coordinator
/// resends it, and stops once it is acknowledged, though the record still holds it.
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
        acknowledged: vec![reference("o-1", 1)],
        ..Reply::default()
    }));
    second.beat_once();
    second.beat_once();
    assert_eq!(last_update(&rig, 0), Some(kept.clone()));
    assert_eq!(last_update(&rig, 1), Some(kept));
    assert_eq!(last_update(&rig, 2), None);
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert!(rig.world.orders.orders().is_empty());
}
