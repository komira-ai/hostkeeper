//! A refusal of an order for its try alone (`try_conflict`, `stale_try`: the daemon's step record
//! holds the order's try for another package, or a higher one) is a disagreement on the try, not
//! the host failing the release: the coordinator orders the host again past the try, and neither
//! the host's gate (`failed_here`), the plan (a failed host, so `stage_failed` for the others) nor
//! the rollback counts it. And the try a daemon's step record holds, which it sends in every
//! heartbeat, starts a restarted coordinator's first order of the host past it, so a host whose
//! last order's verdict the coordinator never heard (the install of a self-update, which the
//! daemon ran across its own restart) is not refused at all. Every time is the rig's clock; every
//! transport is the in-process one, with the host's identity explicit.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{
    Class, Heartbeat, ORDER_FEATURE, OrderId, Outcome, Phase, ServiceBeat, ServiceReply, Token,
    UpdateResult, VerdictAck,
};
use serde_json::{Value, json};

use crate::fleet::Fleet;
use crate::grpc::report;
use crate::marks_support::{WEB, beat_of, digest, native_target, restart, rig, set, who};
use crate::order_terms::order_id_text;
use crate::orders::Order;
use crate::picks::TARGET_FEATURE;
use crate::testutil::{OPERATOR, Rig, id, version};
use crate::verdict_file::FEATURE as VERDICT_ACKS;

/// What a daemon of this release announces: the typed verbs, the order terms and the acks.
const DAEMON: &[&str] = &[
    "heartbeat-v2",
    "halt-v1",
    "deferred-v1",
    "forced-v1",
    "abort-v1",
    "install-v1",
    ORDER_FEATURE,
    VERDICT_ACKS,
    TARGET_FEATURE,
];

/// `web` as a host reports it: it runs `running`, healthy, with the fields of `extra`
/// (`record_try`) added, and `last` as its verdict.
fn web(running: &str, extra: &Value, last: Option<UpdateResult>) -> ServiceBeat {
    let mut one = json!({"name": WEB, "version": running, "health": "healthy"});
    one.as_object_mut()
        .expect("an object")
        .extend(extra.as_object().expect("an object").clone());
    let mut one: ServiceBeat = serde_json::from_value(one).expect("a service beat");
    one.last_update = last;
    one
}

/// The daemon's refusal of the update of `web` to `target` at try `order_try`, for `reason`: failed,
/// with no phase and no class, as the daemon reports every refusal.
fn refusal(target: &str, order_try: u64, reason: &str) -> UpdateResult {
    let mut verdict = UpdateResult::new(version(target), Outcome::Failed);
    verdict.reason = Token::parse(reason);
    verdict.order_id = OrderId::parse(&order_id_text(WEB, &version(target)));
    verdict.order_try = Some(order_try);
    verdict
}

/// The update of `web` to `to`, whose package is [`digest`] 1, at try `order_try`.
fn update_order(to: &str, order_try: u64) -> Order {
    Order::Update {
        target: Some(native_target(WEB, 1)),
        service: WEB.to_owned(),
        version: version(to),
        digest: hostkeeper_wire::wire::PackageDigest::parse(&digest(1)).expect("a digest"),
        index_digest: None,
        order_id: order_id_text(WEB, &version(to)),
        order_try,
    }
}

/// The update reply of `web` to `to`.
fn update_reply(to: &str) -> ServiceReply {
    ServiceReply::Update {
        service: Some(WEB.to_owned()),
        version: version(to),
    }
}

/// The reply, the typed order and the acks a daemon of this release that lists `services` is
/// answered with, as `host` of the role `daemon`.
fn told(
    rig: &Rig,
    host: &str,
    services: Vec<ServiceBeat>,
) -> (ServiceReply, Option<Order>, Vec<VerdictAck>) {
    let (answer, order) = rig
        .coordinator
        .heartbeat_with_orders_native(
            who(host, "daemon"),
            beat_of(DAEMON, services),
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .expect("answered");
    (answer.reply, order, answer.acks)
}

/// The reply and the typed order alone.
fn ordered(rig: &Rig, host: &str, services: Vec<ServiceBeat>) -> (ServiceReply, Option<Order>) {
    let (reply, order, _) = told(rig, host, services);
    (reply, order)
}

/// The gate reason `web`'s status gives `node`.
fn gate_of(rig: &Rig, node: &str) -> proto::GateReason {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("the status");
    let service = report::status(&status, WEB)
        .services
        .into_iter()
        .next()
        .expect("the service");
    let host = service
        .hosts
        .iter()
        .find(|h| h.node_id == node)
        .expect("the host");
    host.view.as_ref().expect("a view").gate_reason()
}

/// The state the plan of `web` gives `node`.
fn plan_state_of(rig: &Rig, node: &str) -> proto::PlanHostState {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("the status");
    let service = report::status(&status, WEB)
        .services
        .into_iter()
        .next()
        .expect("the service");
    let plan = service.plan.expect("a plan");
    plan.stages
        .iter()
        .flat_map(|stage| &stage.members)
        .find(|member| member.node_id == node)
        .expect("a member")
        .state()
}

/// A rig of eight daemons on `web` at 1.0, each heard once, with `web` set to 2.0: the default
/// plan's first stage is `d1` alone, and the others wait for it.
async fn eight_hosts(test: &str) -> Rig {
    let rig = rig(test, "");
    for i in 1..=8 {
        let reply = ordered(&rig, &format!("d{i}"), vec![web("1.0", &json!({}), None)]).0;
        assert_eq!(reply, ServiceReply::Nothing, "d{i}: no desired state yet");
    }
    set(&rig, WEB, "2.0", 1).await;
    rig
}

/// From a daemon that sends no record try (one of an earlier release): a coordinator that numbers the first
/// update of `d1` try 1 hears it refused `try_conflict` at try 1. The same answer orders the
/// update again at try 2; `d1` is not failed here, nor failed in the plan, and the hosts after it
/// are not held back as a failed stage. The refusal resent before the host takes try 2 is told
/// try 2 again, not a third try; and once try 2 succeeds the next host is ordered.
///
/// Catches: a `try_conflict` read as the host failing the version, which halts the whole staged
/// plan over a disagreement on the try; a refusal that is not ordered again; a resent refusal,
/// whose try is older than the order given since, that ends that order and raises the try again.
#[tokio::test]
async fn a_try_conflict_is_ordered_again_past_the_try_and_fails_no_stage() {
    let rig = eight_hosts("try-refusal-conflict").await;
    assert_eq!(
        ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]),
        (update_reply("2.0"), Some(update_order("2.0", 1)))
    );
    let refused = || {
        vec![web(
            "1.0",
            &json!({}),
            Some(refusal("2.0", 1, "try_conflict")),
        )]
    };
    assert_eq!(
        ordered(&rig, "d1", refused()),
        (update_reply("2.0"), Some(update_order("2.0", 2)))
    );
    assert_ne!(gate_of(&rig, "d1"), proto::GateReason::FailedHere);
    assert_ne!(plan_state_of(&rig, "d1"), proto::PlanHostState::Failed);
    for other in ["d2", "d5"] {
        assert_ne!(
            gate_of(&rig, other),
            proto::GateReason::StageFailed,
            "{other}"
        );
    }
    assert_eq!(
        ordered(&rig, "d1", refused()),
        (update_reply("2.0"), Some(update_order("2.0", 2))),
        "a resent refusal keeps the order given since"
    );

    let mut done = UpdateResult::new(version("2.0"), Outcome::Succeeded);
    done.order_id = OrderId::parse(&order_id_text(WEB, &version("2.0")));
    done.order_try = Some(2);
    ordered(&rig, "d1", vec![web("2.0", &json!({}), Some(done))]);
    assert_eq!(gate_of(&rig, "d1"), proto::GateReason::UpToDate);
}

/// A daemon that refuses `stale_try` (its record holds a higher try than the order's) and sends
/// the try its record holds, 5, is ordered again at try 6 in the same answer, not at 2.
///
/// Catches: a `stale_try` read as a failure of the version, and a re-order that numbers past the
/// refused try alone, which a record at 5 refuses `stale_try` again three times more.
#[tokio::test]
async fn a_stale_try_is_ordered_again_past_the_try_the_record_holds() {
    let rig = eight_hosts("try-refusal-stale").await;
    ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]);
    let refused = web(
        "1.0",
        &json!({"record_try": 5}),
        Some(refusal("2.0", 1, "stale_try")),
    );
    assert_eq!(
        ordered(&rig, "d1", vec![refused]),
        (update_reply("2.0"), Some(update_order("2.0", 6)))
    );
    assert_ne!(gate_of(&rig, "d1"), proto::GateReason::FailedHere);
}

/// A coordinator that restarted after it acknowledged a `try_conflict` reads the standing
/// refusal back from its verdicts file (the daemon no longer sends it) and orders the host past
/// its try, not failed here.
///
/// Catches: a refusal kept in the verdicts file that comes back after a restart as the host
/// failing the version, which no operator verb lifts until another version is set.
#[tokio::test]
async fn a_refusal_read_back_after_a_restart_fails_no_one() {
    let mut rig = eight_hosts("try-refusal-restart").await;
    ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]);
    let (_, _, acks) = told(
        &rig,
        "d1",
        vec![web(
            "1.0",
            &json!({}),
            Some(refusal("2.0", 1, "try_conflict")),
        )],
    );
    assert_eq!(acks.len(), 1, "the refusal is acknowledged: {acks:?}");
    restart(&mut rig, "");
    rig.at(2_100);
    assert_eq!(
        ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]),
        (update_reply("2.0"), Some(update_order("2.0", 2)))
    );
    assert_ne!(gate_of(&rig, "d1"), proto::GateReason::FailedHere);
}

/// From a daemon that sends its record's try: after a restart the coordinator
/// has heard no verdict of `d1`, whose step record holds the install's try 1, and orders its
/// first update at try 2, so the daemon has nothing to refuse; a host that sends no record try
/// is ordered try 1.
///
/// Catches: a restarted coordinator that numbers from try 1 a host whose last order it never
/// heard of (the install a self-update ran across the daemon's restart), which the daemon
/// refuses `try_conflict`.
#[tokio::test]
async fn a_restarted_coordinator_orders_past_the_try_the_hosts_record_holds() {
    let mut rig = eight_hosts("try-refusal-record").await;
    restart(&mut rig, "");
    rig.at(2_100);
    assert_eq!(
        ordered(
            &rig,
            "d1",
            vec![web("1.0", &json!({"record_try": 1}), None)]
        ),
        (update_reply("2.0"), Some(update_order("2.0", 2)))
    );
}

/// A refusal for the try that names no try (from an agent that does not report it) is taken as
/// the refusal of the outstanding order: it ends that order, and the host is ordered again at
/// the next try at once, not failed here.
///
/// Catches: a try-less refusal that leaves the order outstanding, so the host is told the same
/// refused try again until the order ages out.
#[tokio::test]
async fn a_refusal_that_names_no_try_ends_the_outstanding_order() {
    let rig = eight_hosts("try-refusal-no-try").await;
    ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]);
    let mut refused = refusal("2.0", 1, "try_conflict");
    refused.order_try = None;
    assert_eq!(
        ordered(&rig, "d1", vec![web("1.0", &json!({}), Some(refused))]),
        (update_reply("2.0"), Some(update_order("2.0", 2)))
    );
    assert_ne!(gate_of(&rig, "d1"), proto::GateReason::FailedHere);
}

/// Only a refusal for the try is passed over: a failure in a phase whose reason reads
/// `try_conflict` (the hook said it), a phase-less failure with a class, and a phase-less refusal
/// for another reason (`package_digest_mismatch`) each still fail `d1` here.
///
/// Catches: a refusal rule wide enough to hide a release failure, or a refusal of the package.
#[tokio::test]
async fn other_failures_still_fail_the_host_here() {
    let mut in_phase = refusal("2.0", 1, "try_conflict");
    in_phase.phase = Some(Phase::AfterCommit);
    in_phase.class = Some(Class::Release);
    let mut with_class = refusal("2.0", 1, "try_conflict");
    with_class.class = Some(Class::Environment);
    let cases = [
        ("try-refusal-phase", in_phase),
        ("try-refusal-class", with_class),
        (
            "try-refusal-package",
            refusal("2.0", 1, "package_digest_mismatch"),
        ),
    ];
    for (test, failure) in cases {
        let rig = eight_hosts(test).await;
        ordered(&rig, "d1", vec![web("1.0", &json!({}), None)]);
        assert_eq!(
            ordered(&rig, "d1", vec![web("1.0", &json!({}), Some(failure))]).1,
            None,
            "{test}"
        );
        assert_eq!(gate_of(&rig, "d1"), proto::GateReason::FailedHere, "{test}");
    }
}

/// The fleet's try of a host and service is the highest of the verdicts' and the records': a
/// record try above the verdict's moves it, one below or absent never lowers it.
///
/// Catches: a record try that replaces the highest heard instead of joining it, and one that is
/// not read at all.
#[test]
fn the_record_try_joins_the_highest_try_heard() {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let host = id("d1");
    let beat = |extra: Value, last_try: Option<u64>| -> Heartbeat {
        let last = last_try.map(|order_try| {
            let mut done = UpdateResult::new(version("1.0"), Outcome::Succeeded);
            done.order_try = Some(order_try);
            done
        });
        web("1.0", &extra, last).as_heartbeat(hostkeeper_wire::Platform::LinuxX86_64)
    };
    let mut hear = |extra: Value, last_try: Option<u64>| {
        fleet.record_in(
            WEB,
            host.clone(),
            crate::testutil::role("daemon"),
            beat(extra, last_try),
            Duration::from_secs(1),
        );
        fleet.next_try(WEB, &host)
    };
    assert_eq!(hear(json!({"record_try": 3}), Some(6)), 7);
    assert_eq!(hear(json!({"record_try": 9}), Some(6)), 10);
    assert_eq!(
        hear(json!({}), None),
        10,
        "an absent record try lowers nothing"
    );
    assert_eq!(hear(json!({"record_try": 2}), None), 10);

    let other = id("d2");
    fleet.record_in(
        WEB,
        other.clone(),
        crate::testutil::role("daemon"),
        beat(json!({"record_try": 0}), None),
        Duration::from_secs(1),
    );
    assert_eq!(
        fleet.next_try(WEB, &other),
        1,
        "a record at try 0 starts at 1"
    );
}
