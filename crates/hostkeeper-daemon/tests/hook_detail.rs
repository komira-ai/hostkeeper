//! The hook's own `detail` of how an update ended, as the heartbeat loop reports it
//! (`OrderResult::detail`, `UpdateResult.detail` of `hostkeeper.agent.v1`): the step record keeps
//! it under the reason of its line, and the verdict of the order, sent now or answered again from
//! the record, carries it.

mod support;
mod support_apt;

use std::sync::Arc;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::link_adapters::{DaemonOrders, MAX_DETAIL, settled_result};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_link::{
    Entry, Order, OrderId, OrderResult, OrderSink, Outcome, PackageDigest, Reason, Target, Verb,
};
use hostkeeper_steps::record::{HookDetail, State};
use hostkeeper_steps::runner::DEFAULT_RETRY_SECS;
use support::name;
use support::selfupdate::record_of;
use support_apt::{AptRig, services};

/// What js-agent's hook says when its state root is not a private directory of its user.
const STATE_ROOT: &str = "state_root: /var/db/js-agent/state: owned by uid 501, not by uid 0";

/// An update of `service` to `version`, its package published in `apt`'s repository; a service
/// that runs nothing yet runs the base version first.
fn update_of(apt: &AptRig, service: &str, version: &str, order_try: u64) -> Order {
    services::based(apt, service);
    let entry = services::publish(apt, service, version);
    Order {
        id: OrderId::new(&format!("order-{service}-{version}")).expect("an order id"),
        order_try,
        verb: Verb::Update(Target {
            service: name(service),
            version: Version::new(version).expect("a version"),
            digest: PackageDigest::new(&entry.digest).expect("a package digest"),
            index_digest: None,
            entry: Some(Entry::new(entry)),
        }),
    }
}

fn version_of(order: &Order) -> Version {
    order.target().expect("a target").version.clone()
}

fn token(text: &str) -> Reason {
    Reason::new(text).expect("a reason token")
}

/// The verdict of `order` that reports nothing but how it ended.
fn plain(order: &Order, outcome: Outcome) -> OrderResult {
    OrderResult {
        order: order.reference(),
        target: version_of(order),
        outcome,
        phase: None,
        class: None,
        reason: None,
        forced: false,
        detail: None,
    }
}

/// The hook's last line: exit `code` is the caller's, the line says `reason` and `detail`.
fn line(reason: &str, detail: &str) -> String {
    format!(r#"{{"class":"environment","reason":"{reason}","detail":"{detail}"}}"#)
}

/// Catches the hook's text being dropped on the way to the heartbeat, the defect that left a
/// halt with nothing but `state_invalid`: a `prepare` that halts saying why is reported `failed`
/// in the phase `halt` with its reason and its detail, the step record keeps the detail under
/// that reason, and a resend of the order, answered from the record, says the same.
#[test]
fn a_halt_reports_the_hooks_detail_now_and_when_it_is_answered_again() {
    let apt = AptRig::new("detail-halt");
    let rig = &apt.rig;
    services::installed(&apt, "js-agent", "1.0.0");
    let said = line("state_invalid", STATE_ROOT);
    rig.launcher
        .replies("js-agent", Step::Prepare, &[(76, said.as_str())]);
    let orders = DaemonOrders::new(Arc::new(apt.daemon()));
    let order = update_of(&apt, "js-agent", "2.0.0", 1);
    let halted = OrderResult {
        phase: Some(Phase::Halt),
        reason: Some(token("state_invalid")),
        detail: Some(STATE_ROOT.to_owned()),
        ..plain(&order, Outcome::Failed)
    };
    assert_eq!(orders.execute(&order, &version_of(&order)), halted);
    let record = rig.record("js-agent").expect("the step record");
    assert_eq!(
        record.progress.detail,
        Some(HookDetail {
            reason: Some("state_invalid".to_owned()),
            text: STATE_ROOT.to_owned(),
        })
    );
    assert_eq!(record.finished_detail(), Some(STATE_ROOT));
    assert_eq!(orders.execute(&order, &version_of(&order)), halted);
    assert_eq!(rig.launcher.steps("js-agent"), [Step::Prepare]);
}

/// Catches the detail of the step that failed being lost when `rollback` says nothing, and a
/// deferral losing its text: a `prepare` that fails with a reason and a detail, followed by a
/// `rollback` whose line has neither, is reported with the `prepare`'s reason and detail; a
/// `prepare` that defers saying why is reported deferred with its text.
#[test]
fn a_failure_keeps_the_detail_of_the_step_that_failed_and_a_deferral_its_own() {
    let apt = AptRig::new("detail-failed");
    let rig = &apt.rig;
    let failed_line = line("bad_unit", "the installed unit plan cannot be read");
    rig.launcher
        .replies("alpha", Step::Prepare, &[(1, failed_line.as_str())]);
    rig.launcher.replies("alpha", Step::Rollback, &[(0, "{}")]);
    let busy_line = line("drain_bound", "3 jobs still run");
    rig.launcher
        .replies("beta", Step::Prepare, &[(75, busy_line.as_str())]);
    let orders = DaemonOrders::new(Arc::new(apt.daemon()));
    let failed = update_of(&apt, "alpha", "2.0.0", 1);
    assert_eq!(
        orders.execute(&failed, &version_of(&failed)),
        OrderResult {
            phase: Some(Phase::BeforeCommit),
            class: Some(Class::Environment),
            reason: Some(token("bad_unit")),
            detail: Some("the installed unit plan cannot be read".to_owned()),
            ..plain(&failed, Outcome::Failed)
        }
    );
    let deferred = update_of(&apt, "beta", "2.0.0", 1);
    assert_eq!(
        orders.execute(&deferred, &version_of(&deferred)),
        OrderResult {
            reason: Some(token("drain_bound")),
            detail: Some("3 jobs still run".to_owned()),
            ..plain(
                &deferred,
                Outcome::Deferred {
                    retry_after_secs: DEFAULT_RETRY_SECS
                }
            )
        }
    );
}

/// Catches a detail carried over from an earlier order, and one sent whole however long it is:
/// the next order of the service, which succeeds saying nothing, is reported with no detail; a
/// detail longer than [`MAX_DETAIL`] bytes is cut to it at a character boundary.
#[test]
fn a_later_order_has_no_earlier_detail_and_a_long_one_is_cut() {
    let apt = AptRig::new("detail-bounds");
    let rig = &apt.rig;
    // One byte short of the bound, then a two-byte character that would cross it.
    let long = format!("{}é tail", "x".repeat(MAX_DETAIL - 1));
    let long_line = line("bad_unit", &long);
    rig.launcher.replies(
        "alpha",
        Step::Prepare,
        &[(1, long_line.as_str()), (0, "{}")],
    );
    rig.launcher.replies("alpha", Step::Rollback, &[(0, "{}")]);
    let orders = DaemonOrders::new(Arc::new(apt.daemon()));
    let first = update_of(&apt, "alpha", "2.0.0", 1);
    let result = orders.execute(&first, &version_of(&first));
    assert_eq!(result.detail, Some("x".repeat(MAX_DETAIL - 1)));
    let second = update_of(&apt, "alpha", "2.1.0", 2);
    assert_eq!(
        orders.execute(&second, &version_of(&second)),
        plain(&second, Outcome::Succeeded)
    );
}

/// Catches the settled verdict of an update of hostkeeper itself losing the hook's detail, or
/// giving one kept under another reason: a finished record that kept its order id reports the
/// detail kept under its own reason, and none when the kept one is of another reason.
#[test]
fn a_settled_verdict_carries_the_detail_kept_under_its_reason() {
    let finished = State::Finished {
        verdict: Verdict::Halt,
        reason: Some("state_invalid".to_owned()),
    };
    let mut record = record_of("hostkeeper", finished);
    record.progress.report_order_id = Some("order-1".to_owned());
    record.progress.detail = Some(HookDetail {
        reason: Some("state_invalid".to_owned()),
        text: STATE_ROOT.to_owned(),
    });
    let settled = settled_result(&record).expect("a settled verdict");
    assert_eq!(settled.detail.as_deref(), Some(STATE_ROOT));
    record.progress.detail = Some(HookDetail {
        reason: Some("drain_bound".to_owned()),
        text: "3 jobs still run".to_owned(),
    });
    let settled = settled_result(&record).expect("a settled verdict");
    assert_eq!(settled.detail, None);
}
