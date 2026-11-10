//! What the coordinator counts and orders across a restart (design sections 5.4, 8.1, 8.2 and
//! 8.9): a verdict counted once per host, order id and try; a canary's release failure; the
//! quiet start of `--quiet-secs`; the `min_version` floor on every order; orders forgotten and
//! the slot rebuilt from heartbeats at a restart; bake reset at a restart; and a drain that holds
//! the slot until its host says otherwise. Every time is explicit.

use std::time::Duration;

use hostkeeper_wire::wire::{
    Class, DeferredReport, Heartbeat, OrderId, Phase, ServiceReply, Token,
};

use crate::decide::{Decision, Gate};
use crate::fleet::{Fleet, QUIET_START};
use crate::ops::OpsState;
use crate::reconcile::Change;
use crate::rollback_tests::{
    AUTO, HOSTS, ONE, TWO, def, failure, good, marks, release, restart, rig, round, services_of,
    status, wanted, who,
};
use crate::testutil::{
    LEGACY, Log, Rig, beat, decide_why, desired, id, plan, role, secs, updating, version,
};

/// The service in one stage (`stages = [100]`): every host is in the stage from the start.
const ONE_STAGE: &str = "stages = [100]";

/// 2.0 desired with one daemon updating at a time.
const ONE_DAEMON: &str =
    r#"{"version":"2.0","max_updating":{"daemon":1},"min_up":{"storage":0,"frontend":0}}"#;

/// A release failure of 2.0 that ends try `attempt` of the order `order`.
fn verdict(order: &str, attempt: u64) -> Heartbeat {
    let (phase, class) = release();
    let mut b = failure(phase, class, "2.0");
    let last = b.last_update.as_mut().expect("a verdict");
    last.order_id = OrderId::parse(order);
    last.order_try = Some(attempt);
    b
}

/// Six daemons at 1.0 seen at `at`, but for the beats `beats` gives, in order.
fn fleet_of(beats: &[(&str, Heartbeat, u64)]) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for host in HOSTS {
        fleet.record_in(LEGACY, id(host), role("daemon"), beat("1.0"), secs(2_000));
    }
    for (host, b, at) in beats {
        fleet.record_in(LEGACY, id(host), role("daemon"), b.clone(), secs(*at));
    }
    fleet
}

/// What the plan of `extra`'s definition says at `at` with 2.0 desired and 1.0 last good.
fn plan_at(extra: &str, fleet: &Fleet, at: u64) -> Vec<Change> {
    plan(
        &def(extra),
        &desired("2.0", 0, 0),
        &good(),
        fleet,
        &OpsState::default(),
        secs(at),
    )
}

fn rolled_back() -> Vec<Change> {
    vec![
        Change::MarkBad(version("2.0")),
        Change::RollBackTo(version("1.0")),
    ]
}

// ---- verdicts ---------------------------------------------------------------------------

/// Catches: a count per heartbeat: one host resending the same verdict (the same order id and
/// try) on three beats is one failure, below the threshold of two.
#[test]
fn a_resent_verdict_counts_once() {
    let resent = fleet_of(&[
        ("d1", verdict("o-1", 1), 2_000),
        ("d1", verdict("o-1", 1), 2_015),
        ("d1", verdict("o-1", 1), 2_030),
    ]);
    assert_eq!(plan_at(AUTO, &resent, 2_030), vec![]);
    assert_eq!(resent.verdicts_of(LEGACY, &version("2.0")).count(), 1);
}

/// Catches: a count per host: a new try of the same order is a new verdict, so one host that
/// failed two tries meets the threshold of two; and a verdict of another order is one too.
#[test]
fn a_new_try_counts_again() {
    let retried = fleet_of(&[
        ("d1", verdict("o-1", 1), 2_000),
        ("d1", verdict("o-1", 2), 2_015),
    ]);
    assert_eq!(plan_at(AUTO, &retried, 2_015), rolled_back());
    let reordered = fleet_of(&[
        ("d1", verdict("o-1", 1), 2_000),
        ("d1", verdict("o-2", 1), 2_015),
    ]);
    assert_eq!(plan_at(AUTO, &reordered, 2_015), rolled_back());
}

/// Catches: verdicts kept about a version that is no longer desired, or an acked one counted
/// once its ack is gone: only the desired version's verdicts are kept, and an acked one is
/// forgotten for good.
#[test]
fn only_the_desired_versions_unacked_verdicts_are_kept() {
    let mut fleet = fleet_of(&[("d1", verdict("o-1", 1), 2_000)]);
    let mut other = verdict("o-9", 1);
    let last = other.last_update.as_mut().expect("a verdict");
    last.target = version("3.0");
    fleet.record_in(LEGACY, id("d2"), role("daemon"), other, secs(2_000));
    assert_eq!(fleet.verdicts_of(LEGACY, &version("3.0")).count(), 1);
    fleet.keep_verdicts_of(LEGACY, &version("2.0"), &[]);
    assert_eq!(fleet.verdicts_of(LEGACY, &version("3.0")).count(), 0);
    assert_eq!(fleet.verdicts_of(LEGACY, &version("2.0")).count(), 1);

    let report = verdict("o-1", 1).last_update.expect("a verdict");
    let ack = crate::ops::FailureAck {
        node_id: "d1".to_owned(),
        report,
    };
    fleet.keep_verdicts_of(LEGACY, &version("2.0"), &[ack]);
    assert_eq!(fleet.verdicts_of(LEGACY, &version("2.0")).count(), 0);
    fleet.keep_verdicts_of("unknown", &version("2.0"), &[]);
}

// ---- the canary -------------------------------------------------------------------------

const CANARY: &str = "rollback = \"automatic\"\ncanary = { count = 1 }";

/// Catches: a canary outvoted by `min_hosts`, or a canary failure with no alert: one release
/// failure while the canary gate is closed rolls back, and the alert names the service, the host
/// and the target.
#[tokio::test]
async fn a_canary_release_failure_meets_the_rollback_threshold_and_alerts() {
    let fleet = fleet_of(&[("d1", verdict("o-1", 1), 2_000)]);
    let mut expected = vec![Change::CanaryFailed {
        node_id: "d1".to_owned(),
        version: version("2.0"),
    }];
    expected.extend(rolled_back());
    assert_eq!(plan_at(CANARY, &fleet, 2_000), expected);

    let rig = rig("canary-alert", CANARY, ONE).await;
    round(&rig, 2_000, &[]);
    rig.set(TWO).await;
    let log = Log::default();
    let _installed = log.install();
    round(&rig, 2_015, &[("d1", verdict("o-1", 1))]);
    assert_eq!(wanted(&rig), version("1.0"));
    assert_eq!(marks(&rig).bad, vec![version("2.0")]);
    rig.coordinator.observe_alerts().expect("observed");
    rig.coordinator.observe_alerts().expect("observed");
    let text = log.text();
    let alert = text
        .lines()
        .find(|line| line.contains("canary_failed"))
        .unwrap_or_else(|| panic!("no alert: {text}"));
    assert!(alert.contains("ERROR"), "{alert}");
    assert!(alert.contains("service=kbf"), "{alert}");
    assert!(alert.contains("node_id=d1"), "{alert}");
    assert!(alert.contains("version=2.0"), "{alert}");
    assert_eq!(text.matches("canary_failed").count(), 1, "{text}");
}

/// Catches: every failure taken for a canary's: once the canary has baked at the desired
/// version the gate is open, and one failure of another host is below the threshold; and a
/// service with no canary needs `min_hosts` failures.
#[test]
fn a_non_canary_failure_does_not() {
    let baked = fleet_of(&[("d1", beat("2.0"), 2_000), ("d2", verdict("o-2", 1), 2_000)]);
    assert_eq!(plan_at(CANARY, &baked, 2_000), vec![]);
    let plain = fleet_of(&[("d1", verdict("o-1", 1), 2_000)]);
    assert_eq!(plan_at(AUTO, &plain, 2_000), vec![]);
}

/// Catches: a canary's failure the hook blamed on the environment counted as the release's.
#[test]
fn an_environment_failure_on_the_canary_does_not() {
    let mut b = failure(Some(Phase::AfterCommit), Some(Class::Environment), "2.0");
    let last = b.last_update.as_mut().expect("a verdict");
    last.order_id = OrderId::parse("o-1");
    last.order_try = Some(1);
    let fleet = fleet_of(&[("d1", b, 2_000)]);
    assert_eq!(plan_at(CANARY, &fleet, 2_000), vec![]);
}

/// Catches: the canary rule reaching a service that never rolls back by itself.
#[test]
fn a_manual_service_never_rolls_back() {
    let all_failed: Vec<(&str, Heartbeat, u64)> = HOSTS
        .iter()
        .map(|host| (*host, verdict("o-1", 1), 2_000))
        .collect();
    let fleet = fleet_of(&all_failed);
    assert_eq!(plan_at("canary = { count = 1 }", &fleet, 2_000), vec![]);
}

// ---- the quiet start --------------------------------------------------------------------

/// Catches: `--quiet-secs` not reaching the decision: a coordinator started at 1000 with a
/// quiet of 90 seconds orders nothing until 1090, says `quiet` meanwhile, and orders then.
#[tokio::test]
async fn a_longer_quiet_orders_nothing_until_it_ends() {
    let mut rig = Rig::new("quiet-long");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(services_of(""))
        .with_quiet(secs(90));
    rig.set(TWO).await;
    for at in [1_031, 1_060, 1_089] {
        rig.at(at);
        let reply = rig
            .coordinator
            .heartbeat_native(who("d1"), beat("1.0"))
            .expect("a heartbeat")
            .reply;
        assert_eq!(reply, ServiceReply::Nothing, "at {at}");
    }
    let shown = serde_json::to_value(status(&rig)).expect("json");
    assert_eq!(shown["quiet"], true);
    assert_eq!(shown["per_service"][LEGACY]["hosts"][0]["hold"], "quiet");
    rig.at(1_090);
    let reply = rig
        .coordinator
        .heartbeat_native(who("d1"), beat("1.0"))
        .expect("a heartbeat")
        .reply;
    assert!(
        matches!(&reply, ServiceReply::Update { version: v, .. } if *v == version("2.0")),
        "{reply:?}"
    );
    assert!(!status(&rig).quiet);
}

// ---- the floor --------------------------------------------------------------------------

/// Catches: the floor checked only when CI sets the desired state: a desired version below
/// `min_version` put in the state file by hand orders no host, and the status says why.
#[tokio::test]
async fn an_order_below_min_version_is_never_issued() {
    let floored = "min_version = \"2.0\"";
    let mut rig = rig("floor-hand-edit", floored, TWO).await;
    std::fs::write(
        rig.dir.path().join("desired.json"),
        "{\"version\": \"1.5\", \"min_up\": {\"storage\": 0, \"frontend\": 0}}\n",
    )
    .expect("edited by hand");
    restart(&mut rig, floored);
    let replies = round(&rig, 2_045, &[]);
    assert!(
        replies.iter().all(|r| *r == ServiceReply::Nothing),
        "{replies:?}"
    );
    assert_eq!(wanted(&rig), version("1.5"));
    let shown = serde_json::to_value(status(&rig)).expect("json");
    assert_eq!(
        shown["per_service"][LEGACY]["hosts"][0]["hold"],
        "below_min_version"
    );

    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.record_in(LEGACY, id("d1"), role("daemon"), beat("1.0"), secs(2_000));
    let services = services_of(floored);
    let at_floor = [(LEGACY.to_owned(), desired("2.0", 0, 0))].into();
    assert_eq!(
        decide_why(&services, &at_floor, LEGACY, &fleet, &id("d1"), secs(2_000)),
        Decision::Update {
            version: version("2.0")
        }
    );
    let below = [(LEGACY.to_owned(), desired("1.9", 0, 0))].into();
    assert_eq!(
        decide_why(&services, &below, LEGACY, &fleet, &id("d1"), secs(2_000)),
        Decision::Gate(Gate::BelowMinVersion)
    );
}

// ---- restarts, the slot and drains ------------------------------------------------------

/// The reply to `host`'s heartbeat `b` at `at`.
fn ask(rig: &Rig, host: &str, b: Heartbeat, at: u64) -> ServiceReply {
    rig.at(at);
    rig.coordinator
        .heartbeat_native(who(host), b)
        .expect("a heartbeat")
        .reply
}

fn ordered(reply: &ServiceReply) -> bool {
    matches!(reply, ServiceReply::Update { version: v, .. } if *v == version("2.0"))
}

/// Catches: an order kept across a restart (it would hold the slot for a host that never heard
/// it), or a restarted coordinator that orders before the host updating says so: after the
/// restart nothing is ordered in the quiet start, and a host that beats `updating` in it holds
/// the slot again; a host that was ordered and stays silent holds nothing after the restart.
/// The service rolls in one stage with one daemon updating at a time, so the slot is what holds
/// `d2`.
#[tokio::test]
async fn a_restart_forgets_orders_and_a_beating_host_takes_its_slot_back() {
    let mut rig = rig("restart-orders", ONE_STAGE, ONE_DAEMON).await;
    assert!(ordered(&ask(&rig, "d1", beat("1.0"), 2_000)));
    assert_eq!(ask(&rig, "d2", beat("1.0"), 2_000), ServiceReply::Nothing);

    restart(&mut rig, ONE_STAGE);
    assert_eq!(
        ask(&rig, "d1", updating(beat("1.0"), "2.0"), 2_015),
        ServiceReply::Nothing
    );
    assert_eq!(ask(&rig, "d2", beat("1.0"), 2_015), ServiceReply::Nothing);
    assert_eq!(
        ask(&rig, "d1", updating(beat("1.0"), "2.0"), 2_031),
        ServiceReply::Nothing
    );
    assert_eq!(ask(&rig, "d2", beat("1.0"), 2_031), ServiceReply::Nothing);
    let shown = serde_json::to_value(status(&rig)).expect("json");
    let hosts = &shown["per_service"][LEGACY]["hosts"];
    assert_eq!(hosts[0]["holds_slot"], true, "d1, from its own beat");
    assert_eq!(hosts[1]["hold"], "no_slot");

    let mut silent =
        crate::rollback_tests::rig("restart-orders-silent", ONE_STAGE, ONE_DAEMON).await;
    assert!(ordered(&ask(&silent, "d1", beat("1.0"), 2_000)));
    assert_eq!(
        ask(&silent, "d2", beat("1.0"), 2_000),
        ServiceReply::Nothing,
        "d1's order holds the slot"
    );
    restart(&mut silent, ONE_STAGE);
    assert!(
        ordered(&ask(&silent, "d2", beat("1.0"), 2_031)),
        "d1's forgotten order holds nothing"
    );
}

/// Catches: bake counts kept across a restart: hosts that beat healthy at the desired version
/// once before the restart and once after have one beat each, so the last good version waits
/// for the second beat after the restart.
#[tokio::test]
async fn a_restart_resets_bake_and_last_good_waits() {
    let baking = "rollback = \"automatic\"\nbake_beats = 2";
    let mut rig = rig("restart-bake", baking, TWO).await;
    let at_two: Vec<(&str, Heartbeat)> = HOSTS.iter().map(|h| (*h, beat("2.0"))).collect();
    round(&rig, 2_000, &at_two);
    assert_eq!(marks(&rig).last_good, None, "one beat each");

    restart(&mut rig, baking);
    round(&rig, 2_030, &at_two);
    assert_eq!(
        marks(&rig).last_good,
        None,
        "one beat each since the restart"
    );
    round(&rig, 2_045, &at_two);
    assert_eq!(marks(&rig).last_good, Some(version("2.0")));
}

/// A beat of `host`'s that says it declined 2.0 at the drain's bound.
fn drain_bound() -> Heartbeat {
    let mut b = beat("1.0");
    b.deferred = Some(DeferredReport {
        target: version("2.0"),
        reason: Token::parse("drain_bound").expect("a token"),
        retry_after_secs: 300,
    });
    b
}

/// Catches: a slot freed by the time a drain takes, or held after its host gave up at its bound:
/// a host updating with no verdict holds the slot through a pause of any length while it beats
/// `updating`; a `Deferred` at the drain's bound frees it, the next host is ordered, and the
/// host that deferred is left alone for its wait. The service rolls in one stage with one daemon
/// updating at a time, so the slot is what holds `d2`.
#[tokio::test]
async fn an_unbounded_prepare_holds_the_slot_and_a_bounded_one_frees_it() {
    let rig = rig("drain-slot", ONE_STAGE, ONE_DAEMON).await;
    assert!(ordered(&ask(&rig, "d1", beat("1.0"), 2_000)));
    for at in [2_015, 2_600, 9_000, 50_000] {
        assert_eq!(
            ask(&rig, "d1", updating(beat("1.0"), "2.0"), at),
            ServiceReply::Nothing,
            "at {at}"
        );
        assert_eq!(
            ask(&rig, "d2", beat("1.0"), at),
            ServiceReply::Nothing,
            "d2 at {at}"
        );
    }
    assert_eq!(
        ask(&rig, "d1", drain_bound(), 50_015),
        ServiceReply::Nothing
    );
    assert!(ordered(&ask(&rig, "d2", beat("1.0"), 50_015)));
    assert_eq!(
        ask(&rig, "d1", drain_bound(), 50_030),
        ServiceReply::Nothing,
        "d1 waits"
    );
}

/// Catches: a canary gate taken for open while the host at the desired version has not baked
/// or is not up: with `bake_beats = 2`, a host at 2.0 after one healthy beat, or one that is
/// unhealthy, has not baked, so another host's release failure is a canary's and rolls back.
#[test]
fn a_canary_gate_closed_by_an_unbaked_or_unhealthy_host_counts_a_failure_as_a_canarys() {
    let baking = "rollback = \"automatic\"\ncanary = { count = 1 }\nbake_beats = 2";
    let mut expected = vec![Change::CanaryFailed {
        node_id: "d2".to_owned(),
        version: version("2.0"),
    }];
    expected.extend(rolled_back());
    for at_two in [beat("2.0"), crate::testutil::unhealthy(beat("2.0"))] {
        let fleet = fleet_of(&[("d1", at_two, 2_000), ("d2", verdict("o-2", 1), 2_000)]);
        assert_eq!(plan_at(baking, &fleet, 2_000), expected);
    }
}
