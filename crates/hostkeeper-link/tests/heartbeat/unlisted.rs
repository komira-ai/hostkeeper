//! The verdict of a service the step records do not list: an install that ended before it placed
//! anything (a refusal writes no record), or a service that left the records while its order
//! ran. It is sent at once under an entry of the service absent at the verdict's target, until
//! the coordinator acknowledges it; nothing is listed for an unlisted service with no verdict.

use hostkeeper_link::testing::{FakeSink, World};
use hostkeeper_link::{
    Acknowledged, Feature, Health, Liveness, Observed, Order, OrderResult, Outcome, Reason, Reply,
};

use crate::{Rig, install, recorded, reply, service, update, verdict, version};

/// The entry of `name` in heartbeat `at`, if it lists one.
fn entry(rig: &Rig, at: usize, name: &str) -> Option<hostkeeper_link::ServiceFacts> {
    rig.world.link.beats()[at]
        .services
        .iter()
        .find(|facts| facts.name == service(name))
        .cloned()
}

fn acknowledging(name: &str, result: &OrderResult, features: &[Feature]) -> Reply {
    Reply {
        acknowledged: vec![Acknowledged {
            service: service(name),
            order: result.order.clone(),
        }],
        features: features.to_vec(),
        ..Reply::default()
    }
}

/// An executor that refuses every order the way the daemon refuses an install that conflicts
/// with a placed path: `Failed` before the commit, of class release, `path_conflict`, naming
/// the version the loop gave.
fn refusing() -> FakeSink {
    FakeSink::new(|order: &Order, target| {
        let mut result = hostkeeper_link::testing::verdict(order, target, Outcome::Failed);
        result.reason = Some(Reason::new("path_conflict").expect("a reason"));
        result
    })
}

/// Catches the verdict of a refused install never reaching the coordinator, and an absent entry
/// that goes with its verdict (the coordinator would then hear nothing of the service, forget the
/// failure and order the same install again): the refusal is sent in the next heartbeat under
/// `web` absent at the install's target, the same until the coordinator acknowledges it; then
/// `web` is still listed absent there, with no verdict, which the coordinator reads as still
/// reporting the one it acknowledged; once the records list `web` the absent entry goes.
#[test]
fn a_refused_install_is_sent_until_acknowledged_and_its_absent_entry_stays_until_listed() {
    let rig = Rig::with_world("unlisted-refused", World::with_orders(refusing()));
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let order = install("install-hk", 1, "web", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    let refused = OrderResult {
        reason: Some(Reason::new("path_conflict").expect("a reason")),
        ..verdict(&order, Outcome::Failed)
    };
    rig.world.link.answer(Ok(Reply::default()));
    heartbeater.beat_once();
    rig.world
        .link
        .answer(Ok(acknowledging("web", &refused, &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    rig.install(vec![recorded("web", "2.0"), recorded("x", "1.0")]);
    heartbeater.beat_once();

    let absent = |last_update: Option<OrderResult>| {
        Some(hostkeeper_link::ServiceFacts {
            name: service("web"),
            version: version("2.0"),
            health: Health::Absent,
            updating: None,
            last_update,
            deferred: None,
            health_detail: None,
            observed: Some(Observed {
                liveness: Liveness::Absent,
                version: None,
            }),
            record_try: None,
            installer: None,
            drift: None,
        })
    };
    for at in [1, 2] {
        assert_eq!(entry(&rig, at, "web"), absent(Some(refused.clone())));
    }
    assert_eq!(entry(&rig, 3, "web"), absent(None));
    let listed = entry(&rig, 4, "web").expect("web is listed");
    assert_ne!(listed.health, Health::Absent);
    assert_eq!(listed.last_update, None);
    assert_eq!(rig.world.link.beats()[4].services.len(), 2);
}

/// Catches a `busy` answer kept unsent for a service the records do not list: an install of
/// `web` that comes while an update of `x` runs is answered `busy`, and the next heartbeat
/// lists `web` absent at the install's target with that answer.
#[test]
fn a_busy_answer_to_an_install_of_an_unlisted_service_is_sent() {
    let rig = Rig::new("unlisted-busy");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let running = update("o-1", 1, "x", "2.0");
    let refused = install("install-1", 1, "web", "3.0");
    rig.world.link.answer(Ok(reply(Some(running), &[])));
    rig.world.link.answer(Ok(reply(Some(refused.clone()), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    heartbeater.beat_once();
    let web = entry(&rig, 2, "web").expect("web is listed");
    assert_eq!((web.version, web.health), (version("3.0"), Health::Absent));
    assert_eq!(
        web.last_update,
        Some(OrderResult::busy(&refused, version("3.0")))
    );
}

/// Catches an absent entry built for a service with nothing to say, or one whose order still
/// runs: while the install runs, and before it, the heartbeat lists only the installed services.
#[test]
fn an_unlisted_service_with_no_verdict_is_not_listed() {
    let rig = Rig::new("unlisted-none");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(Some(install("install-1", 1, "web", "2.0")), &[])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    for at in [0, 1] {
        assert_eq!(entry(&rig, at, "web"), None);
    }
}

/// Catches an absent entry that drops the deferral of a deferred install, or a deferral sent to
/// a coordinator that did not ask for it: with `deferred-v1` the entry carries the verdict and
/// the deferral of its target; without it, the verdict shaped as a failure and no deferral.
#[test]
fn a_deferred_install_is_sent_with_its_deferral_only_when_the_coordinator_reads_one() {
    for (features, deferred) in [(vec![Feature::DeferredV1], true), (vec![], false)] {
        let world = World::with_orders(FakeSink::new(|order: &Order, target| {
            let mut result = hostkeeper_link::testing::verdict(
                order,
                target,
                Outcome::Deferred {
                    retry_after_secs: 600,
                },
            );
            result.reason = Some(Reason::new("drain_bound").expect("a reason"));
            result
        }));
        let rig = Rig::with_world(&format!("unlisted-deferred-{deferred}"), world);
        rig.install(vec![recorded("x", "1.0")]);
        rig.make_service_dir("web");
        let mut heartbeater = rig.heartbeater();
        let order = install("install-1", 1, "web", "2.0");
        rig.world
            .link
            .answer(Ok(reply(Some(order.clone()), &features)));
        heartbeater.beat_once();
        assert!(rig.world.dispatch.run_next());
        heartbeater.beat_once();
        let web = entry(&rig, 1, "web").expect("web is listed");
        let sent = web.last_update.expect("the verdict");
        assert_eq!(sent.order, order.reference());
        assert_eq!(
            matches!(sent.outcome, Outcome::Deferred { .. }),
            deferred,
            "{sent:?}"
        );
        assert_eq!(
            web.deferred.map(|report| report.target),
            deferred.then(|| version("2.0"))
        );
    }
}

/// Catches the order of verdicts lost for an unlisted service: a `busy` answer to an install
/// that came while another order ran, and that order's own verdict, are sent oldest first to a
/// coordinator that advertises `verdict-acks-v1`, the next once the first is acknowledged.
#[test]
fn the_verdicts_of_an_unlisted_service_go_oldest_first_with_verdict_acks() {
    let rig = Rig::new("unlisted-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let first = install("install-1", 1, "web", "2.0");
    let second = install("install-2", 1, "web", "3.0");
    let acks = [Feature::VerdictAcksV1];
    rig.world.link.answer(Ok(reply(Some(first.clone()), &acks)));
    rig.world
        .link
        .answer(Ok(reply(Some(second.clone()), &acks)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    let busy = OrderResult::busy(&second, version("3.0"));
    rig.world
        .link
        .answer(Ok(acknowledging("web", &busy, &acks)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(
        entry(&rig, 2, "web").and_then(|web| web.last_update),
        Some(busy)
    );
    assert_eq!(
        entry(&rig, 3, "web").and_then(|web| web.last_update),
        Some(verdict(&first, Outcome::Succeeded))
    );
}
