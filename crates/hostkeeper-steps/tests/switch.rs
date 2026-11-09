//! The daemon's switch around the hook's steps (`StepRunner::with_switch`): `current` and the
//! placed links to the target before every run of `cutover`, back before every run of
//! `rollback`, confirmed once after a `cleanup` that succeeded; a switch that fails runs nothing;
//! and the `rollback` of a first install that declines with `no_previous_release`.

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{RecordStore, State};
use hostkeeper_steps::runner::REASON_NO_PREVIOUS_RELEASE;
use hostkeeper_steps::{Finished, Loaded, RunError, StepRunner, Switch, Toward, UpdateOrder};
use support::{FAIL, IRREVERSIBLE_OK, OK, PENDING, Reply, Rig};

/// The last line of a `rollback` that declines a first install.
const DECLINED: Reply = Reply::Exit(1, Some(r#"{"reason":"no_previous_release"}"#));
/// The same, after a step committed.
const DECLINED_IRREVERSIBLE: Reply = Reply::Exit(
    1,
    Some(r#"{"reason":"no_previous_release","reversible":false}"#),
);

type Lines = Arc<Mutex<Vec<String>>>;

fn lines(log: &Lines) -> Vec<String> {
    log.lock().expect("the log").clone()
}

/// A runner of `rig` whose switch writes `switch <toward> <target>` to `log`, and fails while
/// `failing` is set; every run of a step writes `run <step>` to the same log.
fn switching(rig: &Rig, log: &Lines, failing: &Arc<AtomicBool>) -> StepRunner {
    let (seen, fail) = (Arc::clone(log), Arc::clone(failing));
    let switch: Switch = Arc::new(move |toward: Toward, order: &UpdateOrder| {
        seen.lock()
            .expect("the log")
            .push(format!("switch {toward:?} {}", order.target_version));
        match fail.load(Ordering::SeqCst) {
            true => Err("the link could not be made".to_owned().into()),
            false => Ok(()),
        }
    });
    let ran = Arc::clone(log);
    rig.fake.on_run(move |call| {
        ran.lock()
            .expect("the log")
            .push(format!("run {}", call.invocation.step));
    });
    StepRunner::new(rig.fake.clone(), rig.clock.clone(), rig.store()).with_switch(switch)
}

fn fin(verdict: Verdict, reason: Option<&str>) -> Finished {
    Finished {
        verdict,
        reason: reason.map(str::to_owned),
    }
}

/// Catches a switch made at the wrong moment, or not made again for a step run again: it comes
/// immediately before `cutover`, and immediately before each run of `rollback`, never around
/// `prepare`, `verify` or `cleanup`.
#[test]
fn the_switch_comes_immediately_before_cutover_and_each_rollback() {
    let rig = Rig::new("switch-order");
    let log = Lines::default();
    let failing = Arc::new(AtomicBool::new(false));
    let runner = switching(&rig, &log, &failing);
    let ok = runner.run(&rig.order()).expect("runs");
    assert_eq!(
        ok,
        fin(
            Verdict::Succeeded {
                promotion_pending: false
            },
            None
        )
    );
    assert_eq!(
        lines(&log),
        [
            "run prepare",
            "switch Target 2.0.0",
            "run cutover",
            "run verify",
            "run cleanup",
            "switch Confirmed 2.0.0"
        ]
    );

    let rig = Rig::new("switch-rollback");
    let log = Lines::default();
    let runner = switching(&rig, &log, &failing);
    rig.fake.say(Step::Verify, &[FAIL]);
    rig.fake.say(Step::Rollback, &[Reply::Exit(75, None), OK]);
    let failed = runner.run(&rig.order()).expect("runs");
    assert_eq!(
        failed.verdict,
        Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release
        }
    );
    assert_eq!(
        lines(&log),
        [
            "run prepare",
            "switch Target 2.0.0",
            "run cutover",
            "run verify",
            "switch Back 2.0.0",
            "run rollback",
            "switch Back 2.0.0",
            "run rollback"
        ]
    );
}

/// Catches a step run on a host the daemon could not switch, or a failed switch that loses the
/// update: nothing of `cutover` runs, the runner stops with `RunError::Switch`, the record holds
/// `cutover` pending, and the next run switches again and carries on from there.
#[test]
fn a_failed_switch_runs_nothing_and_leaves_the_step_pending() {
    let rig = Rig::new("switch-fails");
    let log = Lines::default();
    let failing = Arc::new(AtomicBool::new(true));
    let runner = switching(&rig, &log, &failing);
    let order = rig.order();
    let error = runner.run(&order).expect_err("the switch fails");
    assert!(
        matches!(&error, RunError::Switch(why) if why == "the link could not be made"),
        "{error:?}"
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare]);
    let Ok(Loaded::Current(rec)) = rig.store().load() else {
        panic!("the record reads");
    };
    assert_eq!((rec.step, rec.state), (Step::Cutover, State::Pending));

    failing.store(false, Ordering::SeqCst);
    let ok = runner.run(&order).expect("resumes");
    assert_eq!(
        ok.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(
        rig.fake.steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(
        lines(&log),
        [
            "run prepare",
            "switch Target 2.0.0",
            "switch Target 2.0.0",
            "run cutover",
            "run verify",
            "run cleanup",
            "switch Confirmed 2.0.0"
        ]
    );
}

/// Catches a first install whose `rollback` declines being held as a halt (no order of the
/// service would then run again), and the decline taken for anything but that: on a first
/// install (no previous release), a `rollback` that fails with the reason
/// `no_previous_release` ends `Failed` with that reason, in the phase its commit decides; the
/// same `rollback` of an update with a previous release, and a first-install `rollback` that
/// fails for another reason, are still a halt.
#[test]
fn a_first_install_whose_rollback_declines_ends_failed_not_halted() {
    let first_install = |name: &str| {
        let rig = Rig::new(name);
        let mut order = rig.order();
        order.previous = None;
        order.previous_version = None;
        (rig, order)
    };

    let (rig, order) = first_install("decline-before");
    rig.fake.say(Step::Cutover, &[FAIL]);
    rig.fake.say(Step::Rollback, &[DECLINED]);
    assert_eq!(
        rig.runner().run(&order).expect("runs"),
        fin(
            Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release
            },
            Some(REASON_NO_PREVIOUS_RELEASE)
        )
    );

    let (rig, order) = first_install("decline-after");
    rig.fake.say(Step::Cutover, &[IRREVERSIBLE_OK]);
    rig.fake.say(Step::Verify, &[FAIL]);
    rig.fake.say(Step::Rollback, &[DECLINED_IRREVERSIBLE]);
    assert_eq!(
        rig.runner().run(&order).expect("runs"),
        fin(
            Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release
            },
            Some(REASON_NO_PREVIOUS_RELEASE)
        )
    );

    let (rig, order) = first_install("decline-other-reason");
    rig.fake.say(Step::Cutover, &[FAIL]);
    rig.fake.say(
        Step::Rollback,
        &[Reply::Exit(1, Some(r#"{"reason":"disk_full"}"#))],
    );
    assert_eq!(
        rig.runner().run(&order).expect("runs").verdict,
        Verdict::Halt
    );

    let rig = Rig::new("decline-with-previous");
    rig.fake.say(Step::Cutover, &[FAIL]);
    rig.fake.say(Step::Rollback, &[DECLINED]);
    assert_eq!(
        rig.runner().run(&rig.order()).expect("runs").verdict,
        Verdict::Halt
    );
}

/// Catches the `uninstall` step given a bound in an update: it is no step of one, and the
/// bounds of an update have none for it.
#[test]
fn an_update_has_no_bound_for_the_uninstall_step() {
    let bounds = Rig::bounds();
    assert_eq!(bounds.timeout(Step::Uninstall), 0);
    assert_eq!(bounds.timeout(Step::Cutover), 300);
}

/// A runner of `rig` whose switch writes `switch <toward>` to `log`; on `Confirmed` it also reads
/// the record from the state directory and writes `record <step> <done|other>`; it fails
/// `Confirmed` while `failing` is set.
fn confirming(rig: &Rig, log: &Lines, failing: &Arc<AtomicBool>) -> StepRunner {
    let (seen, fail) = (Arc::clone(log), Arc::clone(failing));
    let state = rig.state.clone();
    let switch: Switch = Arc::new(move |toward: Toward, _: &UpdateOrder| {
        let mut seen = seen.lock().expect("the log");
        seen.push(format!("switch {toward:?}"));
        if toward != Toward::Confirmed {
            return Ok(());
        }
        let Ok(Loaded::Current(rec)) = RecordStore::new(&state).load() else {
            panic!("the record reads");
        };
        let kind = match rec.state {
            State::Done { .. } => "done",
            _ => "other",
        };
        seen.push(format!("record {} {kind}", rec.step));
        match fail.load(Ordering::SeqCst) {
            true => Err("the paths could not be committed".to_owned().into()),
            false => Ok(()),
        }
    });
    let ran = Arc::clone(log);
    rig.fake.on_run(move |call| {
        ran.lock()
            .expect("the log")
            .push(format!("run {}", call.invocation.step));
    });
    StepRunner::new(rig.fake.clone(), rig.clock.clone(), rig.store()).with_switch(switch)
}

/// Proves the confirmation comes after `cutover` and `cleanup` and before the verdict: `Target`
/// precedes `cutover`, `Confirmed` follows `cleanup`, and the record read inside `Confirmed` is
/// the `cleanup` step done, not finished.
#[test]
fn the_switch_is_confirmed_after_cleanup_succeeds_and_before_the_verdict() {
    let rig = Rig::new("confirm-order");
    let log = Lines::default();
    let runner = confirming(&rig, &log, &Arc::new(AtomicBool::new(false)));
    let order = rig.order();
    let ok = runner.run(&order).expect("runs");
    assert_eq!(
        ok,
        fin(
            Verdict::Succeeded {
                promotion_pending: false
            },
            None
        )
    );
    assert_eq!(
        lines(&log),
        [
            "run prepare",
            "switch Target",
            "run cutover",
            "run verify",
            "run cleanup",
            "switch Confirmed",
            "record cleanup done"
        ]
    );
}

/// Proves an update that did not succeed is never confirmed: a `cutover` that fails, a `verify`
/// that fails, and a `cleanup` that fails (after a commit, so it is not rolled back) end without
/// a `Confirmed` call; the first two switch back.
#[test]
fn a_failed_update_is_never_confirmed() {
    let cases: [(&str, Step, &[Reply]); 3] = [
        ("never-cutover", Step::Cutover, &[FAIL]),
        ("never-verify", Step::Verify, &[FAIL]),
        ("never-cleanup", Step::Cleanup, &[FAIL; 4]),
    ];
    for (name, step, replies) in cases {
        let rig = Rig::new(name);
        let log = Lines::default();
        let runner = confirming(&rig, &log, &Arc::new(AtomicBool::new(false)));
        if step == Step::Cleanup {
            rig.fake.say(Step::Cutover, &[IRREVERSIBLE_OK]);
        }
        rig.fake.say(step, replies);
        let finished = runner.run(&rig.order()).expect("runs");
        assert!(
            matches!(finished.verdict, Verdict::Failed { .. }),
            "{name}: {finished:?}"
        );
        let seen = lines(&log);
        assert!(
            !seen.iter().any(|line| line.contains("Confirmed")),
            "{name}: {seen:?}"
        );
        assert_eq!(
            seen.iter().any(|line| line == "switch Back"),
            step != Step::Cleanup,
            "{name}: {seen:?}"
        );
    }
}

/// Proves a confirmation that fails stops the runner with `RunError::Switch` and leaves the
/// record at `cleanup` done (the verdict unwritten, no step run again), and that a resume calls
/// it again and then finishes with the verdict.
#[test]
fn a_confirmation_that_fails_stops_the_runner_and_a_resume_confirms_again() {
    let rig = Rig::new("confirm-fails");
    let log = Lines::default();
    let failing = Arc::new(AtomicBool::new(true));
    let runner = confirming(&rig, &log, &failing);
    let order = rig.order();
    let error = runner.run(&order).expect_err("the confirmation fails");
    assert!(
        matches!(&error, RunError::Switch(why) if why == "the paths could not be committed"),
        "{error:?}"
    );
    let Ok(Loaded::Current(rec)) = rig.store().load() else {
        panic!("the record reads");
    };
    assert_eq!(rec.step, Step::Cleanup);
    assert!(matches!(rec.state, State::Done { .. }), "{:?}", rec.state);

    failing.store(false, Ordering::SeqCst);
    let ok = runner.run(&order).expect("resumes");
    assert_eq!(
        ok.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(
        rig.fake.steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    let confirmed = lines(&log)
        .iter()
        .filter(|line| *line == "switch Confirmed")
        .count();
    assert_eq!(confirmed, 2);
}

/// Proves that with a promotion pending the confirmation is called once, after `cleanup` and
/// before the watcher's first `verify`.
#[test]
fn a_pending_promotion_is_confirmed_once_before_the_watcher() {
    let rig = Rig::new("confirm-watcher");
    let log = Lines::default();
    let runner = confirming(&rig, &log, &Arc::new(AtomicBool::new(false)));
    rig.fake.say(Step::Verify, &[PENDING, PENDING, OK]);
    let ok = runner.run(&rig.order()).expect("runs");
    assert_eq!(
        ok.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    let seen = lines(&log);
    assert_eq!(
        seen.iter()
            .filter(|line| *line == "switch Confirmed")
            .count(),
        1,
        "{seen:?}"
    );
    assert_eq!(
        seen,
        [
            "run prepare",
            "switch Target",
            "run cutover",
            "run verify",
            "run cleanup",
            "switch Confirmed",
            "record cleanup done",
            "run verify",
            "run verify"
        ]
    );
}
