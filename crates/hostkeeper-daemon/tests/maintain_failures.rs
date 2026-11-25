//! The ways a `maintain` turn ends short of done: a stage that fails or defers leaves every service
//! serving, a drain deadline puts the drained services back, and any other failure from drain on
//! halts the turn with the host as the hooks left it (no `resume`, no `rollback`), and a halted
//! host refuses another turn while a deferred one does not.

mod support;
mod support_maintain;

use std::sync::Arc;

use hostkeeper_daemon::maintenance_state::run_state;
use hostkeeper_daemon::testing::AdditiveFault;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_steps::maintenance::{
    Kind, MAINTENANCE_DIR, MAINTENANCE_RECORD_FILE, MaintenanceRecord, RunStep,
};
use support::{Rig, name};
use support_maintain::{HOOK, ORDER, RUN, draining, launched, maintain, order_of, record, terms};

const LINUX: [MaintenanceKind; 1] = [MaintenanceKind::Linux];

/// A rig whose services `alpha` and `beta` both drain.
fn host(label: &str) -> Rig {
    let rig = Rig::new(label);
    draining(&rig, "alpha", "1.0.0");
    draining(&rig, "beta", "1.0.0");
    rig
}

/// The record of a new turn of a Linux run over `alpha` and `beta`, before any step.
fn fresh() -> MaintenanceRecord {
    MaintenanceRecord::new(
        RUN,
        ORDER,
        1,
        &[Kind::Linux],
        vec![name("alpha"), name("beta")],
    )
    .expect("a record")
}

/// The answer of a turn that ended with `verdict` and `reason`.
fn finished(verdict: Verdict, reason: &str) -> Reply {
    Reply::Finished {
        verdict,
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

fn deferred() -> Verdict {
    Verdict::Deferred {
        retry_after_secs: None,
    }
}

fn strings(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|step| (*step).to_owned()).collect()
}

/// Catches a stage that fails taking a service out of service: the hook's `prepare` ends with
/// exit 1 and its report, so the turn is deferred with the hook's reason and detail, nothing is
/// drained or resumed, and the record is done with the deferral set.
#[test]
fn a_stage_that_fails_defers_with_nothing_drained() {
    let rig = host("maintain-failures-stage");
    rig.launcher.additive_replies(
        HOOK,
        &[(
            1,
            r#"{"class":"release","reason":"disk_full","detail":"no space left"}"#,
        )],
    );
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "stage: exit 1, disk_full: no space left";
    assert_eq!(reply, finished(deferred(), why));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    let mut expected = fresh();
    expected.step = RunStep::Done;
    expected.deferred = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a stage that defers (exit 75) being read as a failure: it ends the turn deferred the
/// same way, with the exit in the reason, and no service is touched.
#[test]
fn a_stage_that_defers_defers_the_turn_with_nothing_drained() {
    let rig = host("maintain-failures-stage-75");
    rig.launcher.additive_exits(HOOK, &[75]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    assert_eq!(reply, finished(deferred(), "stage: exit 75"));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    let mut expected = fresh();
    expected.step = RunStep::Done;
    expected.deferred = Some("stage: exit 75".to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a drain whose deadline passes leaving a service drained: `beta`'s drain defers (exit
/// 75), so no cutover runs, both drained services are resumed in reverse (the one whose drain
/// deferred included), and the turn ends deferred with every service put back.
#[test]
fn a_drain_that_defers_puts_every_drained_service_back_and_defers() {
    let rig = host("maintain-failures-drain-75");
    rig.launcher.additive_exits("beta", &[75]);
    let daemon = rig.daemon();
    let order = order_of(ORDER, 1, terms(&LINUX, Some(60)));
    let reply = daemon.order(&order);
    let why = "drain beta: exit 75";
    assert_eq!(reply, finished(deferred(), why));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "beta resume",
            "alpha resume",
        ])
    );
    let mut expected = fresh();
    expected.step = RunStep::Done;
    expected.deferred = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a drain the launcher ended at its bound being read as a failure: `beta`'s drain is
/// killed with no exit code after the clock passed the deadline, so the turn defers with the
/// deadline in its reason, and both services are put back.
#[test]
fn a_drain_the_launcher_ended_at_its_deadline_defers() {
    let rig = host("maintain-failures-drain-deadline");
    rig.launcher.additive_fault("beta", AdditiveFault::Killed);
    let clock = Arc::clone(&rig.clock);
    rig.launcher.during_additive(move |call| {
        if call.step == "drain" && call.service == "beta" {
            clock.advance(60);
        }
    });
    let daemon = rig.daemon();
    let order = order_of(ORDER, 1, terms(&LINUX, Some(60)));
    let reply = daemon.order(&order);
    let why = "drain beta: the drain deadline passed: ended with no exit code";
    assert_eq!(reply, finished(deferred(), why));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "beta resume",
            "alpha resume",
        ])
    );
    let mut expected = fresh();
    expected.step = RunStep::Done;
    expected.deferred = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a drain that fails being put back or carried on past: `beta`'s drain exits 1 with no
/// deadline, so the turn halts, no cutover and no `resume` runs, and the record still holds both
/// drained services (the host stays as the hooks left it).
#[test]
fn a_drain_that_fails_halts_with_the_drained_services_left_drained() {
    let rig = host("maintain-failures-drain-1");
    rig.launcher.additive_exits("beta", &[1]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "drain beta: exit 1";
    assert_eq!(reply, finished(Verdict::Halt, why));
    assert_eq!(
        launched(&rig),
        strings(&["hostkeeper prepare", "alpha drain", "beta drain"])
    );
    let mut expected = fresh();
    expected.step = RunStep::Drain;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a failed apply being rolled back or the services being put back: the `cutover` of
/// `linux` exits 1, so the turn halts, the launches end at the cutover (no `resume`, no
/// `rollback`), and the reason names the kind and the exit.
#[test]
fn an_apply_that_fails_halts_without_resume_or_rollback() {
    let rig = host("maintain-failures-apply");
    rig.launcher.additive_exits(HOOK, &[0, 1]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "apply linux: exit 1";
    assert_eq!(reply, finished(Verdict::Halt, why));
    let ran = launched(&rig);
    assert_eq!(
        ran,
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
        ])
    );
    assert!(ran.iter().all(|step| !step.contains("resume")));
    assert!(ran.iter().all(|step| !step.contains("rollback")));
    let mut expected = fresh();
    expected.step = RunStep::Apply;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a failed check being answered by putting the services back: the hook's `verify` exits
/// 1 after the kind applied, so the turn halts with no `resume` launched and the kind recorded
/// applied.
#[test]
fn a_check_that_fails_halts_without_resume() {
    let rig = host("maintain-failures-check");
    rig.launcher.additive_exits(HOOK, &[0, 0, 1]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "check: exit 1";
    assert_eq!(reply, finished(Verdict::Halt, why));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
        ])
    );
    let mut expected = fresh();
    expected.step = RunStep::Check;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.kinds[0].applied = true;
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a failed `resume` being carried past: `beta`'s `resume` (the first of the reverse
/// order) exits 1, so the turn halts, `alpha`'s `resume` is not launched, and the record still
/// holds both services as drained.
#[test]
fn a_resume_that_fails_halts_before_the_next_resume() {
    let rig = host("maintain-failures-resume");
    rig.launcher.additive_exits("beta", &[0, 1]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "resume beta: exit 1";
    assert_eq!(reply, finished(Verdict::Halt, why));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
        ])
    );
    let mut expected = fresh();
    expected.step = RunStep::PutBack;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.kinds[0].applied = true;
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a hook that does not start being read as anything but a failed stage: the launcher
/// cannot start `prepare`, so the stage fails and the turn defers with the launcher's words, its
/// unit stopped once before the launch and once after, and nothing drained.
#[test]
fn a_launcher_that_cannot_start_the_stage_defers_the_turn() {
    let rig = host("maintain-failures-nostart");
    rig.launcher.additive_fault(HOOK, AdditiveFault::NoStart);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "stage: the launcher failed: the hook did not start: no such file";
    assert_eq!(reply, finished(deferred(), why));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    let stops = rig.launcher.stops();
    let unit = "updater-hostkeeper-prepare-1";
    assert_eq!(stops.iter().filter(|stop| *stop == unit).count(), 2);
    let mut expected = fresh();
    expected.step = RunStep::Done;
    expected.deferred = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a unit the launcher lost being left running: the `cutover` is lost, so the turn halts
/// with the launcher's words and the unit of that `cutover` is stopped (once before the launch,
/// once after the loss).
#[test]
fn a_launcher_that_loses_the_cutover_halts_and_stops_its_unit() {
    let rig = host("maintain-failures-lost");
    let launcher = Arc::clone(&rig.launcher);
    rig.launcher.during_additive(move |call| {
        if call.step == "cutover" {
            launcher.additive_fault(HOOK, AdditiveFault::Lost);
        }
    });
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    let why = "apply linux: the launcher failed: the launcher failed: updater-hostkeeper-cutover-1 was lost";
    assert_eq!(reply, finished(Verdict::Halt, why));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
        ])
    );
    let stops = rig.launcher.stops();
    let unit = "updater-hostkeeper-cutover-1";
    assert_eq!(stops.iter().filter(|stop| *stop == unit).count(), 2);
    let mut expected = fresh();
    expected.step = RunStep::Apply;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a halted host taking another turn: after a halt, an order with another id is refused
/// `busy` with the halted turn named, and nothing more is launched.
#[test]
fn a_halted_host_refuses_another_order() {
    let rig = host("maintain-failures-halted-busy");
    rig.launcher.additive_exits(HOOK, &[0, 1]);
    let daemon = rig.daemon();
    let first = daemon.order(&maintain(&LINUX));
    assert_eq!(first, finished(Verdict::Halt, "apply linux: exit 1"));
    let ran = launched(&rig);
    let held = record(&rig);
    let reply = daemon.order(&order_of("order-2", 1, terms(&LINUX, None)));
    assert_eq!(
        reply,
        Reply::Refused {
            refusal: Refusal::Busy,
            detail: "the maintenance turn of order order-1 (run run-1) stands at halted".to_owned(),
        }
    );
    assert_eq!(launched(&rig), ran);
    assert_eq!(record(&rig), held);
}

/// Catches a deferred turn holding the host: after a deferral, an order with another id starts a
/// new turn that runs every step to done, and the record is that turn's.
#[test]
fn a_deferred_host_takes_a_new_turn() {
    let rig = host("maintain-failures-deferred-new");
    rig.launcher.additive_exits(HOOK, &[75]);
    let daemon = rig.daemon();
    let first = daemon.order(&maintain(&LINUX));
    assert_eq!(first, finished(deferred(), "stage: exit 75"));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    let reply = daemon.order(&order_of("order-2", 1, terms(&LINUX, None)));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
            resent: false,
        }
    );
    assert_eq!(
        launched(&rig)[1..].to_vec(),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume",
        ])
    );
    let mut expected = MaintenanceRecord::new(
        RUN,
        "order-2",
        1,
        &[Kind::Linux],
        vec![name("alpha"), name("beta")],
    )
    .expect("a record");
    expected.step = RunStep::Done;
    expected.kinds[0].applied = true;
    assert_eq!(record(&rig), Some(expected));
}

/// The password of the update account in the macOS hook's own tests: the install program there
/// echoes it to both of its streams.
const PASSWORD: &str = "Up date=pa\"ss$HOME\\1";
/// The report line the macOS hook writes for a failed `softwareupdate --install` that echoed the
/// password (`a_failing_install_fails_naming_the_step_and_exit_code_alone`): the step and the exit
/// code alone.
const MACOS_INSTALL_REPORT: &str = r#"{"class":"release","detail":"softwareupdate --install: the program exited with code 1","promotion_pending":false,"reason":"macos_install_failed"}"#;

/// Catches the hook's report of a failed install (its reason and detail, from a program that
/// echoed the update account's password) reaching the run record other than as the step, the exit
/// and that reason and detail: the `macos` cutover exits 1 with the report line, so the turn halts,
/// the record's halted reason is exactly the words of the report, the run state says the same
/// words, and the record's bytes on disk hold no password.
#[test]
fn a_failed_macos_install_halts_the_record_with_the_reports_words_alone() {
    let rig = host("maintain-failures-macos-install");
    rig.launcher
        .additive_replies(HOOK, &[(0, ""), (1, MACOS_INSTALL_REPORT)]);
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&[MaintenanceKind::Macos]));
    let why = "apply macos: exit 1, macos_install_failed: softwareupdate --install: the program exited with code 1";
    assert_eq!(reply, finished(Verdict::Halt, why));
    let mut expected = MaintenanceRecord::new(
        RUN,
        ORDER,
        1,
        &[Kind::Macos],
        vec![name("alpha"), name("beta")],
    )
    .expect("a record");
    expected.step = RunStep::Apply;
    expected.drained = vec![name("alpha"), name("beta")];
    expected.halted = Some(why.to_owned());
    let stored = record(&rig).expect("a record");
    assert_eq!(stored, expected);
    assert_eq!(run_state(&stored).halted_reason, why);
    let bytes = std::fs::read(
        rig.state
            .parent()
            .expect("the state's parent")
            .join(MAINTENANCE_DIR)
            .join(MAINTENANCE_RECORD_FILE),
    )
    .expect("the record's file");
    assert!(
        !String::from_utf8_lossy(&bytes).contains(PASSWORD),
        "the password is in the record"
    );
}
