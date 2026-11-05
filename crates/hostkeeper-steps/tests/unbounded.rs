//! An unbounded `prepare` (`prepare = 0`): launched with no bound, never stopped by the dead-man,
//! and resumed after a crash by stopping its unit first. Every other step keeps its bound.

mod support;

use std::sync::Arc;

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::{Bound, Bounds, DeadMan, LaunchError, Loaded, RunError};
use support::{Event, Rig, secs};

/// A year, in seconds: far past any bound the rig declares.
const YEAR: u64 = 365 * 24 * 3600;

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
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
fn unbounded_order(rig: &Rig) -> hostkeeper_steps::UpdateOrder {
    let mut order = rig.order();
    order.bounds.prepare_secs = 0;
    order
}

/// Catches the unbounded form reaching a step other than `prepare`, or a 0 for `prepare` read as
/// "nothing left": 0 is unbounded for `prepare` alone, a step not started for every other step.
#[test]
fn only_prepare_can_be_unbounded() {
    let zero = Bounds {
        prepare_secs: 0,
        cutover_secs: 0,
        verify_secs: 0,
        rollback_secs: 0,
        cleanup_secs: 0,
        ..Rig::bounds()
    };
    assert_eq!(zero.bound(Step::Prepare), Some(Bound::Unbounded));
    for step in [Step::Cutover, Step::Verify, Step::Rollback, Step::Cleanup] {
        assert_eq!(zero.bound(step), None, "{step}");
    }
    let bounds = Rig::bounds();
    assert_eq!(bounds.bound(Step::Prepare), Some(secs(600)));
    assert_eq!(bounds.bound(Step::Cleanup), Some(secs(1300)));
}

/// Catches a runner that refuses `prepare = 0` (it ended the step as a failure before): the
/// update runs, `prepare` is run once and unbounded, recorded `running` with `unbounded` and its
/// start second as the deadline, and every later step keeps its own bound.
#[test]
fn the_runner_accepts_zero_for_prepare_and_launches_it_unbounded() {
    let rig = Rig::new("unb-accept");
    let record = rig.state.join(RECORD_FILE);
    rig.fake.on_run(move |call| {
        if call.invocation.step != Step::Prepare {
            return;
        }
        let rec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        let state = &rec["state"];
        assert_eq!(state["state"], "running");
        assert_eq!(state["unbounded"], true);
        assert_eq!(state["deadline_secs"], state["started"]["mono_secs"]);
    });
    let got = rig.runner().run(&unbounded_order(&rig)).unwrap();
    assert_eq!(got.verdict, succeeded());
    assert_eq!(
        bounds_run(&rig),
        [
            (Step::Prepare, Bound::Unbounded),
            (Step::Cutover, secs(300)),
            (Step::Verify, secs(1000)),
            (Step::Cleanup, secs(1300)),
        ]
    );
}

/// Catches a dead-man armed for an unbounded step: with the clock moved on by a year, then by a
/// year again, while `prepare` runs, no tick stops anything; `prepare` then exits `0` and the
/// update goes on to its end.
#[test]
fn an_unbounded_prepare_is_never_stopped_by_the_dead_man() {
    let rig = Rig::new("unb-deadman");
    let runner = rig.runner();
    let man = Arc::new(DeadMan::new(
        runner.watch(),
        rig.fake.clone(),
        rig.clock.clone(),
    ));
    let (clock, seen) = (rig.clock.clone(), man.clone());
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            for _ in 0..3 {
                clock.advance(YEAR);
                assert!(!seen.tick().unwrap());
            }
        }
    });
    let got = runner.run(&unbounded_order(&rig)).unwrap();
    assert_eq!(got.verdict, succeeded());
    assert_eq!(rig.fake.stops(), Vec::<String>::new());
    assert_eq!(
        rig.fake.steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
}

/// Catches a resume that reads the stored `deadline_secs` of an unbounded step as a deadline:
/// the updater dies while an unbounded `prepare` runs, and a year later the next run stops the
/// unit by name once, runs `prepare` again (its next attempt, under the same unit name, which
/// `--collect` frees) still unbounded and unstopped by the dead-man, and the update succeeds.
#[test]
fn a_crashed_unbounded_prepare_is_resumed_by_stopping_its_unit_first() {
    let rig = Rig::new("unb-resume");
    let order = unbounded_order(&rig);
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
    assert!(*unbounded);
    assert_eq!(*deadline_secs, started.mono_secs);
    assert_eq!((rec.step, rec.attempt), (Step::Prepare, 1));

    rig.clock.advance(YEAR);
    rig.fake.clear();
    let runner = rig.runner();
    let man = Arc::new(DeadMan::new(
        runner.watch(),
        rig.fake.clone(),
        rig.clock.clone(),
    ));
    let (clock, seen) = (rig.clock.clone(), man.clone());
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            clock.advance(YEAR);
            assert!(!seen.tick().unwrap());
        }
    });
    let got = runner.run(&order).unwrap();
    assert_eq!(got.verdict, succeeded());
    assert_eq!(rig.fake.stops(), ["updater-svc-prepare-1"]);
    let events = rig.fake.events();
    assert_eq!(events[0], Event::Stop("updater-svc-prepare-1".to_owned()));
    assert!(
        matches!(
            &events[1],
            Event::Run { step: Step::Prepare, unit, bound: Bound::Unbounded, .. }
                if unit == "updater-svc-prepare-1"
        ),
        "{:?}",
        events[1]
    );
}
