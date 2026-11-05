//! The features: what every heartbeat announces, and what the loop uses only while the last
//! reply advertised it.

use hostkeeper_hook::Phase;
use hostkeeper_link::testing::{FakeSink, World, verdict};
use hostkeeper_link::{
    ANNOUNCED, DeferredReport, Feature, Features, Outcome, Reason, Reply, ServiceFacts,
};

use crate::{Rig, forced, recorded, reply, service, update, version};

fn only_service(rig: &Rig, beat: usize) -> ServiceFacts {
    rig.world.link.beats()[beat].services[0].clone()
}

/// Catches an announced list that is not the five names in their order, and a loop that uses a
/// feature its last reply did not advertise: with none advertised a deferral is reported as a
/// failure before the commit point with no deferral report, a halt generation is not kept and a
/// forced order is not run; once `deferred-v1` is advertised the deferral is reported as one;
/// once it is no longer advertised the old shape is back.
#[test]
fn features_announced_are_the_five_names_and_a_behaviour_needs_the_reply_to_advertise_it() {
    let names: Vec<&str> = ANNOUNCED.iter().map(|f| f.as_str()).collect();
    assert_eq!(
        names,
        [
            "deferred-v1",
            "halt-v1",
            "heartbeat-v2",
            "forced-v1",
            "order-v1"
        ]
    );

    let world = World::with_orders(FakeSink::new(|order| {
        let mut result = verdict(
            order,
            Outcome::Deferred {
                retry_after_secs: 600,
            },
        );
        result.reason = Some(Reason::new("drain_bound").expect("a reason"));
        result
    }));
    let rig = Rig::with_world("features-advertised", world);
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = update("o-1", 1, "x", "2.0");

    // Beat 0: the coordinator advertises nothing and orders an update, which is deferred.
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    // Beat 1: the old shapes. The reply holds a halt generation and a forced order.
    rig.world.link.answer(Ok(Reply {
        order: Some(forced("o-2", 1, "x", "2.0")),
        halt_generations: vec![(service("x"), 3)],
        ..Reply::default()
    }));
    heartbeater.beat_once();
    let old = only_service(&rig, 1);
    let shown = old.last_update.expect("the verdict");
    assert_eq!(shown.outcome, Outcome::Failed);
    assert_eq!(shown.phase, Some(Phase::BeforeCommit));
    assert_eq!(old.deferred, None);
    assert!(rig.world.halts.raises().is_empty());
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders(), [order]);

    // Beat 2's reply advertises `deferred-v1`: beat 3 reports the deferral as one.
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::DeferredV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let new = only_service(&rig, 3);
    assert_eq!(
        new.last_update.expect("the verdict").outcome,
        Outcome::Deferred {
            retry_after_secs: 600
        }
    );
    assert_eq!(
        new.deferred,
        Some(DeferredReport {
            target: version("2.0"),
            reason: Reason::new("drain_bound").expect("a reason"),
            retry_after_secs: 600,
        })
    );

    // Beat 3's reply advertises nothing again: beat 4 is in the old shape.
    heartbeater.beat_once();
    let again = only_service(&rig, 4);
    assert_eq!(again.deferred, None);
    assert_eq!(
        again.last_update.expect("the verdict").outcome,
        Outcome::Failed
    );
    for beat in rig.world.link.beats() {
        assert_eq!(beat.features, ANNOUNCED.to_vec());
    }
    assert_eq!(
        rig.world.log.lines(),
        [
            "order o-2 try 1 of x is not run: it is forced and the coordinator does not \
             advertise forced-v1"
        ]
    );
}

/// Catches a feature read from a name it does not have, or a name the daemon does not know
/// read as a feature.
#[test]
fn feature_names_read_back_and_an_unknown_name_is_no_feature() {
    for feature in ANNOUNCED {
        assert_eq!(Feature::parse(feature.as_str()), Some(feature));
    }
    assert_eq!(Feature::parse("deferred-v2"), None);
    assert_eq!(Feature::parse(""), None);
}

/// Catches a halt shown to a coordinator that did not advertise `halt-v1` as a halt (kbf's
/// coordinator reads it as a plain failure), or hidden from one that did; and a phase other
/// than the halt changed.
#[test]
fn a_halt_is_shown_as_one_only_with_halt_v1() {
    let mut halted = verdict(&update("o-1", 1, "x", "2.0"), Outcome::Failed);
    halted.phase = Some(Phase::Halt);
    assert_eq!(halted.shaped(&Features::default()).phase, None);
    assert_eq!(
        halted.shaped(&Features::of(&[Feature::HaltV1])).phase,
        Some(Phase::Halt)
    );
    let mut failed = halted;
    failed.phase = Some(Phase::AfterCommit);
    assert_eq!(failed.shaped(&Features::default()), failed);
    let succeeded = verdict(&update("o-1", 1, "x", "2.0"), Outcome::Succeeded);
    assert_eq!(succeeded.shaped(&Features::default()), succeeded);
}
