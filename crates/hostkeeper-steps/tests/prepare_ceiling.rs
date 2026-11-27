//! A `prepare` whose manifest bound is 0 (`prepare = 0`): launched under the updater's ceiling
//! ([`PREPARE_CEILING_SECS`]), stopped there by the dead-man, and read as a deferral with the
//! reason `prepare_ceiling`; resumed after a crash by stopping its unit first. Every other step
//! keeps its bound, and a `prepare` with a bound of its own keeps that one.

mod support;

use std::sync::Arc;

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::{
    Bound, Bounds, DeadMan, Finished, LaunchError, Loaded, PREPARE_CEILING_SECS,
    REASON_PREPARE_CEILING, RunError, StepRunner,
};
use support::{Event, FAIL, Reply, Rig, secs};

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
    }
}

/// The end of an update whose `prepare` reached the ceiling.
fn at_ceiling() -> Finished {
    Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_PREPARE_CEILING.to_owned()),
    }
}

/// The bound each step was run with, in order.
fn bounds_run(rig: &Rig) -> Vec<(Step, Bound)> {
    rig.fake
        .runs()
        .into_iter()
        .filter_map(|e| match e {
            Event::Run { step, bound, .. } => Some((step, bound)),
            Event::Stop(_) => None,
        })
        .collect()
}

/// The rig's order with `prepare = 0`.
fn ceiling_order(rig: &Rig) -> hostkeeper_steps::UpdateOrder {
    let mut order = rig.order();
    order.bounds.prepare_secs = 0;
    order
}

/// A dead-man over `runner`'s watch, the rig's launcher and its clock.
fn dead_man(rig: &Rig, runner: &StepRunner) -> Arc<DeadMan> {
    Arc::new(DeadMan::new(
        runner.watch(),
        rig.fake.clone(),
        rig.clock.clone(),
    ))
}

/// Makes the rig's first `prepare` a hook that never returns: while it runs, the clock moves to
/// one second before the ceiling, where the dead-man stops nothing, and then to the ceiling,
/// where it stops the step's unit; the step then ends as a stopped unit does, with no code.
fn never_returns(rig: &Rig, man: &Arc<DeadMan>) {
    let (clock, seen) = (rig.clock.clone(), man.clone());
    let first = std::sync::atomic::AtomicBool::new(true);
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare
            && first.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            clock.advance(PREPARE_CEILING_SECS - 1);
            assert!(!seen.tick().unwrap(), "one second before the ceiling");
            clock.advance(1);
            assert!(seen.tick().unwrap(), "at the ceiling");
        }
    });
    rig.fake.say(Step::Prepare, &[Reply::Killed]);
}

/// Catches a ceiling other than the hour the PR states, the unbounded form left for `prepare`, or
/// a 0 of another step read as the ceiling: 0 is the ceiling for `prepare` alone, a step not
/// started for every other step, and a bound of its own is kept.
#[test]
fn only_a_prepare_of_zero_takes_the_ceiling() {
    assert_eq!(PREPARE_CEILING_SECS, 3600);
    let zero = Bounds {
        prepare_secs: 0,
        cutover_secs: 0,
        verify_secs: 0,
        rollback_secs: 0,
        cleanup_secs: 0,
        ..Rig::bounds()
    };
    assert_eq!(zero.bound(Step::Prepare), Some(secs(PREPARE_CEILING_SECS)));
    for step in [Step::Cutover, Step::Verify, Step::Rollback, Step::Cleanup] {
        assert_eq!(zero.bound(step), None, "{step}");
    }
    let bounds = Rig::bounds();
    assert_eq!(bounds.bound(Step::Prepare), Some(secs(600)));
    assert_eq!(bounds.bound(Step::Cleanup), Some(secs(1300)));
}

/// Catches a `prepare = 0` launched with no bound (`RuntimeMaxSec=infinity`, the dead-man not
/// armed), or recorded as unbounded: it is run once under the ceiling, recorded `running` with
/// `unbounded` false and its start plus the ceiling as the deadline, and every later step keeps
/// its own bound.
#[test]
fn a_prepare_of_zero_is_launched_and_recorded_under_the_ceiling() {
    let rig = Rig::new("ceil-launch");
    let record = rig.state.join(RECORD_FILE);
    rig.fake.on_run(move |call| {
        if call.invocation.step != Step::Prepare {
            return;
        }
        let rec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        let state = &rec["state"];
        assert_eq!(state["state"], "running");
        assert_eq!(
            state["unbounded"],
            serde_json::Value::Null,
            "false is not written"
        );
        let started = state["started"]["mono_secs"].as_u64().unwrap();
        assert_eq!(state["deadline_secs"], started + PREPARE_CEILING_SECS);
    });
    let got = rig.runner().run(&ceiling_order(&rig)).unwrap();
    assert_eq!(got.verdict, succeeded());
    assert_eq!(
        bounds_run(&rig),
        [
            (Step::Prepare, secs(PREPARE_CEILING_SECS)),
            (Step::Cutover, secs(300)),
            (Step::Verify, secs(1000)),
            (Step::Cleanup, secs(1300)),
        ]
    );
}

/// Catches a hook whose `prepare = 0` sleeps for ever holding the update for ever, and one stopped
/// at the ceiling reported as a failure of the release: the dead-man stops it at the ceiling and
/// not a second before, `rollback` runs to undo what it did, and the update ends `Deferred` with
/// the reason `prepare_ceiling` and no wait of its own (the coordinator's back-off decides).
/// Nothing after `rollback` runs.
#[test]
fn a_prepare_that_never_returns_ends_deferred_at_the_ceiling() {
    let rig = Rig::new("ceil-forever");
    let runner = rig.runner();
    let man = dead_man(&rig, &runner);
    never_returns(&rig, &man);
    let got = runner.run(&ceiling_order(&rig)).unwrap();
    assert_eq!(got, at_ceiling());
    assert_eq!(rig.fake.stops(), ["updater-svc-prepare-1"]);
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);
}

/// Catches the ceiling's reading leaking to a `prepare` that ended by itself, or to one with a
/// bound of its own: a `prepare = 0` that fails (exit 1) before the ceiling is a failure; one that
/// is killed before it (no code, a second early) is a failure too; a `prepare` of 600 s stopped at
/// its own bound is a failure as before, not a deferral.
#[test]
fn only_a_prepare_of_zero_stopped_at_the_ceiling_reads_as_deferred() {
    let failure = Verdict::Failed {
        phase: hostkeeper_hook::Phase::BeforeCommit,
        class: hostkeeper_hook::Class::Release,
    };
    let rig = Rig::new("ceil-fails");
    rig.fake.say(Step::Prepare, &[FAIL]);
    let got = rig.runner().run(&ceiling_order(&rig)).unwrap();
    assert_eq!((got.verdict, got.reason), (failure.clone(), None));

    let rig = Rig::new("ceil-early-kill");
    let clock = rig.clock.clone();
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            clock.advance(PREPARE_CEILING_SECS - 1);
        }
    });
    rig.fake.say(Step::Prepare, &[Reply::Killed]);
    let got = rig.runner().run(&ceiling_order(&rig)).unwrap();
    assert_eq!((got.verdict, got.reason), (failure.clone(), None));

    let rig = Rig::new("ceil-own-bound");
    let clock = rig.clock.clone();
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            clock.advance(600);
        }
    });
    rig.fake.say(Step::Prepare, &[Reply::Killed]);
    let got = rig.runner().run(&rig.order()).unwrap();
    assert_eq!((got.verdict, got.reason), (failure, None));
}

/// Catches a crash under the ceiling that loses it: the updater dies while a `prepare = 0` runs,
/// and the next run stops the unit by name once, runs `prepare` again (its next attempt, under the
/// same unit name, which `--collect` frees) under the ceiling again, and, when that one never
/// returns either, ends deferred at the ceiling.
#[test]
fn a_crashed_prepare_is_resumed_under_the_ceiling() {
    let rig = Rig::new("ceil-resume");
    let order = ceiling_order(&rig);
    rig.fake
        .lose_run
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let err = rig.runner().run(&order).unwrap_err();
    assert!(
        matches!(&err, RunError::Launch(LaunchError::Launcher(why)) if why == "the updater died"),
        "{err:?}"
    );
    let Loaded::Current(rec) = rig.store().load().unwrap() else {
        panic!("the record is there")
    };
    let State::Running {
        started,
        deadline_secs,
        unbounded,
    } = &rec.state
    else {
        panic!("the step is left running: {:?}", rec.state)
    };
    assert!(!*unbounded);
    assert_eq!(*deadline_secs, started.mono_secs + PREPARE_CEILING_SECS);
    assert_eq!((rec.step, rec.attempt), (Step::Prepare, 1));

    rig.fake.clear();
    let runner = rig.runner();
    let man = dead_man(&rig, &runner);
    never_returns(&rig, &man);
    assert_eq!(runner.run(&order).unwrap(), at_ceiling());
    let events = rig.fake.events();
    assert_eq!(events[0], Event::Stop("updater-svc-prepare-1".to_owned()));
    assert!(
        matches!(
            &events[1],
            Event::Run { step: Step::Prepare, unit, bound, .. }
                if unit == "updater-svc-prepare-1" && *bound == secs(PREPARE_CEILING_SECS)
        ),
        "{:?}",
        events[1]
    );
    assert_eq!(
        rig.fake.stops(),
        ["updater-svc-prepare-1", "updater-svc-prepare-1"]
    );
}
