//! `run_maintenance_step` against a launcher that plays a script: the bound a `drain` or a
//! `resume` is launched with, how each end of the step is read, that nothing but the one step is
//! launched, and that a `LaunchError` is answered as it is.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_steps::{
    AdditiveCall, Bound, LAST_LINE_MAX, LaunchError, MaintenanceCall, MaintenanceExit,
    MaintenanceOutcome, MaintenanceStep, RESUME_BOUND_SECS, StepCall, StepExit, StepLauncher,
    run_maintenance_step,
};
use support::{FakeClock, secs};

/// A year, in seconds.
const YEAR: u64 = 365 * 24 * 3600;

/// What the launcher was asked in one additive run.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    service: String,
    step: String,
    hook_args: Vec<String>,
    params: PathBuf,
    executable: PathBuf,
    unit: String,
    bound: Bound,
}

/// A launcher that records additive runs, moves the shared clock by a scripted number of
/// seconds during each and answers a scripted result. `run` is never to be called.
struct Scripted {
    clock: Arc<FakeClock>,
    advance: u64,
    answer: Mutex<Option<Result<StepExit, LaunchError>>>,
    seen: Mutex<Vec<Seen>>,
    stops: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(
        clock: &Arc<FakeClock>,
        advance: u64,
        answer: Result<StepExit, LaunchError>,
    ) -> Scripted {
        Scripted {
            clock: clock.clone(),
            advance,
            answer: Mutex::new(Some(answer)),
            seen: Mutex::new(Vec::new()),
            stops: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn stops(&self) -> Vec<String> {
        self.stops.lock().unwrap().clone()
    }
}

impl StepLauncher for Scripted {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        panic!("a maintenance step is an additive step: `run` is never called");
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        self.stops.lock().unwrap().push(unit.to_owned());
        Ok(())
    }

    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        self.seen.lock().unwrap().push(Seen {
            service: call.service.to_owned(),
            step: call.step.to_owned(),
            hook_args: call.hook_args.to_vec(),
            params: call.params.to_path_buf(),
            executable: call.executable.to_path_buf(),
            unit: call.unit.to_owned(),
            bound: call.bound,
        });
        self.clock.advance(self.advance);
        self.answer
            .lock()
            .unwrap()
            .take()
            .expect("one launch per script")
    }
}

fn exit(code: Option<i32>) -> Result<StepExit, LaunchError> {
    Ok(StepExit {
        code,
        last_line: None,
    })
}

/// Runs `step` of service `svc` (by the word `drain`, in one unit whichever the step: the
/// launcher here does not read them) with `bound` against a launcher that spends `advance`
/// seconds and answers `answer`; returns the launcher with the result.
fn run(
    step: MaintenanceStep,
    bound: Bound,
    advance: u64,
    answer: Result<StepExit, LaunchError>,
) -> (Scripted, Result<MaintenanceExit, LaunchError>) {
    let clock = FakeClock::new();
    let launcher = Scripted::new(&clock, advance, answer);
    let got = run_with(&launcher, &clock, step, "drain", bound);
    (launcher, got)
}

fn run_with(
    launcher: &Scripted,
    clock: &Arc<FakeClock>,
    step: MaintenanceStep,
    word: &str,
    bound: Bound,
) -> Result<MaintenanceExit, LaunchError> {
    let args = vec!["update-hook".to_owned()];
    run_maintenance_step(
        launcher,
        clock.as_ref(),
        &MaintenanceCall {
            service: "svc",
            step,
            word,
            hook_args: &args,
            params: Path::new("/state/svc/params.toml"),
            executable: Path::new("/packages/svc/1.0.0/hook"),
            unit: "updater-svc-drain-3",
            bound,
        },
    )
}

fn outcome(got: Result<MaintenanceExit, LaunchError>) -> (MaintenanceOutcome, bool) {
    let got = got.unwrap();
    (got.outcome, got.deadline_passed)
}

const DRAIN: MaintenanceStep = MaintenanceStep::Drain;
const RESUME: MaintenanceStep = MaintenanceStep::Resume;

/// Catches a `drain` with no deadline bounded by the host or ended as deferred by time: it is
/// launched `Bound::Unbounded` however long it runs (a year on the clock), exit 0 is succeeded,
/// and exit 1 after a year is failed, not deferred.
#[test]
fn a_drain_with_no_deadline_is_unbounded_and_never_deferred_by_time() {
    assert_eq!(DRAIN.default_bound(), Bound::Unbounded);
    let (launcher, got) = run(DRAIN, DRAIN.default_bound(), YEAR, exit(Some(0)));
    assert_eq!(launcher.seen()[0].bound, Bound::Unbounded);
    assert_eq!(outcome(got), (MaintenanceOutcome::Succeeded, false));

    let (launcher, got) = run(DRAIN, DRAIN.default_bound(), YEAR, exit(Some(1)));
    assert_eq!(launcher.seen()[0].bound, Bound::Unbounded);
    assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false));

    let (_, got) = run(DRAIN, DRAIN.default_bound(), YEAR, exit(None));
    assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false));
}

/// Catches a drain deadline altered on its way to the launcher, or exit 75 not read as deferred:
/// a deadline of 1800 seconds is launched as exactly that bound, and exit 75 is deferred.
#[test]
fn a_drain_with_a_deadline_is_launched_with_exactly_it_and_exit_75_is_deferred() {
    let (launcher, got) = run(DRAIN, secs(1800), 10, exit(Some(75)));
    assert_eq!(launcher.seen()[0].bound, secs(1800));
    let got = got.unwrap();
    assert_eq!(got.outcome, MaintenanceOutcome::Deferred);
    assert_eq!(got.exit.code, Some(75));
    assert!(!got.deadline_passed);
}

/// Catches a drain that the launcher ended at its deadline read as failed: at exactly 1800
/// seconds, with no code or with code 1 (`systemd-run`'s code for a `RuntimeMaxSec=` end), it
/// is deferred and the deadline is marked passed.
#[test]
fn a_drain_ended_at_its_deadline_is_deferred() {
    for code in [None, Some(1)] {
        let (_, got) = run(DRAIN, secs(1800), 1800, exit(code));
        let got = got.unwrap();
        assert_eq!(got.outcome, MaintenanceOutcome::Deferred, "{code:?}");
        assert!(got.deadline_passed, "{code:?}");
        assert_eq!(got.exit.code, code);
    }
}

/// Catches a halt, or a code that is not the launcher's, read as a deadline end because it came
/// after the deadline: exit 76 and exit 2 at 1800 seconds of 1800 are failed, with the deadline
/// not marked passed.
#[test]
fn a_drain_halting_or_failing_on_its_own_after_its_deadline_is_failed() {
    for code in [76, 2] {
        let (_, got) = run(DRAIN, secs(1800), 1800, exit(Some(code)));
        assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false), "{code}");
    }
}

/// Catches a failure before the deadline read as a deadline end, and a success after it read as
/// one: exit 1 after 10 seconds of 1800 is failed, exit 0 after the deadline is succeeded.
#[test]
fn a_drain_failing_before_its_deadline_is_failed_and_one_succeeding_after_is_succeeded() {
    let (_, got) = run(DRAIN, secs(1800), 10, exit(Some(1)));
    assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false));
    let (_, got) = run(DRAIN, secs(1800), 1801, exit(Some(0)));
    assert_eq!(outcome(got), (MaintenanceOutcome::Succeeded, false));
}

/// Catches a failed step followed by a rollback or any other launch: exit 1, exit 76 and no
/// code are each failed, the launcher saw exactly the one drain, `run` was not called (it
/// panics) and nothing was stopped.
#[test]
fn a_failed_step_launches_nothing_else() {
    for code in [Some(1), Some(76), None] {
        let (launcher, got) = run(DRAIN, DRAIN.default_bound(), 3, exit(code));
        let got = got.unwrap();
        assert_eq!(got.outcome, MaintenanceOutcome::Failed, "{code:?}");
        assert_eq!(got.exit.code, code);
        assert_eq!(launcher.seen().len(), 1, "{code:?}");
        assert_eq!(launcher.seen()[0].step, "drain");
        assert!(launcher.stops().is_empty());
    }
}

/// Catches a `resume` unbounded or bounded other than ten minutes, or one killed at its bound
/// read as deferred: the default is `Bound::Secs(RESUME_BOUND_SECS)` of 600, a `resume` ended at
/// 600 with no code is failed (not deferred), exit 75 is deferred and exit 0 succeeded.
#[test]
fn a_resume_is_bounded_by_default_and_failed_when_its_bound_ends_it() {
    assert_eq!(RESUME_BOUND_SECS.get(), 600);
    assert_eq!(RESUME.default_bound(), Bound::Secs(RESUME_BOUND_SECS));
    assert_eq!(RESUME.default_bound(), secs(600));

    let (launcher, got) = run(RESUME, RESUME.default_bound(), 600, exit(None));
    assert_eq!(launcher.seen()[0].bound, secs(600));
    assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false));
    let (_, got) = run(RESUME, RESUME.default_bound(), 600, exit(Some(1)));
    assert_eq!(outcome(got), (MaintenanceOutcome::Failed, false));
    let (_, got) = run(RESUME, RESUME.default_bound(), 5, exit(Some(75)));
    assert_eq!(outcome(got), (MaintenanceOutcome::Deferred, false));
    let (_, got) = run(RESUME, RESUME.default_bound(), 5, exit(Some(0)));
    assert_eq!(outcome(got), (MaintenanceOutcome::Succeeded, false));
}

/// Catches the call changed on its way to the launcher: the service, the manifest's word
/// (`drain-now`, not the step's own name), the hook's arguments, the params file, the
/// executable, the unit and the bound arrive exactly as given.
#[test]
fn the_call_reaches_the_launcher_as_given() {
    let clock = FakeClock::new();
    let launcher = Scripted::new(&clock, 0, exit(Some(0)));
    run_with(&launcher, &clock, DRAIN, "drain-now", secs(90)).unwrap();
    assert_eq!(
        launcher.seen(),
        [Seen {
            service: "svc".to_owned(),
            step: "drain-now".to_owned(),
            hook_args: vec!["update-hook".to_owned()],
            params: PathBuf::from("/state/svc/params.toml"),
            executable: PathBuf::from("/packages/svc/1.0.0/hook"),
            unit: "updater-svc-drain-3".to_owned(),
            bound: secs(90),
        }]
    );
}

/// Catches a last line kept past the limit or dropped at it: one of exactly `LAST_LINE_MAX`
/// bytes is kept, one of one byte more is dropped, and the outcome follows the code either way.
#[test]
fn a_last_line_over_the_limit_is_dropped() {
    let with_line = |len: usize, code: i32| {
        Ok(StepExit {
            code: Some(code),
            last_line: Some("a".repeat(len)),
        })
    };
    let unbounded = DRAIN.default_bound();
    let (_, got) = run(DRAIN, unbounded, 1, with_line(LAST_LINE_MAX, 75));
    let got = got.unwrap();
    assert_eq!(got.outcome, MaintenanceOutcome::Deferred);
    assert_eq!(got.exit.last_line, Some("a".repeat(LAST_LINE_MAX)));

    let (_, got) = run(DRAIN, unbounded, 1, with_line(LAST_LINE_MAX + 1, 75));
    let got = got.unwrap();
    assert_eq!(got.outcome, MaintenanceOutcome::Deferred);
    assert_eq!(got.exit.last_line, None);

    let (_, got) = run(DRAIN, unbounded, 1, with_line(LAST_LINE_MAX + 1, 1));
    let got = got.unwrap();
    assert_eq!(got.outcome, MaintenanceOutcome::Failed);
    assert_eq!(got.exit.last_line, None);
}

/// Catches a launch error swallowed, turned into an outcome, or followed by a stop or another
/// launch: `NoStart` and `Launcher` errors are answered as they are, with one launch seen and
/// nothing stopped.
#[test]
fn a_launch_error_is_answered_as_the_error_and_nothing_else_runs() {
    let (launcher, got) = run(
        DRAIN,
        secs(60),
        0,
        Err(LaunchError::NoStart("no such file".to_owned())),
    );
    let Err(LaunchError::NoStart(why)) = got else {
        panic!("NoStart expected")
    };
    assert_eq!(why, "no such file");
    assert_eq!(launcher.seen().len(), 1);
    assert!(launcher.stops().is_empty());

    let (launcher, got) = run(
        RESUME,
        RESUME.default_bound(),
        0,
        Err(LaunchError::Launcher("the updater died".to_owned())),
    );
    let Err(LaunchError::Launcher(why)) = got else {
        panic!("Launcher expected")
    };
    assert_eq!(why, "the updater died");
    assert_eq!(launcher.seen().len(), 1);
    assert!(launcher.stops().is_empty());
}
