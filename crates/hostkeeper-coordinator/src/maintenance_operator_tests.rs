//! `Maintenance.RetryHost` and `Maintenance.PutBack` ([`crate::maintenance_operator`]) with the
//! heartbeats that carry their order to the halted host ([`crate::decide_maintenance`]): what a
//! call stores and audits, the `maintain` the host is sent until its daemon reports the turn at
//! another step, and every refusal. The builders are in `decide_maintenance_tests.rs` and
//! `maintenance_calls_tests.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceKind, MaintenanceStep};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHostStep, MaintenanceState};
use hostkeeper_wire::wire::Heartbeat;

use crate::alerts::Kind as AlertKind;
use crate::decide_maintenance::tests::{
    ORDER, RUN, beat, maintain_id, scheduled, starts, table, tell,
};
use crate::host_groups_support::{REASON, UNIX, World, operator_entry};
use crate::maintenance::text_of;
use crate::maintenance_calls_tests::{
    GROUP, LINUX, cancel, edit_runs, get, host_at, kept, seed_runs,
};
use crate::maintenance_runs::{Act, Kind, RunHost, Step};
use crate::maintenance_service::host_proto;
use crate::marks_support::operator;
use crate::operator::{Caller, entry_proto};
use crate::orders::{Maintain, Order};

/// The order id of [`RUN`]'s second offer to a host, after it deferred once.
const ORDER_2: &str = "maintain-run-1900000000-1-2";

/// The reason the daemon gives for a halt: the step and the exit code.
const HALT: &str = "apply linux: exit 1";

/// A heartbeat of a host that reports a Linux update due and the run record of [`RUN`] taken by
/// `order` at `step`, with `reason`.
fn reported(order: &str, step: MaintenanceStep, reason: &str) -> Heartbeat {
    let mut said = beat(LINUX, None);
    let record = agent::MaintenanceReport {
        due: vec![agent::DueItem {
            kind: MaintenanceKind::Linux as i32,
            label: "pkg".to_owned(),
            ..agent::DueItem::default()
        }],
        run: Some(agent::MaintenanceRunState {
            run_id: RUN.to_owned(),
            order_id: order.to_owned(),
            step: step as i32,
            applied: Vec::new(),
            halted_reason: reason.to_owned(),
        }),
        ..agent::MaintenanceReport::default()
    };
    said.maintenance = Some(text_of(&record));
    said
}

/// [`reported`] under [`ORDER`], the order of the first offer.
fn at(step: MaintenanceStep, reason: &str) -> Heartbeat {
    reported(ORDER, step, reason)
}

/// A world named `name` of the group [`GROUP`] with hosts h1 and h2 and a run of it, whose h1 was
/// ordered and reports its turn halted with [`HALT`]; h2 waits behind the halt.
fn halted(name: &str) -> World {
    halted_of(name, &["h1", "h2"])
}

/// [`halted`] with `hosts` in the group, h1 among them.
fn halted_of(name: &str, hosts: &[&str]) -> World {
    let world = scheduled(name, table(1, 0), hosts, |_| {});
    assert!(starts(&world, "h1"));
    tell(&world, "h1", at(MaintenanceStep::Halted, HALT));
    let held = turn_of(&world, "h1");
    assert_eq!((held.step, held.reason.as_str()), (Step::Halted, HALT));
    world
}

/// The turn of `host` in the first run the world stores.
fn turn_of(world: &World, host: &str) -> RunHost {
    world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .find(|held| held.host_id == host)
        .cloned()
        .expect("the run holds the host")
}

/// The `maintain` the run's Linux host is sent with `action` under `order_try`, as the
/// coordinator decides it, for the order of the first offer.
fn expected(action: agent::MaintainAction, order_try: u64) -> Order {
    expected_under(ORDER, action, order_try)
}

/// [`expected`] under the order id `order_id`.
fn expected_under(order_id: &str, action: agent::MaintainAction, order_try: u64) -> Order {
    Order::Maintain {
        maintain: Maintain {
            run_id: RUN.to_owned(),
            action,
            kinds: vec![MaintenanceKind::Linux],
            linux_scope: agent::LinuxScope::Security,
            drain_deadline_secs: None,
            macos_path: agent::MacosPath::Install,
            macos_major: false,
            return_secs: 1200,
        },
        order_id: order_id.to_owned(),
        order_try,
    }
}

/// [`expected`] as it reaches the wire.
fn expected_wire(action: agent::MaintainAction, order_try: u64) -> agent::Order {
    agent::Order {
        order_id: ORDER.to_owned(),
        r#try: order_try,
        verb: Some(agent::order::Verb::Maintain(agent::Maintain {
            run_id: RUN.to_owned(),
            action: action as i32,
            kinds: vec![MaintenanceKind::Linux as i32],
            linux_scope: agent::LinuxScope::Security as i32,
            drain_deadline_secs: None,
            macos_path: agent::MacosPath::Install as i32,
            xcodes: Vec::new(),
            xcode_remove: Vec::new(),
            xcode_default: String::new(),
            return_secs: 1200,
            macos_major: false,
        })),
    }
}

/// `RetryHost` of `host` in the run `run`, with `reason`, by the operator.
fn retry(
    world: &World,
    run: &str,
    host: &str,
    reason: &str,
) -> Result<proto::RetryMaintenanceHostResponse, Refusal> {
    world.coordinator().maintenance_retry_host(
        &operator(),
        proto::RetryMaintenanceHostRequest {
            run_id: run.to_owned(),
            host_id: host.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// `PutBack` of `host` in the run `run`, with `reason`, by the operator.
fn put_back(
    world: &World,
    run: &str,
    host: &str,
    reason: &str,
) -> Result<proto::PutBackMaintenanceHostResponse, Refusal> {
    world.coordinator().maintenance_put_back(
        &operator(),
        proto::PutBackMaintenanceHostRequest {
            run_id: run.to_owned(),
            host_id: host.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// The hosts the alert `maintenance_halted` stands for now, by node id.
fn halted_alerts(world: &World) -> Vec<String> {
    world
        .coordinator()
        .observe_alerts()
        .expect("observed")
        .into_iter()
        .filter(|seen| seen.alert.subject.kind == AlertKind::MaintenanceHalted)
        .map(|seen| seen.alert.subject.host)
        .collect()
}

/// Proves: `RetryHost` of a halted host stores the retry queued under try 2 and answers the host
/// as stored (still halted, with its reason) and the one audit entry `maintenance_retry_host` by
/// the operator, with the host as `node_id`, its group as `host_group` and the reason followed by
/// `run <id>`; the host's next heartbeat is sent exactly a `maintain` with the action `RETRY`, the
/// order id of its turn, try 2 and the run's kinds, scope and `return_secs`, as the wire carries
/// it; a heartbeat that still reports the turn halted is sent it again, and nothing changes in
/// the store by it.
///
/// Catches: a retry that is never sent, one sent as a `RUN` or under try 1 (which the daemon
/// takes for the order it holds), one sent once only, and a call that writes no entry or two.
#[test]
fn retry_host_queues_a_retry_that_the_hosts_heartbeats_are_sent() {
    let world = halted("mo-retry");
    let audited = world.audit().len();

    let answered = retry(&world, RUN, "h1", REASON).expect("queued");
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.queued, held.order_try),
        (Step::Halted, Some(Act::Retry), 2)
    );
    assert_eq!(answered.host, Some(host_proto(&held)));
    let shown = answered.host.expect("a host");
    assert_eq!(
        (shown.step, shown.reason.as_str()),
        (MaintenanceHostStep::Halted as i32, HALT)
    );
    let wanted = operator_entry(
        audited as u64 + 1,
        "maintenance_retry_host",
        Some(GROUP),
        Some("h1"),
        &format!("{REASON} run {RUN}"),
    );
    assert_eq!(answered.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit_after(audited), vec![wanted]);

    let stored = (world.ops().maintenance_runs, world.audit().len());
    for _ in 0..2 {
        let order = tell(&world, "h1", at(MaintenanceStep::Halted, HALT)).expect("an order");
        assert_eq!(order, expected(agent::MaintainAction::Retry, 2));
        assert_eq!(
            order.to_proto(),
            expected_wire(agent::MaintainAction::Retry, 2)
        );
    }
    assert_eq!(
        (world.ops().maintenance_runs, world.audit().len()),
        stored,
        "sending the order stores nothing"
    );
}

/// Proves: `PutBack` of a halted host stores the put back queued under try 2, audited
/// `maintenance_put_back` as `RetryHost` is, and the host's heartbeats are sent a `maintain` with
/// the action `PUT_BACK` under try 2, exactly, again while it still reports the turn halted.
///
/// Catches: a put back sent as a retry, and one that is audited as a retry.
#[test]
fn put_back_queues_a_put_back_that_the_hosts_heartbeats_are_sent() {
    let world = halted("mo-put-back");
    let audited = world.audit().len();

    let answered = put_back(&world, RUN, "h1", REASON).expect("queued");
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.queued, held.order_try),
        (Step::Halted, Some(Act::PutBack), 2)
    );
    assert_eq!(answered.host, Some(host_proto(&held)));
    let wanted = operator_entry(
        audited as u64 + 1,
        "maintenance_put_back",
        Some(GROUP),
        Some("h1"),
        &format!("{REASON} run {RUN}"),
    );
    assert_eq!(answered.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit_after(audited), vec![wanted]);

    for _ in 0..2 {
        let order = tell(&world, "h1", at(MaintenanceStep::Halted, HALT)).expect("an order");
        assert_eq!(order, expected(agent::MaintainAction::PutBack, 2));
        assert_eq!(
            order.to_proto(),
            expected_wire(agent::MaintainAction::PutBack, 2)
        );
    }
}

/// Proves: a second `RetryHost` while the host is still halted queues the retry again under try
/// 3, and a `PutBack` after it queues the put back under try 4 in place of the retry: the host is
/// sent the newest act only, each call writes its own entry.
///
/// Catches: an act queued twice under one try (the daemon would take the second for the first),
/// and a queue that keeps the older act when the operator changed their mind.
#[test]
fn a_repeated_call_queues_a_newer_try_in_place_of_the_act_queued() {
    let world = halted("mo-repeat");
    let audited = world.audit().len();
    let sent = |world: &World| tell(world, "h1", at(MaintenanceStep::Halted, HALT));

    retry(&world, RUN, "h1", REASON).expect("queued");
    retry(&world, RUN, "h1", REASON).expect("queued again");
    let held = turn_of(&world, "h1");
    assert_eq!((held.queued, held.order_try), (Some(Act::Retry), 3));
    assert_eq!(
        sent(&world),
        Some(expected(agent::MaintainAction::Retry, 3))
    );

    put_back(&world, RUN, "h1", REASON).expect("replaced");
    let held = turn_of(&world, "h1");
    assert_eq!((held.queued, held.order_try), (Some(Act::PutBack), 4));
    assert_eq!(
        sent(&world),
        Some(expected(agent::MaintainAction::PutBack, 4))
    );

    let actions: Vec<String> = world
        .audit_after(audited)
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert_eq!(
        actions,
        [
            "maintenance_retry_host",
            "maintenance_retry_host",
            "maintenance_put_back"
        ]
    );
}

/// Proves: once the host's daemon reports its turn at another step after a retry, that step is
/// stored, the queued act is cleared and the try is kept, no `maintain` is sent at that heartbeat
/// or after it, and the alert `maintenance_halted` that stood while the host was halted is gone;
/// when the host halts again, a new `RetryHost` queues try 3 and the host is sent it.
///
/// Catches: an act that is sent again after the daemon took it (the daemon would refuse it, but
/// the coordinator would send it for ever), an alert that outlives the halt, and a try that starts
/// over so the daemon refuses the next act as stale.
#[test]
fn a_report_of_another_step_clears_the_retry_and_the_next_one_is_a_newer_try() {
    let world = halted("mo-retry-reported");
    assert_eq!(halted_alerts(&world), ["h1"]);
    retry(&world, RUN, "h1", REASON).expect("queued");
    assert_eq!(halted_alerts(&world), ["h1"], "queued, still halted");

    let order = tell(&world, "h1", at(MaintenanceStep::Applying, ""));
    assert_eq!(maintain_id(order.as_ref()), None);
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.queued, held.order_try, held.reason.as_str()),
        (Step::Applying, None, 2, "")
    );
    assert_eq!(halted_alerts(&world), Vec::<String>::new());
    let order = tell(&world, "h1", at(MaintenanceStep::Applying, ""));
    assert_eq!(maintain_id(order.as_ref()), None, "nothing more is sent");

    let again = || at(MaintenanceStep::Halted, "apply linux: exit 2");
    tell(&world, "h1", again());
    assert_eq!(halted_alerts(&world), ["h1"]);
    assert_eq!(turn_of(&world, "h1").queued, None);
    retry(&world, RUN, "h1", REASON).expect("queued");
    let held = turn_of(&world, "h1");
    assert_eq!((held.queued, held.order_try), (Some(Act::Retry), 3));
    assert_eq!(
        tell(&world, "h1", again()),
        Some(expected(agent::MaintainAction::Retry, 3))
    );
}

/// Proves: once the host's daemon reports its turn putting back and then done after a `PutBack`,
/// each step is stored, the queued act is cleared, nothing more is sent, and the alert is gone;
/// the run's group is no longer halted.
///
/// Catches: a put back that is sent after the daemon ran it, and a host kept halted by the act it
/// was given.
#[test]
fn a_report_of_putting_back_and_done_clears_the_put_back() {
    let world = halted("mo-put-back-reported");
    put_back(&world, RUN, "h1", REASON).expect("queued");
    assert_eq!(halted_alerts(&world), ["h1"]);

    let order = tell(&world, "h1", at(MaintenanceStep::PuttingBack, ""));
    assert_eq!(maintain_id(order.as_ref()), None);
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.queued, held.order_try),
        (Step::PuttingBack, None, 2)
    );
    assert_eq!(halted_alerts(&world), Vec::<String>::new());

    let order = tell(&world, "h1", at(MaintenanceStep::Done, ""));
    assert_eq!(maintain_id(order.as_ref()), None);
    let held = turn_of(&world, "h1");
    assert_eq!((held.step, held.queued), (Step::Done, None));
    assert_eq!(
        world.ops().maintenance_runs[0].halted_groups(),
        Vec::<String>::new()
    );
}

/// Proves: the run's group stays halted after `RetryHost` and after `PutBack` until the host
/// reports the outcome: the run is `halted` with the group in `halted_groups`, and the group's
/// other host, which waits, is not started at its heartbeat.
///
/// Catches: a queued act that frees the group's batch place or lifts the halt before the daemon
/// has acted, so that a second host is drained beside a host still cordoned.
#[test]
fn the_group_stays_halted_until_the_host_reports_the_outcome() {
    for (name, retried) in [("mo-group-retry", true), ("mo-group-put-back", false)] {
        let world = halted(name);
        if retried {
            retry(&world, RUN, "h1", REASON).expect("queued");
        } else {
            put_back(&world, RUN, "h1", REASON).expect("queued");
        }
        let run = get(&world, RUN).expect("the run").run.expect("a run");
        assert_eq!(
            (run.state(), run.halted_groups),
            (MaintenanceState::Halted, vec![GROUP.to_owned()]),
            "{name}"
        );
        assert!(!starts(&world, "h2"), "{name}: the group is halted");
        assert_eq!(turn_of(&world, "h2").step, Step::Waiting, "{name}");
    }
}

/// Proves: a host offered its turn again after a deferral starts with its try reset: a halted
/// host that was retried (try 2) and then reports the turn deferred keeps try 2 with no act
/// queued; when it is offered again, under the next order id, it is sent a `RUN` under try 1 and
/// its stored try is 0 again.
///
/// Catches: an offer after a deferral sent under the try of an earlier act, or with the act
/// still queued.
#[test]
fn a_host_offered_again_after_a_deferral_starts_under_try_one() {
    let world = halted_of("mo-offered-again", &["h1"]);
    retry(&world, RUN, "h1", REASON).expect("queued");
    tell(&world, "h1", at(MaintenanceStep::Deferred, "stage: exit 75"));
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.queued, held.order_try),
        (Step::Deferred, None, 2)
    );

    edit_runs(&world, |runs| runs[0].hosts[0].since_unix = UNIX - 10_000);
    let order = tell(&world, "h1", at(MaintenanceStep::Deferred, ""));
    assert_eq!(
        order,
        Some(expected_under(ORDER_2, agent::MaintainAction::Run, 1))
    );
    let held = turn_of(&world, "h1");
    assert_eq!(
        (held.step, held.offered_again, held.queued, held.order_try),
        (Step::Staging, 1, None, 0)
    );
}

/// Proves: every refusal of `RetryHost` and of `PutBack` changes nothing and writes no audit
/// entry: CI's token `forbidden` and none `unauthorized`; a run id, a host id or a reason that
/// breaks its rule `invalid_request`; a run no one holds `unknown_run`; a host the run does not
/// hold `not_in_run`; a host waiting, staging, rebooting, not back or done `not_halted`; a store
/// that fails `store_failed` and an audit line that cannot be kept `audit_unavailable`.
///
/// Catches: an act queued on a turn that is not halted (the daemon would act on a turn in
/// progress), a refusal that queues the act all the same, and a call that is audited before it is
/// refused.
#[test]
fn every_refusal_changes_nothing_and_writes_no_audit_entry() {
    let world = halted("mo-refusals");
    let at_step = |id: &str, step: Step| host_at(id, GROUP, step, UNIX, &[Kind::Linux]);
    seed_runs(
        &world,
        vec![crate::maintenance_calls_tests::run_of(
            "run-2-1",
            vec![
                at_step("w1", Step::Waiting),
                at_step("s1", Step::Staging),
                at_step("r1", Step::Rebooting),
                at_step("n1", Step::NotBack),
                at_step("d1", Step::Done),
                at_step("x1", Step::Halted),
            ],
        )],
    );
    let before = kept(&world);
    let coordinator = world.coordinator();
    let request = |run: &str, host: &str, reason: &str| {
        (
            proto::RetryMaintenanceHostRequest {
                run_id: run.to_owned(),
                host_id: host.to_owned(),
                reason: reason.to_owned(),
            },
            proto::PutBackMaintenanceHostRequest {
                run_id: run.to_owned(),
                host_id: host.to_owned(),
                reason: reason.to_owned(),
            },
        )
    };
    for (caller, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        let (retried, put) = request("run-2-1", "x1", REASON);
        assert_eq!(
            coordinator
                .maintenance_retry_host(&caller, retried)
                .expect_err("refused"),
            refusal
        );
        assert_eq!(
            coordinator
                .maintenance_put_back(&caller, put)
                .expect_err("refused"),
            refusal
        );
    }

    let long_id = "a".repeat(65);
    let long = "x".repeat(257);
    let invalid: [(&str, &str, &str); 9] = [
        ("", "x1", REASON),
        ("has space", "x1", REASON),
        (long_id.as_str(), "x1", REASON),
        ("run-2-1", "", REASON),
        ("run-2-1", "X1", REASON),
        ("run-2-1", "-x1", REASON),
        ("run-2-1", "x1", ""),
        ("run-2-1", "x1", long.as_str()),
        ("run-2-1", "x1", "a\u{7}b"),
    ];
    let mut refused: Vec<(&str, &str, &str, Refusal)> = invalid
        .iter()
        .map(|(run, host, reason)| (*run, *host, *reason, Refusal::InvalidRequest))
        .collect();
    refused.push(("run-2-1", "x1", "  ", Refusal::InvalidRequest));
    refused.push(("run-9-9", "x1", REASON, Refusal::UnknownRun));
    refused.push(("run-2-1", "zz", REASON, Refusal::NotInRun));
    for host in ["w1", "s1", "r1", "n1", "d1"] {
        refused.push(("run-2-1", host, REASON, Refusal::NotHalted));
    }
    for (run, host, reason, refusal) in refused {
        let (retried, put) = request(run, host, reason);
        assert_eq!(
            coordinator
                .maintenance_retry_host(&operator(), retried)
                .expect_err("refused"),
            refusal,
            "retry {run} {host} {reason:?}"
        );
        assert_eq!(
            coordinator
                .maintenance_put_back(&operator(), put)
                .expect_err("refused"),
            refusal,
            "put back {run} {host} {reason:?}"
        );
    }

    for (failure, refusal) in [
        (
            crate::store::store_failure as fn() -> _,
            Refusal::StoreFailed,
        ),
        (crate::store::audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(
            retry(&world, RUN, "h1", REASON).expect_err("refused"),
            refusal
        );
        assert_eq!(
            put_back(&world, RUN, "h1", REASON).expect_err("refused"),
            refusal
        );
    }
    world.fake.heal();
    assert_eq!(kept(&world), before);
    retry(&world, RUN, "h1", REASON).expect("a healthy store takes it");
}

/// Proves: in a run that was cancelled, `RetryHost` of its halted host is `run_ended` and changes
/// nothing, while `PutBack` of the same host is taken: it queues the put back under try 2, audits
/// it, and the host is sent it at its next heartbeat.
///
/// Catches: a halted host that stays cordoned for good because its run ended, and a retry of a
/// run that no longer goes on.
#[test]
fn a_cancelled_runs_halted_host_is_put_back_and_not_retried() {
    let world = halted("mo-cancelled");
    cancel(&world, RUN, "stop it").expect("cancelled");
    let before = kept(&world);
    assert_eq!(
        retry(&world, RUN, "h1", REASON).expect_err("refused"),
        Refusal::RunEnded
    );
    assert_eq!(kept(&world), before);

    let audited = world.audit().len();
    put_back(&world, RUN, "h1", REASON).expect("taken");
    let held = turn_of(&world, "h1");
    assert_eq!((held.queued, held.order_try), (Some(Act::PutBack), 2));
    assert_eq!(world.audit_after(audited).len(), 1);
    assert_eq!(
        tell(&world, "h1", at(MaintenanceStep::Halted, HALT)),
        Some(expected(agent::MaintainAction::PutBack, 2))
    );
}
