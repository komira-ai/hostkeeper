//! `Hosts.Retry` from the outside, on the install of a service that failed on a host: the failed
//! try's report keeps the host from being ordered again through a cleared halt, an exclusion and
//! an inclusion; `Hosts.Retry` sets it aside, the host is ordered the next try at its next
//! heartbeat, and `Releases.Status` and `Hosts.Get` show that order as the host's current one, not
//! the stale report. Then the refusals, the repeat, and a restart. Every time is the rig's clock;
//! nothing sleeps.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{OrderId, Outcome, Phase, ServiceBeat, Token, UpdateResult};

use crate::assign_support::{absent, install, order_to, why_of};
use crate::audit_refusal_tests::full_disk;
use crate::decide::{Gate, Skip, Why};
use crate::grpc::report;
use crate::marks_support::{
    INSTALLER, WEB, ask, beat_of, operator, restart, rig, runs, set, start,
};
use crate::operator::Caller;
use crate::orders::Order;
use crate::rollback_tests::unwritable;
use crate::testutil::{OPERATOR, Rig, version};

/// The install of `web` 2.0 from the package of digest 7 at the try `order_try`.
fn install_try(order_try: u64) -> Order {
    let mut order = install("2.0", 7);
    if let Order::Install { order_try: at, .. } = &mut order {
        *at = order_try;
    }
    order
}

/// The id of the install of `web` 2.0 from the package of digest 7.
fn install_id() -> String {
    match install("2.0", 7) {
        Order::Install { order_id, .. } => order_id,
        _ => unreachable!("an install"),
    }
}

/// The daemon's verdict of the install of `web` 2.0 at the try `order_try`: failed, halted, for
/// `reason`.
fn failed_install(order_try: u64, reason: &str) -> UpdateResult {
    let mut verdict = UpdateResult::new(version("2.0"), Outcome::Failed);
    verdict.phase = Some(Phase::Halt);
    verdict.reason = Token::parse(reason);
    verdict.order_id = OrderId::parse(&install_id());
    verdict.order_try = Some(order_try);
    verdict
}

/// `web` as a host lists it after an install that placed nothing: absent at 2.0, with `last` as
/// its verdict.
fn absent_with(last: Option<UpdateResult>) -> ServiceBeat {
    let mut one = absent(WEB, "2.0");
    one.last_update = last;
    one
}

/// The operator's `Hosts.Retry` of `service` on `node`.
fn retry(
    rig: &Rig,
    node: &str,
    service: &str,
) -> Result<proto::RetryResponse, proto::HostsRefusal> {
    retry_as(rig, &operator(), node, service)
}

/// `Hosts.Retry` of `service` on `node` by `caller`.
fn retry_as(
    rig: &Rig,
    caller: &Caller,
    node: &str,
    service: &str,
) -> Result<proto::RetryResponse, proto::HostsRefusal> {
    rig.coordinator.retry(
        caller,
        &proto::RetryRequest {
            node_id: node.to_owned(),
            service: service.to_owned(),
        },
    )
}

/// An operator's call on the frozen JSON route, `body` its JSON.
fn operate(rig: &Rig, body: &str) {
    rig.coordinator
        .operate(Some(OPERATOR), body.as_bytes())
        .expect("done");
}

/// The entries of the audit log whose action is `retry`.
fn retried(rig: &Rig) -> Vec<proto::AuditEntry> {
    rig.coordinator
        .audit_list(&operator(), proto::ListAuditRequest::default())
        .expect("the audit log")
        .entries
        .into_iter()
        .filter(|entry| entry.action == "retry")
        .collect()
}

/// How `Releases.Status` shows `web` on `node`.
fn status_view(rig: &Rig, node: &str) -> proto::HostService {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("the status");
    let service = report::status(&status, WEB)
        .services
        .into_iter()
        .next()
        .expect("the service");
    service
        .hosts
        .into_iter()
        .find(|h| h.node_id == node)
        .and_then(|h| h.view)
        .expect("the host's view")
}

/// How `Hosts.Get` shows `web` on `node`.
fn host_view(rig: &Rig, node: &str) -> proto::HostService {
    rig.coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: node.to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
        .services
        .into_iter()
        .find(|s| s.service == WEB)
        .expect("web")
}

/// A rig where `d1` was ordered the install of `web` 2.0 at try 1 and failed it `unit_failed`
/// with a halt, which halted `web`, and an operator cleared the halt.
async fn failed_and_cleared(test: &str) -> Rig {
    let rig = rig(test, "");
    set(&rig, WEB, "2.0", 7).await;
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install_try(1)));
    let failed = absent_with(Some(failed_install(1, "unit_failed")));
    assert_eq!(order_to(&rig, "d1", vec![failed]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Halted)));
    operate(
        &rig,
        r#"{"action":"clear_halt","service":"web","reason":"the dpkg lock is free"}"#,
    );
    rig
}

/// Proves: a host whose install failed at try 1 and whose halt an operator cleared is not ordered
/// again, whether it beats with the failed report, is excluded, or is included again; after
/// `Hosts.Retry` its next heartbeat, with the same stale report, is ordered the install at try
/// 2, with the same order id. Until the host answers, `Releases.Status` and `Hosts.Get` show that
/// order (id, try 2, version) and no last update, and its heartbeats resend the same try 2 order
/// rather than read the stale report as the new order failing. When the host reports try 2
/// failing, the halt is raised again and the host is held back again.
///
/// Catches: the stale report of try 1 read as the result of the new order (no order after a
/// clear and an inclusion, or the new order ended by its own predecessor's report at the next
/// beat and the try moved on), an order at try 1 again (which the daemon refuses as a stale
/// try), a status that shows the try 1 failure as the host's current state, and a retry that
/// keeps setting aside the new try's failure.
#[tokio::test]
async fn after_a_cleared_halt_and_an_inclusion_retry_orders_the_next_try() {
    let rig = failed_and_cleared("retry-orders-try-two").await;
    let stale = || vec![absent_with(Some(failed_install(1, "unit_failed")))];
    assert_eq!(order_to(&rig, "d1", stale()), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));

    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: WEB.to_owned(),
                node_id: "d1".to_owned(),
                reason: "the dpkg lock was held".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    assert_eq!(order_to(&rig, "d1", stale()), None);
    operate(
        &rig,
        r#"{"action":"include","service":"web","node_id":"d1"}"#,
    );
    assert_eq!(order_to(&rig, "d1", stale()), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));
    assert_eq!(
        status_view(&rig, "d1").gate_reason(),
        proto::GateReason::FailedHere
    );

    let answer = retry(&rig, "d1", WEB).expect("set aside");
    let set_aside = answer.set_aside.expect("the report");
    assert_eq!(set_aside.order_try, Some(1));
    assert_eq!(set_aside.order_id.as_deref(), Some(install_id().as_str()));
    assert_eq!(set_aside.reason, "unit_failed");
    let entry = answer.entry.expect("an audit entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.service.as_deref(),
            entry.node_id.as_deref()
        ),
        ("retry", Some(WEB), Some("d1"))
    );
    assert_eq!(retried(&rig).len(), 1);
    assert_eq!(why_of(&rig, WEB, "d1"), None);

    assert_eq!(order_to(&rig, "d1", stale()), Some(install_try(2)));
    let current = proto::CurrentOrder {
        order_id: Some(install_id()),
        order_try: 2,
        version: "2.0".to_owned(),
        uninstall: false,
    };
    for view in [status_view(&rig, "d1"), host_view(&rig, "d1")] {
        assert_eq!(view.order.as_ref(), Some(&current));
        assert_eq!(view.last_update, None);
        assert_ne!(view.gate_reason(), proto::GateReason::FailedHere);
    }
    rig.at(2_015);
    assert_eq!(order_to(&rig, "d1", stale()), Some(install_try(2)));

    let mut updating = absent_with(Some(failed_install(1, "unit_failed")));
    updating.updating = Some(version("2.0"));
    assert_eq!(order_to(&rig, "d1", vec![updating]), None);
    assert_eq!(status_view(&rig, "d1").order, None);
    let again = vec![absent_with(Some(failed_install(2, "unit_failed")))];
    assert_eq!(order_to(&rig, "d1", again.clone()), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Halted)));
    let view = status_view(&rig, "d1");
    assert_eq!(view.order, None);
    assert_eq!(
        view.last_update.and_then(|r| r.order_try),
        Some(2),
        "the new try's failure is the host's report"
    );
    operate(
        &rig,
        r#"{"action":"clear_halt","service":"web","reason":"looked again"}"#,
    );
    assert_eq!(order_to(&rig, "d1", again), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));
}

/// Proves: a second `Hosts.Retry` while the report is set aside answers the same report, writes
/// no audit line and changes nothing; a host that runs its service with no failure, one that has
/// no word of the service, and one that failed the version it runs (a halt after its switch) are
/// refused `nothing_to_retry`; an unknown host, an unknown service, a bad node id and callers
/// that are not the operator are refused, and nothing is audited.
///
/// Catches: a repeat that writes a second `retry` line or answers no report, a retry of a host
/// with nothing to retry accepted (audited, or a running host's report taken away), and a
/// retry the gate would answer `up_to_date` accepted though it orders nothing.
#[tokio::test]
async fn retry_refuses_what_it_cannot_set_aside_and_a_repeat_writes_nothing() {
    let rig = failed_and_cleared("retry-refusals").await;
    let first = retry(&rig, "d1", WEB).expect("set aside");
    let second = retry(&rig, "d1", WEB).expect("set aside already");
    assert_eq!(second.set_aside, first.set_aside);
    assert_eq!(second.entry, None);
    assert_eq!(retried(&rig).len(), 1);

    ask(
        &rig,
        "d2",
        "daemon",
        beat_of(INSTALLER, vec![runs(WEB, "1.0")]),
    );
    let mut halted_after_switch = runs(WEB, "2.0");
    halted_after_switch.health = hostkeeper_wire::wire::Health::Unhealthy;
    halted_after_switch.last_update = Some(failed_install(1, "verify_failed"));
    ask(
        &rig,
        "d3",
        "daemon",
        beat_of(INSTALLER, vec![halted_after_switch]),
    );
    for (node, service, refusal) in [
        ("d2", WEB, proto::HostsRefusal::NothingToRetry),
        (
            "d2",
            crate::marks_support::DB,
            proto::HostsRefusal::NothingToRetry,
        ),
        ("d3", WEB, proto::HostsRefusal::NothingToRetry),
        ("d9", WEB, proto::HostsRefusal::UnknownHost),
        ("d1", "nope", proto::HostsRefusal::UnknownService),
        ("D1", WEB, proto::HostsRefusal::InvalidRequest),
    ] {
        assert_eq!(retry(&rig, node, service), Err(refusal), "{node} {service}");
    }
    for (caller, refusal) in [
        (Caller::Ci, proto::HostsRefusal::Forbidden),
        (Caller::Anonymous, proto::HostsRefusal::Unauthorized),
    ] {
        assert_eq!(retry_as(&rig, &caller, "d1", WEB), Err(refusal));
    }
    assert_eq!(retried(&rig).len(), 1);
}

/// Proves: a `Hosts.Retry` whose audit line cannot be written is refused `store_failed` or
/// `audit_unavailable`, and the host is still not ordered (`failed_here`); once the audit file
/// takes lines again the same call sets the report aside, and the gate holds the host back no
/// more.
///
/// Catches: a report set aside before its audit line is written, or a failed write answered as
/// accepted.
#[tokio::test]
async fn retry_whose_audit_line_cannot_be_written_sets_nothing_aside() {
    let mut rig = failed_and_cleared("retry-store-failed").await;
    let state = rig.coordinator.ops().state();
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(state));
    let stale = || vec![absent_with(Some(failed_install(1, "unit_failed")))];
    rig.at(2_100);
    assert_eq!(order_to(&rig, "d1", stale()), None);
    assert_eq!(
        retry(&rig, "d1", WEB),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(order_to(&rig, "d1", stale()), None);

    let (rig, down) = full_disk("retry-audit-unavailable").await;
    rig.at(2_100);
    let failed = || {
        let mut one = runs(WEB, "1.0");
        one.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Failed));
        beat_of(INSTALLER, vec![one])
    };
    ask(&rig, "d2", "daemon", failed());
    assert_eq!(
        retry(&rig, "d2", WEB),
        Err(proto::HostsRefusal::AuditUnavailable)
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Skip(Skip::FailedHere)));
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    retry(&rig, "d2", WEB).expect("set aside");
    assert_eq!(why_of(&rig, WEB, "d2"), None);
}

/// Proves: what `Hosts.Retry` set aside is kept in memory only: after a restart of the
/// coordinator the stale report holds the host back again (`failed_here`), and the operator's
/// second call orders it, at the try past the failed one, which the restarted coordinator reads
/// from the report.
///
/// Catches: a restarted coordinator that orders try 1 again (the daemon's `stale_try`), or that
/// cannot set the report aside again.
#[tokio::test]
async fn after_a_restart_the_operator_retries_again_and_the_try_still_moves_on() {
    let mut rig = failed_and_cleared("retry-restart").await;
    retry(&rig, "d1", WEB).expect("set aside");
    restart(&mut rig, "");
    rig.at(2_100);
    let stale = || vec![absent_with(Some(failed_install(1, "unit_failed")))];
    assert_eq!(order_to(&rig, "d1", stale()), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::FailedHere)));
    retry(&rig, "d1", WEB).expect("set aside");
    assert_eq!(order_to(&rig, "d1", stale()), Some(install_try(2)));
}
