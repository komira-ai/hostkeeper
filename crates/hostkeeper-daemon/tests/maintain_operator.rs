//! An operator's `RETRY` and `PUT_BACK` of a halted turn through `Daemon::order`
//! (docs/design/maintenance-updates.md, sections 4.2, 5.4 and 5.7): a retry launches the step that
//! failed again under the new try and goes on to done, or halts again; a put back resumes the
//! drained services in reverse and ends the turn done; each is refused `not_halted` where the turn
//! is not halted, `stale_try` under an older try, and acts once when delivered twice; neither runs
//! a `rollback`. All on the rig's scripted launcher.

mod support;
mod support_maintain;

use std::sync::Arc;

use hostkeeper_daemon::maintenance_hook::Kind as HookKind;
use hostkeeper_daemon::maintenance_state::run_state;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1::{
    MaintainAction, MaintenanceKind, MaintenanceStep, Order,
};
use hostkeeper_steps::maintenance::{MaintenanceRecord, RunStep};
use hostkeeper_steps::{CrashGate, CrashPoint};
use support::Rig;
use support_maintain::{
    HOOK, ORDER, RUN, boot, draining, launched, maintain, order_of, plain, reboot_on_cutover,
    record, terms,
};

const LINUX: [MaintenanceKind; 1] = [MaintenanceKind::Linux];

/// A rig whose services `alpha` and `beta` both drain.
fn host(label: &str) -> Rig {
    let rig = Rig::new(label);
    draining(&rig, "alpha", "1.0.0");
    draining(&rig, "beta", "1.0.0");
    rig
}

/// The order [`ORDER`] of the Linux run with `action`, try `order_try`.
fn acting(action: MaintainAction, order_try: u64) -> Order {
    let mut terms = terms(&LINUX, None);
    terms.action = action.into();
    order_of(ORDER, order_try, terms)
}

fn retry(order_try: u64) -> Order {
    acting(MaintainAction::Retry, order_try)
}

fn put_back(order_try: u64) -> Order {
    acting(MaintainAction::PutBack, order_try)
}

fn done(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent,
    }
}

fn halt(reason: &str, resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Halt,
        reason: Some(reason.to_owned()),
        resent,
    }
}

fn refused(refusal: Refusal, detail: &str) -> Reply {
    Reply::Refused {
        refusal,
        detail: detail.to_owned(),
    }
}

fn strings(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|step| (*step).to_owned()).collect()
}

/// The steps launched after the first `n`.
fn launched_after(rig: &Rig, n: usize) -> Vec<String> {
    launched(rig)[n..].to_vec()
}

/// Runs the Linux turn of [`ORDER`] try 1 with the hook's steps exiting `hook_exits` in turn, and
/// checks that it halts with `reason`; the record of the halt.
fn halted(rig: &Rig, hook_exits: &[i32], reason: &str) -> MaintenanceRecord {
    rig.launcher.additive_exits(HOOK, hook_exits);
    assert_eq!(rig.daemon().order(&maintain(&LINUX)), halt(reason, false));
    let held = record(rig).expect("the halted record");
    assert_eq!(held.halted.as_deref(), Some(reason));
    held
}

fn no_rollback(rig: &Rig) {
    assert!(
        launched(rig).iter().all(|step| !step.contains("rollback")),
        "{:?}",
        launched(rig)
    );
}

// Retry.

/// Catches a retry that re-runs the steps before the failed apply, keeps the old try, or stops
/// after the apply: the `cutover` failed, and `RETRY` try 2 launches the `cutover` again, then
/// `verify` and both `resume`s in reverse; the turn ends done, the record holds try 2, the kind
/// applied, no halt and no note, and no `rollback` ever runs.
#[test]
fn a_retry_after_a_failed_apply_reruns_the_apply_and_goes_on_to_done() {
    let rig = host("maintain-operator-retry-apply");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&retry(2)), done(false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&[
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume",
        ])
    );
    let held = record(&rig).expect("the record");
    assert_eq!(held.order_try, 2);
    assert_eq!(held.step, RunStep::Done);
    assert!(held.kinds[0].applied);
    assert!(held.drained.is_empty());
    assert_eq!((held.halted, held.put_back), (None, None));
    no_rollback(&rig);
}

/// Catches a retry of a failed check that applies the kind again: the `verify` failed, and
/// `RETRY` try 2 launches `verify` alone of the hook's steps, then both `resume`s, to done.
#[test]
fn a_retry_of_a_failed_check_reruns_the_check_alone() {
    let rig = host("maintain-operator-retry-check");
    halted(&rig, &[0, 0, 1], "check: exit 1");
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&retry(2)), done(false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&["hostkeeper verify", "beta resume", "alpha resume"])
    );
    no_rollback(&rig);
}

/// Catches a retried drain that drains the next service instead, or puts the drained ones back:
/// `beta`'s drain failed, and `RETRY` try 2 launches `beta`'s drain again and goes on from there.
#[test]
fn a_retry_of_a_failed_drain_drains_the_same_service_again() {
    let rig = host("maintain-operator-retry-drain");
    rig.launcher.additive_exits("beta", &[1]);
    assert_eq!(
        rig.daemon().order(&maintain(&LINUX)),
        halt("drain beta: exit 1", false)
    );
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&retry(2)), done(false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&[
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume",
        ])
    );
}

/// Catches a retry that fails again carrying on, or keeping the first reason: the retried
/// `verify` exits 2, so the turn halts again with the new reason under try 2, no `resume` runs,
/// and the report says halted with that reason.
#[test]
fn a_retry_that_fails_again_halts_again_with_the_new_reason() {
    let rig = host("maintain-operator-retry-again");
    halted(&rig, &[0, 0, 1], "check: exit 1");
    let before = launched(&rig).len();
    rig.launcher.additive_exits(HOOK, &[2]);
    assert_eq!(rig.daemon().order(&retry(2)), halt("check: exit 2", false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&["hostkeeper verify"])
    );
    let held = record(&rig).expect("the record");
    assert_eq!(held.order_try, 2);
    assert_eq!(held.halted.as_deref(), Some("check: exit 2"));
    let state = run_state(&held);
    assert_eq!(state.step, i32::from(MaintenanceStep::Halted));
    assert_eq!(state.halted_reason, "check: exit 2");
}

/// Catches a retry of a `cutover` that failed after asking for its reboot that waits for a reboot
/// that never came: on the boot the reboot was asked in, `RETRY` launches the `cutover` again,
/// which asks for the reboot again, and the turn waits for it (`Running`).
#[test]
fn a_retry_on_the_boot_its_reboot_was_asked_in_launches_the_cutover_again() {
    let rig = host("maintain-operator-retry-reboot-same");
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let held = halted(&rig, &[0, 1], "apply linux: exit 1");
    assert_eq!(held.kinds[0].reboot_under_way, Some(boot("boot-1")));
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&retry(2)), Reply::Running);
    assert_eq!(
        launched_after(&rig, before),
        strings(&["hostkeeper cutover"])
    );
    let waiting = record(&rig).expect("the record");
    assert_eq!(waiting.kinds[0].reboot_under_way, Some(boot("boot-1")));
    assert_eq!(waiting.halted, None);
    assert_eq!(
        run_state(&waiting).step,
        i32::from(MaintenanceStep::Rebooting)
    );
}

/// Catches a retry after the host did reboot that installs the kind a second time: the reboot was
/// asked, the `cutover` failed, the host then came back on a new boot, and `RETRY` applies the
/// kind by that reboot with no `cutover`, then checks and puts back, to done.
#[test]
fn a_retry_on_a_new_boot_applies_the_kind_without_a_second_cutover() {
    let rig = host("maintain-operator-retry-reboot-new");
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    halted(&rig, &[0, 1], "apply linux: exit 1");
    rig.reboot();
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&retry(2)), done(false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&["hostkeeper verify", "beta resume", "alpha resume"])
    );
    assert!(record(&rig).expect("the record").kinds[0].applied);
}

// Put back.

/// Catches a put back that resumes in the drain's order, runs the check or another `cutover`, or
/// leaves the halt in the report: the `cutover` failed, and `PUT_BACK` try 2 launches `beta`'s
/// then `alpha`'s `resume` and nothing else; the answer is done; the record is done under try 2
/// with the halt's reason as its note and the kind not applied; and the report says done with no
/// reason and nothing applied.
#[test]
fn a_put_back_resumes_in_reverse_and_clears_the_halt_in_the_report() {
    let rig = host("maintain-operator-put-back");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&put_back(2)), done(false));
    assert_eq!(
        launched_after(&rig, before),
        strings(&["beta resume", "alpha resume"])
    );
    let held = record(&rig).expect("the record");
    assert_eq!(held.order_try, 2);
    assert_eq!(held.step, RunStep::Done);
    assert_eq!(held.halted, None);
    assert_eq!(held.put_back.as_deref(), Some("apply linux: exit 1"));
    assert!(!held.kinds[0].applied);
    assert!(held.drained.is_empty());
    let state = run_state(&held);
    assert_eq!(state.step, i32::from(MaintenanceStep::Done));
    assert_eq!(state.halted_reason, "");
    assert!(state.applied.is_empty());
    no_rollback(&rig);
}

/// Catches a put back that drops what was applied: the check failed after the kind applied, and
/// `PUT_BACK` leaves the kind applied, in the record and in the report.
#[test]
fn a_put_back_after_the_apply_keeps_the_kind_applied() {
    let rig = host("maintain-operator-put-back-applied");
    halted(&rig, &[0, 0, 1], "check: exit 1");
    assert_eq!(rig.daemon().order(&put_back(2)), done(false));
    let held = record(&rig).expect("the record");
    assert!(held.kinds[0].applied);
    assert_eq!(
        run_state(&held).applied,
        [i32::from(MaintenanceKind::Linux)]
    );
}

/// Catches a put back of a host that was never drained that launches anything: no service names
/// a `drain`, the check failed, and `PUT_BACK` launches nothing and answers done.
#[test]
fn a_put_back_of_a_host_never_drained_launches_nothing_and_is_done() {
    let rig = Rig::new("maintain-operator-put-back-undrained");
    plain(&rig, "alpha", "1.0.0");
    let held = halted(&rig, &[0, 0, 1], "check: exit 1");
    assert!(held.drained.is_empty());
    let before = launched(&rig).len();
    assert_eq!(rig.daemon().order(&put_back(2)), done(false));
    assert_eq!(launched_after(&rig, before), Vec::<String>::new());
    let after = record(&rig).expect("the record");
    assert_eq!(after.step, RunStep::Done);
    assert_eq!(after.put_back.as_deref(), Some("check: exit 1"));
}

/// Catches a put back whose `resume` fails being carried past: `beta`'s `resume` exits 1, so the
/// turn halts again with that reason, and `alpha` is not resumed.
#[test]
fn a_put_back_whose_resume_fails_halts_again() {
    let rig = host("maintain-operator-put-back-fails");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    rig.launcher.additive_exits("beta", &[1]);
    let before = launched(&rig).len();
    assert_eq!(
        rig.daemon().order(&put_back(2)),
        halt("resume beta: exit 1", false)
    );
    assert_eq!(launched_after(&rig, before), strings(&["beta resume"]));
}

// Delivered twice.

/// Catches an action delivered twice that acts twice: the second delivery of `RETRY` try 2, and
/// of `PUT_BACK` try 2, answers the end of the first (`resent`) and launches nothing more.
#[test]
fn an_action_delivered_twice_acts_once() {
    for (label, action) in [
        ("maintain-operator-twice-retry", MaintainAction::Retry),
        ("maintain-operator-twice-put-back", MaintainAction::PutBack),
    ] {
        let rig = host(label);
        halted(&rig, &[0, 1], "apply linux: exit 1");
        let daemon = rig.daemon();
        assert_eq!(daemon.order(&acting(action, 2)), done(false), "{label}");
        let ran = launched(&rig);
        let held = record(&rig);
        assert_eq!(daemon.order(&acting(action, 2)), done(true), "{label}");
        assert_eq!(launched(&rig), ran, "{label}");
        assert_eq!(record(&rig), held, "{label}");
    }
}

/// Catches a retry that fails again being retried by its own resend: the second delivery of the
/// `RETRY` try 2 that halted again answers that halt (`resent`) and launches nothing.
#[test]
fn a_resent_retry_that_halted_again_answers_the_halt() {
    let rig = host("maintain-operator-twice-halted");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    rig.launcher.additive_exits(HOOK, &[2]);
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&retry(2)), halt("apply linux: exit 2", false));
    let ran = launched(&rig);
    assert_eq!(daemon.order(&retry(2)), halt("apply linux: exit 2", true));
    assert_eq!(launched(&rig), ran);
}

// Refused.

/// Catches a retry or a put back taken where no halted turn of its order stands: with no record;
/// with the record of a turn that ended done; with the record of another order; with the record
/// of another run under the same order id; each is refused `not_halted`, nothing is launched and
/// the record is left as it was.
#[test]
fn a_retry_or_a_put_back_of_a_turn_that_is_not_halted_is_refused() {
    let rig = host("maintain-operator-not-halted");
    let daemon = rig.daemon();
    for order in [retry(2), put_back(2)] {
        assert_eq!(
            daemon.order(&order),
            refused(
                Refusal::NotHalted,
                "no maintenance turn is held on this host"
            )
        );
    }
    assert_eq!(launched(&rig), Vec::<String>::new());
    assert_eq!(record(&rig), None);

    assert_eq!(daemon.order(&maintain(&LINUX)), done(false));
    let ran = launched(&rig);
    let held = record(&rig);
    let mut other_run = terms(&LINUX, None);
    other_run.run_id = "run-2".to_owned();
    other_run.action = MaintainAction::Retry.into();
    let mut other_order = terms(&LINUX, None);
    other_order.action = MaintainAction::PutBack.into();
    let cases = [
        (
            retry(2),
            format!(
                "the maintenance turn of order {ORDER} (run {RUN}) stands at done, and is not halted"
            ),
        ),
        (
            put_back(2),
            format!(
                "the maintenance turn of order {ORDER} (run {RUN}) stands at done, and is not halted"
            ),
        ),
        (
            order_of("order-2", 2, other_order),
            format!(
                "the maintenance record holds order {ORDER} of run {RUN}, not order order-2 of run {RUN}"
            ),
        ),
        (
            order_of(ORDER, 2, other_run),
            format!(
                "the maintenance record holds order {ORDER} of run {RUN}, not order {ORDER} of run run-2"
            ),
        ),
    ];
    for (order, detail) in cases {
        assert_eq!(
            daemon.order(&order),
            refused(Refusal::NotHalted, &detail),
            "{detail}"
        );
    }
    assert_eq!(launched(&rig), ran);
    assert_eq!(record(&rig), held);
}

/// Catches a retry taken while the turn waits for its reboot: the `cutover` asked for a reboot
/// and the turn waits, so `RETRY` try 2 is refused `not_halted` naming the wait, and nothing runs.
#[test]
fn a_retry_of_a_turn_that_waits_for_its_reboot_is_refused() {
    let rig = host("maintain-operator-not-halted-waiting");
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&maintain(&LINUX)), Reply::Running);
    let ran = launched(&rig);
    let held = record(&rig);
    assert_eq!(
        daemon.order(&retry(2)),
        refused(
            Refusal::NotHalted,
            &format!(
                "the maintenance turn of order {ORDER} (run {RUN}) stands at wait for the reboot \
                 of linux, and is not halted"
            )
        )
    );
    assert_eq!(launched(&rig), ran);
    assert_eq!(record(&rig), held);
}

/// Catches an action under an older try than the record's taken as new: after `RETRY` try 3
/// halted again, a `PUT_BACK` try 2 is refused `stale_try`, and nothing runs.
#[test]
fn an_action_under_an_older_try_is_refused_stale() {
    let rig = host("maintain-operator-stale");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    rig.launcher.additive_exits(HOOK, &[1]);
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&retry(3)), halt("apply linux: exit 1", false));
    let ran = launched(&rig);
    assert_eq!(
        daemon.order(&put_back(2)),
        refused(
            Refusal::StaleTry,
            &format!("order {ORDER} try 2 is older than try 3, which the maintenance record holds")
        )
    );
    assert_eq!(launched(&rig), ran);
}

/// Catches a retry under the try of the halted order taken as a retry: `RETRY` try 1 on the turn
/// that halted under try 1 is a resend of that order, so it answers the halt (`resent`) and
/// launches nothing.
#[test]
fn an_action_under_the_records_own_try_is_a_resend() {
    let rig = host("maintain-operator-same-try");
    halted(&rig, &[0, 1], "apply linux: exit 1");
    let ran = launched(&rig);
    assert_eq!(
        rig.daemon().order(&retry(1)),
        halt("apply linux: exit 1", true)
    );
    assert_eq!(launched(&rig), ran);
}

/// Catches a retry that runs a step before the record says so: the first write of the lifted halt
/// fails, so the retry is refused `store_failed`, nothing is launched, and the record still holds
/// the halt under try 1.
#[test]
fn a_retry_whose_record_cannot_be_written_runs_nothing() {
    let rig = host("maintain-operator-store-failed");
    let held = halted(&rig, &[0, 1], "apply linux: exit 1");
    let ran = launched(&rig);
    let gate: CrashGate = Arc::new(|point| point == CrashPoint::BeforeWrite(1));
    let reply = rig
        .daemon()
        .with_maintenance_crash_gate(gate)
        .order(&retry(2));
    assert_eq!(
        reply,
        refused(Refusal::StoreFailed, "stopped at BeforeWrite(1)")
    );
    assert_eq!(launched(&rig), ran);
    assert_eq!(record(&rig), Some(held));
}

/// Catches the refusal of a turn that is not halted reported under another token than
/// `not_halted`, the one the coordinator reads.
#[test]
fn the_refusal_of_a_turn_that_is_not_halted_is_reported_not_halted() {
    assert_eq!(Refusal::NotHalted.token(), "not_halted");
}
