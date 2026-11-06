//! The try of each order the coordinator sends, per host and service as the daemon keys it
//! (design 8.3): one past the try of the last verdict the host reports for the service, whatever
//! order that verdict ended, so the update, force or uninstall after an install is not refused
//! `try_conflict` (the same try of another package) or `stale_try` (a lower one). Every time is
//! the rig's clock; every transport is the in-process one, with the host's identity explicit.

use hostkeeper_wire::wire::{
    ORDER_FEATURE, OrderId, Outcome, ServiceBeat, ServiceReply, UpdateResult,
};

use crate::assign_support::{order_to, unassign, uninstall_order};
use crate::marks_support::{
    INSTALLER, WEB, ask_typed, beat_of, digest, fleet_target, force_request, operator, restart,
    rig, runs, set,
};
use crate::order_terms::order_id_text;
use crate::orders::{Order, install_order_id};
use crate::registry::Digest;
use crate::testutil::{Rig, version};

/// What a daemon of this release announces: the typed verbs and the order terms.
const DAEMON: &[&str] = &[
    "heartbeat-v2",
    "halt-v1",
    "deferred-v1",
    "forced-v1",
    "abort-v1",
    "install-v1",
    ORDER_FEATURE,
];

/// `web` as a host reports it: it runs `running`, healthy, and the last verdict it reports ended
/// the order `order_id` at try `order_try`, with `outcome`, targeting `running`.
fn after(running: &str, order_id: &str, order_try: u64, outcome: Outcome) -> ServiceBeat {
    let mut one = runs(WEB, running);
    let mut verdict = UpdateResult::new(version(running), outcome);
    verdict.order_id = OrderId::parse(order_id);
    verdict.order_try = Some(order_try);
    one.last_update = Some(verdict);
    one
}

/// The order id of the install of the package [`digest`] `n`.
fn install_id(n: u8) -> String {
    install_order_id(&Digest::parse(&digest(n)).expect("a digest"))
}

/// The update of `web` to `to`, whose package is [`digest`] `n`, at try `order_try`.
fn update_order(to: &str, n: u8, order_try: u64) -> Order {
    Order::Update {
        service: WEB.to_owned(),
        version: version(to),
        digest: hostkeeper_wire::wire::PackageDigest::parse(&digest(n)).expect("a digest"),
        index_digest: None,
        order_id: order_id_text(WEB, &version(to)),
        order_try,
    }
}

/// The reply and the typed order a daemon of this release that lists `services` is answered
/// with.
fn told(rig: &Rig, host: &str, services: Vec<ServiceBeat>) -> (ServiceReply, Option<Order>) {
    ask_typed(rig, host, "daemon", beat_of(DAEMON, services))
}

/// The update reply of `web` to `to`.
fn update_reply(to: &str) -> ServiceReply {
    ServiceReply::Update {
        service: Some(WEB.to_owned()),
        version: version(to),
    }
}

/// A host that installed `web` 2.0 (try 1) and reports it is ordered the update to 2.1 at try 2,
/// and that order is resent at try 2 until the host reports a verdict of it; that verdict, try 2,
/// moves the next update to try 3.
///
/// Catches: the update after an install numbered from 1 again (the daemon's step record holds the
/// install's try 1 and refuses another package at it `try_conflict`, so the host never updates),
/// a resend that raises the try before a verdict, and a try that does not follow the update's own
/// verdict.
#[tokio::test]
async fn an_update_after_an_install_is_ordered_one_try_past_it() {
    let rig = rig("try-install-update", "");
    set(&rig, WEB, "2.0", 7).await;
    let install = told(&rig, "d1", vec![]).1.expect("an install");
    assert!(
        matches!(&install, Order::Install { order_try: 1, order_id, .. } if *order_id == install_id(7)),
        "{install:?}"
    );

    set(&rig, WEB, "2.1", 8).await;
    let installed = || vec![after("2.0", &install_id(7), 1, Outcome::Succeeded)];
    for _ in 0..2 {
        assert_eq!(
            told(&rig, "d1", installed()),
            (update_reply("2.1"), Some(update_order("2.1", 8, 2)))
        );
    }

    set(&rig, WEB, "2.2", 9).await;
    let updated = vec![after(
        "2.1",
        &order_id_text(WEB, &version("2.1")),
        2,
        Outcome::Succeeded,
    )];
    assert_eq!(
        told(&rig, "d1", updated),
        (update_reply("2.2"), Some(update_order("2.2", 9, 3)))
    );
}

/// A coordinator that restarted remembers no tries: a host that reports a verdict at try 6 of an
/// order the new coordinator never sent is ordered try 7, and one that reports no verdict try 1.
///
/// Catches: a restarted coordinator that orders from try 1 (refused `stale_try` by a daemon whose
/// record holds try 6), and one that keeps a try in memory across hosts.
#[tokio::test]
async fn a_restarted_coordinator_orders_one_past_the_highest_try_the_host_reports() {
    let mut rig = rig("try-restart", "");
    set(&rig, WEB, "2.1", 8).await;
    restart(&mut rig, "");
    rig.at(2_100);
    assert_eq!(
        told(
            &rig,
            "d1",
            vec![after("2.0", &install_id(3), 6, Outcome::Succeeded)]
        ),
        (update_reply("2.1"), Some(update_order("2.1", 8, 7)))
    );
    // One live update at a time: the second host is ordered once the first runs the version.
    told(
        &rig,
        "d1",
        vec![after(
            "2.1",
            &order_id_text(WEB, &version("2.1")),
            7,
            Outcome::Succeeded,
        )],
    );
    assert_eq!(
        told(&rig, "d2", vec![runs(WEB, "2.0")]),
        (update_reply("2.1"), Some(update_order("2.1", 8, 1)))
    );
}

/// A force of `web` to a host that installed it at try 1 carries try 2; a force to a host that
/// reports a failed update at try 4 carries try 5.
///
/// Catches: a force always sent at try 1, which a daemon whose record holds the install's try 1
/// refuses `try_conflict` and one whose record holds a higher try refuses `stale_try`.
#[tokio::test]
async fn a_force_after_an_install_or_an_update_is_ordered_one_try_past_it() {
    let rig = rig("try-force", "");
    set(&rig, WEB, "2.0", 7).await;
    set(&rig, WEB, "2.1", 8).await;
    let installed = || vec![after("2.0", &install_id(7), 1, Outcome::Succeeded)];
    let failed = || {
        vec![after(
            "2.0",
            &order_id_text(WEB, &version("2.1")),
            4,
            Outcome::Failed,
        )]
    };
    told(&rig, "d1", installed());
    told(&rig, "d2", failed());
    rig.coordinator
        .force(
            &operator(),
            force_request(WEB, "2.1", fleet_target(WEB), false),
        )
        .expect("the force is accepted");

    let try_of = |order: Option<Order>| match order {
        Some(Order::Force {
            order_try,
            order_id,
            ..
        }) => {
            assert_eq!(order_id, "force-1");
            order_try
        }
        other => panic!("a force, not {other:?}"),
    };
    assert_eq!(try_of(told(&rig, "d1", installed()).1), 2);
    assert_eq!(try_of(told(&rig, "d2", failed()).1), 5);
    assert_eq!(
        try_of(told(&rig, "d1", installed()).1),
        2,
        "a resend keeps its try"
    );
}

/// An uninstall of `web` from a host that reports its update at try 3 carries try 4.
///
/// Catches: an uninstall numbered from 1 because the host never reported a verdict of the
/// uninstall's own order id (the daemon keeps the step record's try and refuses a lower one).
#[tokio::test]
async fn an_uninstall_after_an_update_is_ordered_one_try_past_it() {
    let rig = rig("try-uninstall", "");
    let updated = || {
        vec![after(
            "1.0",
            &order_id_text(WEB, &version("1.0")),
            3,
            Outcome::Succeeded,
        )]
    };
    assert_eq!(order_to(&rig, "d1", updated()), None);
    unassign(&rig, WEB, "", "daemon", true).expect("taken off the daemons");
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", beat_of(INSTALLER, updated())).1,
        Some(uninstall_order(WEB, "uninstall-1", 4))
    );
}
