//! A host group's `failed_hosts` ([`crate::failed_hosts`]), the automatic retry of a failure the
//! host can clear by itself ([`crate::fleet::Fleet::retry_by_itself`]) and the alert of a deferral
//! that repeats its reason ([`crate::defer`]), each driven by the heartbeats of enrolled hosts
//! placed in groups.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{
    Class, DeferredReport, Health, Outcome, Phase, ServiceBeat, ServiceReply, Token, UpdateResult,
};

use crate::decide::{Gate, Skip, Why};
use crate::defer::DeferBackoff;
use crate::failed_hosts::{CLEARS_BY_ITSELF, clears_by_itself};
use crate::group_decision_support::{Farm, policy, stages};
use crate::marks_support::{operator, runs, update};
use crate::orders::Order;
use crate::stuck_rollouts_tests::{failure, order_with, plan_of, update_failed};
use crate::testutil::version;

const NOTHING: ServiceReply = ServiceReply::Nothing;
const SIX: [&str; 6] = ["b1", "b2", "b3", "b4", "b5", "b6"];

fn at(farm: &Farm, secs: u64) {
    farm.world.rig.at(secs);
}

/// Takes the fleet's lock once, so the cluster stamps the desired versions set so far.
fn stamp(farm: &Farm) {
    drop(farm.world.coordinator().lock_fleet());
}

/// The alerts of `kind` `Alerts.List` answers.
fn listed(farm: &Farm, kind: proto::AlertKind) -> Vec<proto::Alert> {
    farm.world
        .coordinator()
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("listed")
        .alerts
        .into_iter()
        .filter(|a| a.kind == i32::from(kind))
        .collect()
}

/// A failure the host cannot clear by itself: the manifest of the release does not parse.
fn invalid() -> UpdateResult {
    failure(
        "2.0",
        Some(Phase::BeforeCommit),
        Some(Class::Release),
        "manifest_invalid",
        "the manifest does not parse",
    )
}

/// The beat of a host that runs `hostkeeper` 1.0 and `web` 1.0, and failed `web` 2.0 by `report`.
fn web_failed(report: UpdateResult) -> Vec<ServiceBeat> {
    vec![
        runs("hostkeeper", "1.0"),
        update_failed("web", "1.0", report),
    ]
}

/// A group `build` of `hosts` listing `web`, with `edit` applied to its policy, and a group `two`
/// with `db` on `t1`; `web` set to 2.0, then `db`, each host heard at 1.0 first.
fn two_groups(name: &str, hosts: &[&str], edit: fn(&mut proto::RolloutPolicy)) -> Farm {
    let farm = Farm::new(name);
    farm.flag(1);
    farm.group("build", &["web"], Some(policy(edit)));
    farm.group("two", &["db"], None);
    for host in hosts {
        farm.host(host, "build");
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    farm.host("t1", "two");
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("build", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_006);
    farm.want("two", "db", "2.0");
    stamp(&farm);
    farm
}

/// Orders `host` `web` 2.0 and has it report it runs it.
fn rolls(farm: &Farm, host: &str) {
    assert_eq!(
        farm.tell(host, &[("web", "1.0")]),
        update("web", "2.0"),
        "{host}"
    );
    assert_eq!(farm.tell(host, &[("web", "2.0")]), NOTHING, "{host}");
}

/// Proves the example of `docs/operations/rollout-plans.md` ("Example: one host fails, under
/// `continue`"): in a group of six with `stages = [50, 100]` and `max_updating = 1`, under the
/// default `failed_hosts`, the first host fails `manifest_invalid` and stays at 1.0
/// (`failed_here`, one `failed_update` alert); the stage counts it as done, failed, so the other
/// two hosts of the first stage are ordered one at a time, then the three of the second stage. While
/// `web` has a host to order it is the one live rollout, and `db` of another group, set after it,
/// waits `rollout_elsewhere`; once `web` has none left, `db` is ordered and `web`'s plan says
/// `nothing_to_order`, waiting on no service.
///
/// Catches: a failed host that holds its stage (the others `stage_failed`) when its group named no
/// `failed_hosts`, which left the rest of the group on the old version until an operator acted.
#[test]
fn a_failed_host_counts_as_done_failed_and_the_rest_of_the_group_is_ordered() {
    let farm = two_groups("fh-continue", &SIX, |p| {
        p.stages = stages(&[50, 100]);
        p.max_updating = Some(1);
    });
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell_with("b1", web_failed(invalid())), NOTHING);
    assert_eq!(farm.why("b1", "web"), Some(Why::Skip(Skip::FailedHere)));
    rolls(&farm, "b2");
    assert_eq!(
        farm.why("t1", "db"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    rolls(&farm, "b3");
    for host in ["b4", "b5", "b6"] {
        rolls(&farm, host);
    }
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    let web = plan_of(&farm, "b2", "web");
    assert_eq!(
        (web.waiting, web.waiting_on),
        (i32::from(proto::PlanWait::NothingToOrder), vec![])
    );
    let alerts = listed(&farm, proto::AlertKind::FailedUpdate);
    assert_eq!(
        alerts
            .iter()
            .map(|a| (a.host_id.as_str(), a.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(
            "b1",
            "b1 failed web 2.0 in host group build: manifest_invalid (\"the manifest does not \
             parse\"); it is not ordered 2.0 again until an operator retries it (Hosts.Retry), \
             holds or excludes it, or another version is set"
        )]
    );
}

/// Proves: the same group with `failed_hosts = FAILED_HOSTS_STOP` stops where it did before the
/// key: once the first stage's other two hosts are done, the failed host holds it, the next host
/// waits `stage_failed`, the plan waits `failed_host` and takes no place, and `db` is ordered.
///
/// Catches: `stop` read as `continue`, or a stopped plan that holds another group's rollout.
#[test]
fn under_stop_a_failed_host_holds_its_stage() {
    let farm = two_groups("fh-stop", &SIX, |p| {
        p.stages = stages(&[50, 100]);
        p.max_updating = Some(1);
        p.failed_hosts = proto::FailedHosts::Stop.into();
    });
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell_with("b1", web_failed(invalid())), NOTHING);
    rolls(&farm, "b2");
    rolls(&farm, "b3");
    assert_eq!(farm.tell("b4", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("b4", "web"), Some(Why::Gate(Gate::StageFailed)));
    assert_eq!(
        plan_of(&farm, "b4", "web").waiting,
        i32::from(proto::PlanWait::FailedHost)
    );
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Has `hosts` of the group `build`, `max_updating = 1`, fail `web` 2.0 one after the other up to
/// the ceiling `ceiling`: each failure below it lets the next host be ordered, and the one that
/// reaches it leaves every host not yet ordered `stage_failed`, the plan `failed_host`, and `db`
/// free to roll.
fn stops_at(name: &str, hosts: &[&str], ceiling: usize, edit: fn(&mut proto::RolloutPolicy)) {
    let farm = two_groups(name, hosts, edit);
    for (i, host) in hosts.iter().take(ceiling).enumerate() {
        assert_eq!(
            farm.tell(host, &[("web", "1.0")]),
            update("web", "2.0"),
            "{name}: {host} is ordered after {i} failures"
        );
        assert_eq!(
            farm.why("t1", "db"),
            Some(Why::Gate(Gate::RolloutElsewhere))
        );
        assert_eq!(farm.tell_with(host, web_failed(invalid())), NOTHING);
    }
    for host in &hosts[ceiling..] {
        assert_eq!(
            farm.tell(host, &[("web", "1.0")]),
            NOTHING,
            "{name}: {host}"
        );
        assert_eq!(
            farm.why(host, "web"),
            Some(Why::Gate(Gate::StageFailed)),
            "{name}: {host}"
        );
    }
    assert_eq!(
        plan_of(&farm, hosts[0], "web").waiting,
        i32::from(proto::PlanWait::FailedHost),
        "{name}"
    );
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves the worked examples of the ceiling (`docs/operations/rollout-plans.md`, A failed host):
/// under `continue`, `rollback_after_failed` at its default (25 percent, at least 2 hosts) stops a
/// plan of six hosts at its second failed host and one of four hosts at its second too (25 percent
/// of four is one, under the two); a threshold of 50 percent and 1 host stops six at the third.
/// Once it is reached, no host is ordered that was not, and the plan holds no other rollout.
///
/// Catches: a release that fails on every host of a group, one after the other, because the plan
/// went on past any number of failures; or a ceiling counted against another number of hosts.
#[test]
fn failed_hosts_that_reach_the_ceiling_stop_the_plan() {
    let one_at_a_time = |p: &mut proto::RolloutPolicy| {
        p.stages = stages(&[100]);
        p.max_updating = Some(1);
    };
    stops_at("fh-ceiling-six", &SIX, 2, one_at_a_time);
    stops_at(
        "fh-ceiling-four",
        &["m1", "m2", "m3", "m4"],
        2,
        one_at_a_time,
    );
    stops_at("fh-ceiling-half", &SIX, 3, |p| {
        p.stages = stages(&[100]);
        p.max_updating = Some(1);
        p.rollback_after_failed = Some(proto::FailThreshold {
            percent: Some(50),
            min_hosts: Some(1),
        });
    });
}

/// Proves: failures the host can clear by itself count for no ceiling: in a group of six under
/// `continue`, two hosts refused for a moment (`busy`) and the next is ordered all the same, though
/// two failures of another reason would stop the plan.
///
/// Catches: a plan stopped by hosts that were busy for a moment, which the coordinator orders
/// again by itself and which say nothing of the release.
#[test]
fn failures_the_host_can_clear_count_for_no_ceiling() {
    let farm = two_groups("fh-ceiling-busy", &SIX, |p| {
        p.stages = stages(&[100]);
        p.max_updating = Some(1);
    });
    let busy = failure("2.0", None, None, "busy", "another update runs");
    for host in ["b1", "b2"] {
        assert_eq!(farm.tell(host, &[("web", "1.0")]), update("web", "2.0"));
        assert_eq!(farm.tell_with(host, web_failed(busy.clone())), NOTHING);
    }
    assert_eq!(farm.tell("b3", &[("web", "1.0")]), update("web", "2.0"));
}

/// What a host reports that switched `web` to 2.0, failed after the switch and was left there,
/// not healthy: the failure holds an update slot while the plan stops at it.
fn failed_in_place() -> Vec<ServiceBeat> {
    let mut beat = runs("web", "2.0");
    beat.health = Health::Unhealthy;
    beat.last_update = Some(failure(
        "2.0",
        Some(Phase::AfterCommit),
        Some(Class::Release),
        "not_serving",
        "the service did not come up",
    ));
    vec![runs("hostkeeper", "1.0"), beat]
}

/// Proves: with `max_updating = 1`, a host that failed after its switch and is not healthy holds
/// the one slot under `stop` (the next host waits `no_slot`), and none under `continue` (the next
/// host is ordered): the failed host runs nothing.
///
/// Catches: a continuing plan held for good at `max_updating` by a failed host that is not
/// healthy, though nothing of its update runs.
#[test]
fn a_failed_host_that_is_not_healthy_holds_no_slot_of_a_plan_that_goes_on() {
    for (name, stop, next) in [
        ("fh-slot-continue", false, update("web", "2.0")),
        ("fh-slot-stop", true, NOTHING),
    ] {
        let farm = two_groups(name, &SIX, if stop { slot_stop } else { slot_continue });
        assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
        assert_eq!(farm.tell_with("b1", failed_in_place()), NOTHING);
        assert_eq!(farm.tell("b2", &[("web", "1.0")]), next, "{name}");
        if stop {
            assert_eq!(farm.why("b2", "web"), Some(Why::Gate(Gate::NoSlot)));
        }
    }
}

fn slot_continue(p: &mut proto::RolloutPolicy) {
    p.stages = stages(&[100]);
    p.max_updating = Some(1);
}

fn slot_stop(p: &mut proto::RolloutPolicy) {
    slot_continue(p);
    p.failed_hosts = proto::FailedHosts::Stop.into();
}

/// The try of the update `order` is, if it is one of `web`.
fn try_of(order: Option<Order>) -> Option<u64> {
    match order? {
        Order::Update {
            service, order_try, ..
        } if service == "web" => Some(order_try),
        _ => None,
    }
}

/// A group `r` of `hosts` listing `web` with `edit` applied to its policy, `web` set to 2.0 with
/// a release whose entries a daemon that takes installs is ordered, each host heard at 1.0 first.
async fn retried(name: &str, hosts: &[&str], edit: fn(&mut proto::RolloutPolicy)) -> Farm {
    let farm = Farm::new(name);
    farm.group("r", &["web"], Some(policy(edit)));
    for host in hosts {
        farm.host(host, "r");
        assert_eq!(order_with(&farm, host, vec![runs("web", "1.0")]), None);
    }
    at(&farm, 2_005);
    farm.deploy("r", "web", "2.0", 1).await;
    farm
}

/// Proves: a failure the host can clear by itself (`busy`) is ordered again by itself once the
/// group's `defer_backoff` has passed since the host reported it, and not a second before: 60 s
/// after the first failure, 120 s after the second (`factor` 2), each at the try past the failed
/// one. Its `failed_update` alert says so, stays listed while the retry runs, counts the failures
/// in a row, and clears once the host runs the version.
///
/// Catches: a host held at `failed_here` for good by a moment of its own (another update running,
/// its package manager busy), which only an operator's `Hosts.Retry` lifted; or a retry that does
/// not back off.
#[tokio::test]
async fn a_failure_the_host_can_clear_is_ordered_again_after_the_backoff() {
    let farm = retried("fh-retry", &["r1"], |p| {
        p.defer_backoff = Some(proto::DeferBackoff {
            initial_secs: Some(60),
            factor: Some(2),
            max_secs: Some(600),
        });
    })
    .await;
    at(&farm, 2_010);
    assert_eq!(
        try_of(order_with(&farm, "r1", vec![runs("web", "1.0")])),
        Some(1)
    );
    let busy = |order_try| UpdateResult {
        order_try: Some(order_try),
        ..failure("2.0", None, None, "busy", "another update runs")
    };
    let said = |failures: u32| {
        format!(
            "r1 failed web 2.0 in host group r: busy (\"another update runs\"); the host can \
             clear it by itself ({failures} in a row): it is ordered 2.0 again after the group's \
             defer_backoff, longer after each failure"
        )
    };
    let alert = |farm: &Farm| {
        listed(farm, proto::AlertKind::FailedUpdate)
            .into_iter()
            .map(|a| (a.message, a.tries))
            .collect::<Vec<_>>()
    };
    at(&farm, 2_020);
    assert_eq!(order_with(&farm, "r1", web_failed(busy(1))), None);
    assert_eq!(alert(&farm), vec![(said(1), 1)]);
    at(&farm, 2_079);
    assert_eq!(order_with(&farm, "r1", web_failed(busy(1))), None);
    assert_eq!(farm.why("r1", "web"), Some(Why::Skip(Skip::FailedHere)));
    at(&farm, 2_080);
    assert_eq!(
        try_of(order_with(&farm, "r1", web_failed(busy(1)))),
        Some(2)
    );
    assert_eq!(alert(&farm), vec![(said(1), 1)]);

    at(&farm, 2_100);
    assert_eq!(order_with(&farm, "r1", web_failed(busy(2))), None);
    assert_eq!(alert(&farm), vec![(said(2), 2)]);
    at(&farm, 2_219);
    assert_eq!(order_with(&farm, "r1", web_failed(busy(2))), None);
    at(&farm, 2_220);
    assert_eq!(
        try_of(order_with(&farm, "r1", web_failed(busy(2)))),
        Some(3)
    );

    let mut done = runs("web", "2.0");
    done.last_update = Some(UpdateResult {
        order_try: Some(3),
        ..UpdateResult::new(version("2.0"), Outcome::Succeeded)
    });
    at(&farm, 2_230);
    assert_eq!(order_with(&farm, "r1", vec![done]), None);
    assert_eq!(alert(&farm), vec![]);
}

/// Proves: each reason of a failure the host can clear by itself (`busy`, `verify_unavailable`,
/// `fetch_failed`, `package_manager_busy`, `no_space`) is ordered again after the group's default
/// `defer_backoff` (300 s), and a failure of a reason the host cannot clear (`manifest_invalid`,
/// `digest_mismatch`, `signature`, a missing configuration, the `halted` refusal of a host's own
/// halt, and `busy` after the commit) is never ordered again by itself, a day later included: it
/// stays `failed_here` until an operator acts. (A failure in the halt phase halts the service;
/// [`a_failure_clears_by_itself_by_its_reason_and_phase`] proves it is never one to retry.)
///
/// Catches: a reason the brief names left out of the retry, or a failure that needs a person (a
/// broken release, a bad signature, a host the release cannot set up) ordered again and again.
#[tokio::test]
async fn only_a_failure_the_host_can_clear_is_retried() {
    let clears: Vec<(&str, UpdateResult)> = CLEARS_BY_ITSELF
        .iter()
        .map(|reason| (*reason, failure("2.0", None, None, reason, "")))
        .chain([(
            "package_manager_busy_before_commit",
            failure(
                "2.0",
                Some(Phase::BeforeCommit),
                Some(Class::Environment),
                "package_manager_busy",
                "",
            ),
        )])
        .collect();
    let stands: Vec<(&str, UpdateResult)> = vec![
        ("manifest_invalid", invalid()),
        (
            "digest_mismatch",
            failure("2.0", None, None, "digest_mismatch", ""),
        ),
        (
            "signature",
            failure(
                "2.0",
                Some(Phase::BeforeCommit),
                Some(Class::Environment),
                "signature",
                "",
            ),
        ),
        (
            "config_missing",
            failure(
                "2.0",
                Some(Phase::BeforeCommit),
                Some(Class::Environment),
                "config_missing",
                "",
            ),
        ),
        ("halted", failure("2.0", None, None, "halted", "")),
        (
            "busy_after_commit",
            failure("2.0", Some(Phase::AfterCommit), None, "busy", ""),
        ),
    ];
    let hosts: Vec<String> = (1..=clears.len() + stands.len())
        .map(|n| format!("h{n}"))
        .collect();
    let names: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let farm = retried("fh-reasons", &names, |p| {
        p.stages = stages(&[100]);
        p.rollback_after_failed = Some(proto::FailThreshold {
            percent: Some(100),
            min_hosts: None,
        });
    })
    .await;
    let cases: Vec<(&str, &str, UpdateResult, bool)> = clears
        .into_iter()
        .map(|(reason, report)| (reason, report, true))
        .chain(
            stands
                .into_iter()
                .map(|(reason, report)| (reason, report, false)),
        )
        .zip(names.iter())
        .map(|((reason, report, again), host)| (*host, reason, report, again))
        .collect();
    at(&farm, 2_010);
    for (host, reason, report, _) in &cases {
        let order = order_with(&farm, host, vec![runs("web", "1.0")]);
        assert_eq!(try_of(order), Some(1), "{reason}");
        assert_eq!(
            order_with(&farm, host, web_failed(report.clone())),
            None,
            "{reason}"
        );
    }
    at(&farm, 2_309);
    for (host, reason, report, _) in &cases {
        assert_eq!(
            order_with(&farm, host, web_failed(report.clone())),
            None,
            "{reason} before its backoff"
        );
    }
    at(&farm, 2_310);
    for (host, reason, report, again) in &cases {
        let ordered = try_of(order_with(&farm, host, web_failed(report.clone())));
        assert_eq!(ordered, again.then_some(2), "{reason} at its backoff");
    }
    at(&farm, 2_010 + 86_400);
    for (host, reason, report, _) in cases.iter().filter(|(_, _, _, again)| !again) {
        assert_eq!(
            order_with(&farm, host, web_failed(report.clone())),
            None,
            "{reason} a day later"
        );
        assert_eq!(
            farm.why(host, "web"),
            Some(Why::Skip(Skip::FailedHere)),
            "{reason}"
        );
    }
}

/// Proves: which reports are failures the host can clear by itself: a failure of one of the
/// reasons, with no phase or before the commit; not a success, a failure with no reason, one after
/// the commit or in the halt, or one of another reason.
///
/// Catches: a retried report that changed the host (after its commit) or that says nothing of why.
#[test]
fn a_failure_clears_by_itself_by_its_reason_and_phase() {
    let busy = |phase| failure("2.0", phase, None, "busy", "");
    assert!(clears_by_itself(&busy(None)));
    assert!(clears_by_itself(&busy(Some(Phase::BeforeCommit))));
    assert!(!clears_by_itself(&busy(Some(Phase::AfterCommit))));
    assert!(!clears_by_itself(&busy(Some(Phase::Halt))));
    assert!(!clears_by_itself(&UpdateResult {
        reason: None,
        ..busy(None)
    }));
    assert!(!clears_by_itself(&UpdateResult {
        outcome: Outcome::Succeeded,
        ..busy(None)
    }));
    assert!(!clears_by_itself(&failure(
        "2.0",
        None,
        None,
        "busy_elsewhere",
        ""
    )));
}

/// What a host that declined the install of `js` 2.0 for `reason` reports.
fn declined(reason: &str) -> ServiceBeat {
    let mut beat: ServiceBeat = serde_json::from_value(serde_json::json!({
        "name": "js", "version": "2.0", "health": "absent"
    }))
    .expect("a service beat");
    beat.deferred = Some(DeferredReport {
        target: version("2.0"),
        reason: Token::parse(reason).expect("a token"),
        retry_after_secs: 60,
    });
    beat
}

/// Has `host` be offered the install of `js` at `secs` and decline it for `reason` a second later.
fn declines(farm: &Farm, host: &str, secs: u64, reason: &str) {
    at(farm, secs);
    assert!(
        matches!(order_with(farm, host, vec![]), Some(Order::Install { .. })),
        "{host} is offered js at {secs}"
    );
    at(farm, secs + 1);
    assert_eq!(order_with(farm, host, vec![declined(reason)]), None);
}

/// Proves the case seen on four Macs: a first install of `js` declined each time it is offered for
/// a reason that does not clear (`host_setup_missing`) waits longer each time (300 s, then 600 s),
/// holds no rollout of another group (`db`, set after it, is ordered while every Mac waits), and
/// is a `repeated_deferral` alert from its third deferral in a row (critical, with the reason and
/// how many times), until the Mac runs the version; a Mac whose third deferral gives another
/// reason is none.
///
/// Catches: a host that declines an update forever for a reason that cannot clear by itself,
/// counted as rolling and seen by nobody.
#[tokio::test]
async fn a_deferral_that_repeats_its_reason_is_an_alert_and_holds_nobody_else() {
    let farm = Farm::new("fh-deferral");
    farm.flag(1);
    farm.group("mac", &["js"], Some(policy(|p| p.stages = stages(&[100]))));
    farm.group("two", &["db"], None);
    for host in ["m1", "m2"] {
        farm.host(host, "mac");
        assert_eq!(order_with(&farm, host, vec![]), None);
    }
    farm.host("t1", "two");
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.deploy("mac", "js", "2.0", 1).await;
    stamp(&farm);
    at(&farm, 2_006);
    farm.want("two", "db", "2.0");
    stamp(&farm);

    declines(&farm, "m1", 2_010, "host_setup_missing");
    declines(&farm, "m2", 2_010, "host_setup_missing");
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    at(&farm, 2_310);
    assert_eq!(order_with(&farm, "m1", vec![]), None);
    declines(&farm, "m1", 2_311, "host_setup_missing");
    declines(&farm, "m2", 2_311, "host_setup_missing");
    assert_eq!(listed(&farm, proto::AlertKind::RepeatedDeferral), vec![]);
    at(&farm, 2_911);
    assert_eq!(order_with(&farm, "m1", vec![]), None);
    declines(&farm, "m1", 2_912, "host_setup_missing");
    declines(&farm, "m2", 2_912, "busy_host");

    let alerts = listed(&farm, proto::AlertKind::RepeatedDeferral);
    let alerted = alerts.clone();
    assert_eq!(
        alerts
            .iter()
            .map(|a| (
                a.host_id.as_str(),
                a.service.as_str(),
                a.version.as_str(),
                a.reason.as_str(),
                a.tries,
                a.severity,
                a.message.as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![(
            "m1",
            "js",
            "2.0",
            "host_setup_missing",
            3,
            i32::from(proto::AlertSeverity::Critical),
            "m1 declined js 2.0 in host group mac 3 times in a row: host_setup_missing; it is \
             offered the version again after each wait, longer each time, and holds no other \
             host or service, but its reason may not clear by itself",
        )]
    );
    // m1 then runs js 2.0, though no heartbeat said it was updating: the alert clears.
    assert_eq!(alerted.len(), 1);
    at(&farm, 3_000);
    assert_eq!(order_with(&farm, "m1", vec![runs("js", "2.0")]), None);
    assert_eq!(listed(&farm, proto::AlertKind::RepeatedDeferral), vec![]);
}

/// Proves: the count of a deferral's reason follows the reason, episode by episode: two of one
/// reason then one of another is one in a row of the other; a report heard again within an episode
/// counts once; a reason that changes within an episode starts the count again; a new target starts
/// it again; and an update under way forgets it.
///
/// Catches: a streak that counts heartbeats rather than deferrals, or keeps a reason the host no
/// longer gives.
#[test]
fn a_deferrals_streak_counts_episodes_of_one_reason() {
    use crate::defer::Defers;
    use crate::testutil::{beat, id, secs};
    let key = (id("h1"), "web".to_owned());
    let of = |target: &str, reason: &str| {
        let mut b = beat("1.0");
        b.deferred = Some(DeferredReport {
            target: version(target),
            reason: Token::parse(reason).expect("a token"),
            retry_after_secs: 0,
        });
        b
    };
    let streak = |defers: &Defers, target: &str| {
        defers
            .streak(&key, &version(target))
            .map(|(reason, n)| (reason.as_str().to_owned(), n))
    };
    let mut defers = Defers::default();
    defers.observe(&key, &of("2.0", "a"), true, secs(1));
    defers.observe(&key, &of("2.0", "a"), false, secs(2));
    assert_eq!(streak(&defers, "2.0"), Some(("a".to_owned(), 1)));
    defers.observe(&key, &of("2.0", "a"), true, secs(3));
    assert_eq!(streak(&defers, "2.0"), Some(("a".to_owned(), 2)));
    defers.observe(&key, &of("2.0", "b"), true, secs(4));
    assert_eq!(streak(&defers, "2.0"), Some(("b".to_owned(), 1)));
    defers.observe(&key, &of("2.0", "b"), true, secs(5));
    defers.observe(&key, &of("2.0", "c"), false, secs(6));
    assert_eq!(streak(&defers, "2.0"), Some(("c".to_owned(), 1)));
    assert_eq!(streak(&defers, "3.0"), None);
    defers.observe(&key, &of("3.0", "c"), true, secs(7));
    assert_eq!(streak(&defers, "3.0"), Some(("c".to_owned(), 1)));
    let mut updating = beat("1.0");
    updating.updating = Some(version("3.0"));
    defers.observe(&key, &updating, false, secs(8));
    assert_eq!(streak(&defers, "3.0"), None);
}

/// Proves: the wait of the n-th failure in a row is the n-th deferral's: the default backoff of
/// 300 s doubling to its cap of 3600 s.
///
/// Catches: a retry schedule that drifts from the deferral's the group's policy names.
#[test]
fn the_retry_waits_as_a_deferral_does() {
    let backoff = DeferBackoff::default();
    let waits: Vec<u64> = (1..=6).map(|n| backoff.wait(n, 0).as_secs()).collect();
    assert_eq!(waits, vec![300, 600, 1_200, 2_400, 3_600, 3_600]);
}

/// Proves, over the fleet alone: a run of failures the host can clear by itself counts each
/// failure once (a report heard again waits on), restarts at one for a failure of another version,
/// keeps its count across a failure an operator set aside (`Hosts.Retry`), and ends at a report the
/// host cannot clear; an operator's setting aside makes the next failure heard a new one; the run's
/// last failure is what [`crate::fleet::Fleet::failing`] answers only
/// while it is set aside, and a forgotten host keeps none.
///
/// Catches: a backoff that grows with every heartbeat that repeats the report, a count carried to
/// another version, or a run that outlives a failure an operator must see.
#[test]
fn a_run_of_failures_counts_each_failure_once_and_ends_at_another_report() {
    use std::time::Duration;

    use crate::fleet::Fleet;
    use crate::testutil::{id, role, secs};
    let host = id("d1");
    let backoff = DeferBackoff {
        initial_secs: 10,
        factor: 2,
        max_secs: 1_000,
    };
    let fails = |target: &str, order_try: u64, reason: &str| {
        let mut beat = runs("web", "1.0");
        beat.last_update = Some(UpdateResult {
            order_try: Some(order_try),
            ..failure(target, None, None, reason, "")
        });
        beat.as_heartbeat(hostkeeper_wire::Platform::LinuxX86_64)
    };
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let hear = |fleet: &mut Fleet, beat, at: u64| {
        fleet.record_in("web", host.clone(), role("daemon"), beat, secs(at));
        fleet.retry_by_itself("web", &host, &backoff, secs(at))
    };
    assert!(!hear(&mut fleet, fails("2.0", 1, "busy"), 100));
    assert!(!hear(&mut fleet, fails("2.0", 1, "busy"), 109));
    assert_eq!(fleet.failures_in_a_row("web", &host), Some(1));
    assert_eq!(fleet.failing("web", &host), None);
    assert!(hear(&mut fleet, fails("2.0", 1, "busy"), 110));
    assert_eq!(
        fleet
            .failing("web", &host)
            .map(|run| (run.failures, run.report.order_try)),
        Some((1, Some(1)))
    );
    // The second failure waits 20 s.
    assert!(!hear(&mut fleet, fails("2.0", 2, "busy"), 200));
    assert_eq!(fleet.failing("web", &host), None);
    assert!(!hear(&mut fleet, fails("2.0", 2, "busy"), 219));
    assert_eq!(fleet.failures_in_a_row("web", &host), Some(2));
    // An operator sets the second aside; a failure heard after the host was seen updating is the
    // run's third, though its report is the second's again (a daemon that names no new try).
    fleet.set_aside("web", &host);
    let mut updating = runs("web", "1.0");
    updating.updating = Some(version("2.0"));
    let updating = updating.as_heartbeat(hostkeeper_wire::Platform::LinuxX86_64);
    assert!(!hear(&mut fleet, updating, 250));
    assert!(!hear(&mut fleet, fails("2.0", 2, "busy"), 300));
    assert_eq!(fleet.failures_in_a_row("web", &host), Some(3));
    assert!(!hear(&mut fleet, fails("2.0", 3, "no_space"), 339));
    assert_eq!(fleet.failures_in_a_row("web", &host), Some(4));
    // Another version starts the run again.
    assert!(!hear(&mut fleet, fails("2.1", 4, "busy"), 400));
    assert_eq!(fleet.failures_in_a_row("web", &host), Some(1));
    assert!(hear(&mut fleet, fails("2.1", 4, "busy"), 410));
    // A failure the host cannot clear ends it.
    assert!(!hear(&mut fleet, fails("2.1", 5, "manifest_invalid"), 500));
    assert_eq!(fleet.failures_in_a_row("web", &host), None);
    assert!(!hear(&mut fleet, fails("2.1", 6, "busy"), 600));
    assert!(fleet.release_if_final(&host, secs(10_000)));
    assert_eq!(fleet.failures_in_a_row("web", &host), None);
}

/// A failure the host can clear by itself.
fn busy() -> UpdateResult {
    failure("2.0", None, None, "busy", "another update runs")
}

/// Proves: in a group of six under `continue` with `max_updating = 1`, one failure the host can
/// clear by itself (`busy`) and one it cannot (`manifest_invalid`) are one failure towards the
/// ceiling of two: the next host is ordered; a second `manifest_invalid` reaches it, and the host
/// after waits `stage_failed`.
///
/// Catches: a ceiling that counts the failures the coordinator retries, or none at all.
#[test]
fn a_ceiling_counts_only_the_failures_the_host_cannot_clear() {
    let farm = two_groups("fh-ceiling-mixed", &SIX, slot_continue);
    for (host, report) in [("b1", busy()), ("b2", invalid()), ("b3", invalid())] {
        assert_eq!(
            farm.tell(host, &[("web", "1.0")]),
            update("web", "2.0"),
            "{host}"
        );
        assert_eq!(farm.tell_with(host, web_failed(report)), NOTHING);
    }
    assert_eq!(farm.tell("b4", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("b4", "web"), Some(Why::Gate(Gate::StageFailed)));
}

/// Proves: under `continue` a canary holds the plan only while it is the current stage: a canary
/// host that refused for a moment (`busy`) leaves the next host `stage_failed` until it is ordered
/// again by itself after the backoff (300 s), and once the canary is done, a host of the next stage
/// that fails `manifest_invalid` leaves the plan going on: the host after it is ordered.
///
/// Catches: a canary that stops nothing under `continue`, or a plan that keeps the canary's
/// stop for every later failure.
#[test]
fn a_canary_holds_the_plan_only_while_it_is_the_current_stage() {
    let farm = two_groups("fh-canary", &SIX, |p| {
        p.canary = Some(proto::Canary { count: 1 });
        p.stages = stages(&[100]);
        p.max_updating = Some(1);
    });
    at(&farm, 2_010);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell_with("b1", web_failed(busy())), NOTHING);
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("b2", "web"), Some(Why::Gate(Gate::StageFailed)));
    at(&farm, 2_309);
    assert_eq!(farm.tell_with("b1", web_failed(busy())), NOTHING);
    at(&farm, 2_310);
    assert_eq!(
        farm.tell_with("b1", web_failed(busy())),
        update("web", "2.0")
    );
    assert_eq!(farm.tell("b1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell_with("b2", web_failed(invalid())), NOTHING);
    assert_eq!(farm.tell("b3", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: under `stop`, a failure the host can clear by itself holds its stage only until the
/// coordinator orders the host again (300 s): then the host rolls, and the next stage is ordered.
///
/// Catches: a retry that runs only under `continue`, leaving a `stop` group held by a moment's
/// refusal until an operator acts.
#[test]
fn under_stop_a_failure_the_host_can_clear_holds_its_stage_until_its_retry() {
    let farm = two_groups("fh-stop-busy", &SIX, |p| {
        p.stages = stages(&[50, 100]);
        p.max_updating = Some(1);
        p.failed_hosts = proto::FailedHosts::Stop.into();
    });
    at(&farm, 2_010);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell_with("b1", web_failed(busy())), NOTHING);
    rolls(&farm, "b2");
    rolls(&farm, "b3");
    assert_eq!(farm.why("b4", "web"), Some(Why::Gate(Gate::StageFailed)));
    at(&farm, 2_310);
    assert_eq!(
        farm.tell_with("b1", web_failed(busy())),
        update("web", "2.0")
    );
    assert_eq!(farm.tell("b1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("b4", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: a service of the rollout file (no host group) keeps its failed host as it did: a host
/// that failed `busy` is not ordered again by itself, a day later included.
///
/// Catches: the automatic retry, a host group's, reaching the services of the rollout file.
#[tokio::test]
async fn a_service_of_the_rollout_file_is_not_retried_by_itself() {
    use crate::marks_support::{NEW_DAEMON, ask, beat_of, rig, set};
    let rig = rig("fh-file", "stages = [100]");
    let beat = |one: ServiceBeat| beat_of(NEW_DAEMON, vec![one]);
    assert_eq!(ask(&rig, "d1", "daemon", beat(runs("web", "1.0"))), NOTHING);
    set(&rig, "web", "2.0", 1).await;
    rig.at(2_100);
    assert_eq!(
        ask(&rig, "d1", "daemon", beat(runs("web", "1.0"))),
        update("web", "2.0")
    );
    let failed = || beat(update_failed("web", "1.0", busy()));
    assert_eq!(ask(&rig, "d1", "daemon", failed()), NOTHING);
    for secs in [2_500, 2_100 + 86_400] {
        rig.at(secs);
        assert_eq!(ask(&rig, "d1", "daemon", failed()), NOTHING, "{secs}");
    }
}

/// Proves: under `continue`, with `max_updating = 1`, a host whose update runs, or that waits for
/// its promotion, still holds the one slot (the next host waits `no_slot`); once it is up at the
/// version, the next host is ordered. Only a failure alone holds no slot.
///
/// Catches: a plan that goes on past its failed hosts and, with them, stops counting the hosts that
/// are updating, ordering more at once than `max_updating`.
#[test]
fn under_continue_a_host_that_updates_or_waits_for_its_promotion_holds_its_slot() {
    let farm = two_groups("fh-slot-running", &SIX, slot_continue);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    let mut running = runs("web", "1.0");
    running.updating = Some(version("2.0"));
    assert_eq!(
        farm.tell_with("b1", vec![runs("hostkeeper", "1.0"), running]),
        NOTHING
    );
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("b2", "web"), Some(Why::Gate(Gate::NoSlot)));
    let mut promoting = runs("web", "2.0");
    promoting.promotion_pending = true;
    assert_eq!(
        farm.tell_with("b1", vec![runs("hostkeeper", "1.0"), promoting]),
        NOTHING
    );
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("b2", "web"), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(farm.tell("b1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: a plan whose first stage waits for an operator's Advance has entered no stage, so no
/// stage of it is done, though one of its hosts failed and the group goes on past failed hosts:
/// the failure counts towards a stage only once the plan is in one.
///
/// Catches: a failed host that counts as a done stage before any stage was entered, which would
/// let the plan enter its second stage without the operator's Advance of the first.
#[test]
fn a_plan_that_entered_no_stage_has_no_stage_done() {
    let farm = two_groups("fh-manual-first", &SIX, |p| {
        p.stages = vec![
            proto::PolicyStage {
                percent: 50,
                advance: proto::StageAdvance::Manual.into(),
            },
            proto::PolicyStage {
                percent: 100,
                advance: proto::StageAdvance::Auto.into(),
            },
        ];
    });
    assert_eq!(farm.tell_with("b1", web_failed(invalid())), NOTHING);
    let coordinator = farm.world.coordinator();
    let basis = coordinator.basis_of(&crate::testutil::id("b1"));
    let all = coordinator.desired_for(&basis.services);
    let fleet = coordinator.lock_fleet();
    let now = (coordinator.clock)();
    let def = basis.services.get("web").expect("a def");
    let view = crate::stages::current(def, &all["web"], &fleet, now);
    assert_eq!(
        (view.current, view.failed(), view.stage_done()),
        (0, 1, false)
    );
}
