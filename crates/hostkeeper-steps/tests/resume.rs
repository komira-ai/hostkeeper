//! The resume table of the daemon's start (design section 6.5): every cell of it, the rewrite of
//! a record for each, the runner reaching each cell's verdict from the rewritten record, the
//! runner's hand-off of the post-success watcher, the record's write-fault seam, and the
//! launcher's default for an additive step.

mod support;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{HookSel, Progress, State};
use hostkeeper_steps::{
    AdditiveCall, Finished, LaunchError, Loaded, REASON_REBOOTED, Record, RecordError,
    ResumeAction, Stamp, StepLauncher, resume, resume_action,
};
use support::{DEFER, FAIL, IRREVERSIBLE_OK, OK, PENDING, Rig};

use ResumeAction::{
    ContinueVerifyFreshWindow, NothingFinished, RecomputeNext, RerunStep,
    RestartNewReleaseContinueNext, RollbackThenDeferred, RollbackThenFailed, StopUnitAndRerun,
    WatcherRestart,
};

fn stamp(boot_id: &str, mono_secs: u64) -> Stamp {
    Stamp {
        boot_id: boot_id.to_owned(),
        mono_secs,
    }
}

/// A record of `step` in `state`, committed or not, in the watcher or not, written in boot `a`.
fn record(step: Step, state: State, committed: bool, watching: bool) -> Record {
    Record {
        schema: 1,
        service: "svc".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "cd".repeat(32),
        tarball_digest: "ab".repeat(32),
        previous_digest: None,
        generation: 3,
        params: "/state/svc/params.toml".into(),
        step,
        attempt: 2,
        state,
        hook: HookSel::New,
        progress: Progress {
            committed,
            verify_started: Some(stamp("boot-a", 100)),
            watching: watching.then(|| stamp("boot-a", 200)),
            ..Progress::default()
        },
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

/// Every state a record can hold. A new state fails to compile here until it has its row.
fn states() -> Vec<State> {
    let all = vec![
        State::Pending,
        State::Running {
            started: stamp("boot-a", 300),
            deadline_secs: 900,
            unbounded: false,
        },
        State::Done {
            code: Some(0),
            last_line: None,
        },
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
        },
    ];
    for state in &all {
        match state {
            State::Pending
            | State::Running { .. }
            | State::Done { .. }
            | State::Finished { .. } => {}
        }
    }
    all
}

const DEFERRED: ResumeAction = RollbackThenDeferred {
    reason: REASON_REBOOTED,
};
const BEFORE_COMMIT: ResumeAction = RollbackThenFailed {
    phase: Phase::BeforeCommit,
};
const GATE_RELEASE: ResumeAction = RollbackThenFailed {
    phase: Phase::GateRelease,
};

/// The table for a step that is pending or running and not in the watcher: (step, committed,
/// rebooted, the action when pending, the action when running), one line per cell.
const LIVE: [(Step, bool, bool, ResumeAction, ResumeAction); 20] = [
    // | `prepare` pending or running | stop the unit by name, run `prepare` again | run
    // `rollback`, then report Deferred `rebooted` |
    (Step::Prepare, false, false, RerunStep, StopUnitAndRerun),
    (Step::Prepare, false, true, DEFERRED, DEFERRED),
    (Step::Prepare, true, false, RerunStep, StopUnitAndRerun),
    (Step::Prepare, true, true, DEFERRED, DEFERRED),
    // | `cutover` pending or running, nothing committed | stop the unit, run `cutover` again |
    // run `rollback` before any unit starts, then Failed `before_commit` |
    (Step::Cutover, false, false, RerunStep, StopUnitAndRerun),
    (Step::Cutover, false, true, BEFORE_COMMIT, BEFORE_COMMIT),
    // | `cutover` or later, a step has committed | run the step again | restart of the new
    // release; continue with the next step |
    (Step::Cutover, true, false, RerunStep, StopUnitAndRerun),
    (
        Step::Cutover,
        true,
        true,
        RestartNewReleaseContinueNext,
        RestartNewReleaseContinueNext,
    ),
    // | `verify` running | run `verify` again | nothing committed: `rollback`, Failed
    // `gate_release`; committed: continue `verify` with a fresh window |
    (Step::Verify, false, false, RerunStep, StopUnitAndRerun),
    (Step::Verify, false, true, GATE_RELEASE, GATE_RELEASE),
    (Step::Verify, true, false, RerunStep, StopUnitAndRerun),
    (
        Step::Verify,
        true,
        true,
        ContinueVerifyFreshWindow,
        ContinueVerifyFreshWindow,
    ),
    // | `rollback` or `cleanup` running | run it again | run it again |
    (Step::Rollback, false, false, RerunStep, StopUnitAndRerun),
    (Step::Rollback, false, true, RerunStep, RerunStep),
    (Step::Rollback, true, false, RerunStep, StopUnitAndRerun),
    (Step::Rollback, true, true, RerunStep, RerunStep),
    (Step::Cleanup, false, false, RerunStep, StopUnitAndRerun),
    (Step::Cleanup, false, true, RerunStep, RerunStep),
    (Step::Cleanup, true, false, RerunStep, StopUnitAndRerun),
    (Step::Cleanup, true, true, RerunStep, RerunStep),
];

/// The expected cell: the table above for a live step, `done` recomputes, a finished update has
/// nothing to resume, and the watcher restarts whatever its step.
fn expected(
    step: Step,
    state: &State,
    committed: bool,
    watching: bool,
    rebooted: bool,
) -> ResumeAction {
    match (state, watching) {
        (State::Finished { .. }, _) => return NothingFinished,
        (_, true) => return WatcherRestart,
        (State::Done { .. }, false) => return RecomputeNext,
        _ => {}
    }
    let row = LIVE
        .iter()
        .find(|row| (row.0, row.1, row.2) == (step, committed, rebooted))
        .expect("every live cell has a row");
    match state {
        State::Running { .. } => row.4,
        _ => row.3,
    }
}

/// Catches a cell of the table of design section 6.5 changed or missing: every step, state,
/// commit, watcher and reboot is asked, and each answer is the table's.
#[test]
fn the_resume_table_has_one_action_per_cell() {
    assert_eq!(Step::ALL.len(), 5, "a new step needs its rows in LIVE");
    let mut cells = 0;
    for step in Step::ALL {
        for state in states() {
            for committed in [false, true] {
                for watching in [false, true] {
                    for rebooted in [false, true] {
                        let rec = record(step, state.clone(), committed, watching);
                        let want = expected(step, &state, committed, watching, rebooted);
                        let what = format!(
                            "{step} {state:?} committed {committed} watching {watching} rebooted {rebooted}"
                        );
                        assert_eq!(resume_action(&rec, rebooted), want, "{what}");
                        cells += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cells, 5 * 4 * 2 * 2 * 2);
}

/// A rewrite of `rec` at second `now` of boot `boot`, the host having booted or not.
fn rewrite(mut rec: Record, rebooted: bool, boot: &str, now: u64) -> (ResumeAction, bool, Record) {
    let rig = Rig::new("resume-rewrite");
    rig.clock.reboot(boot);
    rig.clock.advance(now - 1000);
    let (action, changed) = resume(&mut rec, rebooted, &*rig.clock);
    (action, changed, rec)
}

fn running(boot: &str) -> State {
    State::Running {
        started: stamp(boot, 300),
        deadline_secs: 900,
        unbounded: false,
    }
}

/// Catches a cell whose record is not rewritten into what the runner then does as the table
/// says: a `prepare` cut off by a boot is moved to `rollback` with the verdict `Deferred
/// rebooted` waiting; a `cutover` or a `verify` before the commit is read as killed; a committed
/// `cutover` moves on to `verify` with its window begun now; a committed `verify` runs again
/// with a fresh window; a `rollback` cut off runs again as its next attempt.
#[test]
fn resume_rewrites_each_reboot_cell_for_the_runner() {
    let (action, changed, rec) = rewrite(
        record(Step::Prepare, running("boot-a"), false, false),
        true,
        "boot-b",
        1500,
    );
    assert_eq!((action, changed), (DEFERRED, true));
    assert_eq!(
        (rec.step, rec.attempt, rec.state),
        (Step::Rollback, 1, State::Pending)
    );
    assert_eq!(rec.hook, HookSel::New);
    assert_eq!(
        rec.progress.then,
        Some(Verdict::Deferred {
            retry_after_secs: None
        })
    );
    assert_eq!(rec.progress.reason.as_deref(), Some("rebooted"));
    assert_eq!(rec.progress.retry, 0);

    for step in [Step::Cutover, Step::Verify] {
        let (action, changed, rec) = rewrite(
            record(step, State::Pending, false, false),
            true,
            "boot-b",
            1500,
        );
        assert!(matches!(action, RollbackThenFailed { .. }), "{step}");
        assert!(changed);
        assert_eq!(
            rec.state,
            State::Done {
                code: None,
                last_line: None
            },
            "{step}"
        );
    }

    let (action, changed, rec) = rewrite(
        record(Step::Cutover, running("boot-a"), true, false),
        true,
        "boot-b",
        1500,
    );
    assert_eq!((action, changed), (RestartNewReleaseContinueNext, true));
    assert_eq!(
        (rec.step, rec.attempt, rec.state),
        (Step::Verify, 1, State::Pending)
    );
    assert_eq!(rec.progress.verify_started, Some(stamp("boot-b", 1500)));

    let (action, changed, rec) = rewrite(
        record(Step::Verify, running("boot-a"), true, false),
        true,
        "boot-b",
        1500,
    );
    assert_eq!((action, changed), (ContinueVerifyFreshWindow, true));
    assert_eq!(
        (rec.step, rec.attempt, rec.state),
        (Step::Verify, 3, State::Pending)
    );
    assert_eq!(rec.progress.verify_started, Some(stamp("boot-b", 1500)));

    let (action, changed, rec) = rewrite(
        record(Step::Rollback, running("boot-a"), false, false),
        true,
        "boot-b",
        1500,
    );
    assert_eq!((action, changed), (RerunStep, true));
    assert_eq!(
        (rec.step, rec.attempt, rec.state),
        (Step::Rollback, 3, State::Pending)
    );
}

/// Catches a rewrite of a record the table leaves to the runner, or of a finished one: a step
/// running in the same boot (the daemon died) is left for the runner to stop and run again, a
/// pending one to run, a `done` one to decide from; a finished record is never changed, not
/// even its stamps; a step that started in this boot is read as after a death of the daemon even
/// when the boot id says the host booted (a start after the boot ran it); and a watcher's
/// ceiling and a verify window of another boot begin again now.
#[test]
fn resume_leaves_what_the_runner_does_itself() {
    for (state, want) in [
        (running("boot-a"), StopUnitAndRerun),
        (State::Pending, RerunStep),
        (
            State::Done {
                code: Some(1),
                last_line: None,
            },
            RecomputeNext,
        ),
    ] {
        let before = record(Step::Cleanup, state, true, false);
        let (action, changed, rec) = rewrite(before.clone(), false, "boot-a", 1500);
        assert_eq!((action, changed), (want, false));
        assert_eq!(rec, before);
    }

    let mut finished = record(
        Step::Cleanup,
        State::Finished {
            verdict: Verdict::Halt,
            reason: None,
        },
        false,
        true,
    );
    let before = finished.clone();
    let rig = Rig::new("resume-finished");
    rig.clock.reboot("boot-b");
    assert_eq!(
        resume(&mut finished, true, &*rig.clock),
        (NothingFinished, false)
    );
    assert_eq!(finished, before);

    let (action, changed, rec) = rewrite(
        record(Step::Prepare, running("boot-b"), false, false),
        true,
        "boot-b",
        1500,
    );
    // Only the verify window stamped in boot `a` changed.
    assert_eq!((action, changed), (StopUnitAndRerun, true));
    assert_eq!((rec.step, rec.state), (Step::Prepare, running("boot-b")));

    let (action, changed, rec) = rewrite(
        record(Step::Verify, running("boot-a"), true, true),
        true,
        "boot-b",
        1500,
    );
    assert_eq!((action, changed), (WatcherRestart, true));
    assert_eq!(rec.progress.watching, Some(stamp("boot-b", 1500)));
    assert_eq!(rec.progress.verify_started, Some(stamp("boot-b", 1500)));
    assert_eq!(rec.state, running("boot-a"));
}

/// Runs `rig`'s order with the launcher losing run `lose` (the daemon dies with the step's unit
/// alive), then boots the host again, rewrites the record for its cell and resumes: the verdict,
/// the cell, and the steps run after the boot.
fn reboot_after_losing(rig: &Rig, lose: u64) -> (Finished, ResumeAction, Vec<Step>) {
    rig.fake.lose_run.store(lose, Ordering::SeqCst);
    assert!(rig.runner().run(&rig.order()).is_err());
    rig.clock.reboot("boot-b");
    rig.clock.advance(5000);
    let Loaded::Current(mut rec) = rig.store().load().expect("load") else {
        panic!("a record")
    };
    let (action, changed) = resume(&mut rec, true, &*rig.clock);
    if changed {
        rig.store().save(&rec).expect("save");
    }
    rig.fake.clear();
    let done = rig.runner().run(&rig.order()).expect("resumed");
    (done, action, rig.fake.steps())
}

fn ok() -> Finished {
    Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    }
}

/// Catches the two cells the runner did not do by itself before: a `cutover` cut off by a boot
/// after an earlier step committed is not rolled back (the reboot restarted the new release) but
/// continues with `verify`; and a committed `verify` cut off by a boot runs again with a whole
/// window begun in the new boot, which then runs out there (a window still stamped with the old
/// boot never would: `verify` deferring would be run again without end). Before the commit a
/// reboot is an abort: `rollback`, then the table's verdict.
#[test]
fn the_reboot_cells_reach_their_verdict_through_the_runner() {
    let rig = Rig::new("resume-cutover-committed");
    rig.fake.say(Step::Prepare, &[IRREVERSIBLE_OK]);
    let (done, action, steps) = reboot_after_losing(&rig, 2);
    assert_eq!(action, RestartNewReleaseContinueNext);
    assert_eq!(done, ok());
    assert_eq!(steps, [Step::Verify, Step::Cleanup]);

    let rig = Rig::new("resume-verify-committed");
    rig.fake.say(Step::Cutover, &[IRREVERSIBLE_OK]);
    rig.fake.say(Step::Verify, &[DEFER; 40]);
    let (done, action, steps) = reboot_after_losing(&rig, 3);
    assert_eq!(action, ContinueVerifyFreshWindow);
    assert_eq!(
        done.verdict,
        Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release
        }
    );
    // The window of 1600 seconds, begun at the resume, with a wait of 100 between two runs: the
    // 16th deferral would run past it.
    let verifies = steps.iter().filter(|step| **step == Step::Verify).count();
    assert_eq!(verifies, 16);
    assert_eq!(steps.last(), Some(&Step::Rollback));
    let timeouts: Vec<u64> = rig
        .fake
        .runs()
        .iter()
        .filter_map(|e| match e {
            support::Event::Run {
                step: Step::Verify,
                timeout_secs,
                ..
            } => Some(*timeout_secs),
            _ => None,
        })
        .collect();
    assert_eq!(timeouts.first(), Some(&1000));

    let rig = Rig::new("resume-prepare-reboot");
    let (done, action, steps) = reboot_after_losing(&rig, 1);
    assert_eq!(action, DEFERRED);
    assert_eq!(
        done,
        Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: None
            },
            reason: Some("rebooted".to_owned())
        }
    );
    assert_eq!(steps, [Step::Rollback]);

    for (lose, want, phase) in [
        (2, BEFORE_COMMIT, Phase::BeforeCommit),
        (3, GATE_RELEASE, Phase::GateRelease),
    ] {
        let rig = Rig::new("resume-before-commit");
        let (done, action, steps) = reboot_after_losing(&rig, lose);
        assert_eq!(action, want);
        assert_eq!(
            done.verdict,
            Verdict::Failed {
                phase,
                class: Class::Release
            }
        );
        assert_eq!(steps, [Step::Rollback]);
    }

    let rig = Rig::new("resume-rollback-reboot");
    rig.fake.say(Step::Verify, &[FAIL]);
    let (done, action, steps) = reboot_after_losing(&rig, 4);
    assert_eq!(action, RerunStep);
    assert_eq!(
        done.verdict,
        Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release
        }
    );
    assert_eq!(steps, [Step::Rollback]);
}

/// Catches a start pass held up by a watcher, or a watcher lost on the way: the update runs to
/// the post-success watcher and is handed back as `Succeeded` with the promotion pending, its
/// record left in the watcher; a second hand-off runs nothing; a later run watches it to the end.
#[test]
fn run_until_watching_leaves_the_watcher_to_a_later_run() {
    let rig = Rig::new("resume-handoff");
    rig.fake.say(Step::Verify, &[PENDING, OK]);
    let runner = rig.runner();
    let pending = Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: true,
        },
        reason: None,
    };
    assert_eq!(
        runner.run_until_watching(&rig.order()).expect("run"),
        pending
    );
    let first = [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup];
    assert_eq!(rig.fake.steps(), first);
    let Loaded::Current(rec) = rig.store().load().expect("load") else {
        panic!("a record")
    };
    assert!(rec.progress.watching.is_some());
    assert_eq!((rec.step, rec.state), (Step::Verify, State::Pending));
    assert_eq!(
        runner.run_until_watching(&rig.order()).expect("again"),
        pending
    );
    assert_eq!(rig.fake.steps(), first);
    assert_eq!(runner.run(&rig.order()).expect("watch"), ok());
    assert_eq!(rig.fake.steps().len(), 5);
}

/// Catches a write fault that touches the disk or fails the wrong write: the second write fails
/// with the injected error and the record on disk is still the first.
#[test]
fn a_write_fault_fails_the_write_before_it_touches_the_disk() {
    let rig = Rig::new("resume-write-fault");
    let fault = Arc::new(|n: u64| (n == 2).then(|| io::Error::from_raw_os_error(28)));
    let store = rig.store().with_write_fault(fault);
    let first = record(Step::Prepare, State::Pending, false, false);
    store.save(&first).expect("the first write");
    let mut second = first.clone();
    second.attempt = 9;
    let Err(RecordError::Io(error)) = store.save(&second) else {
        panic!("the second write fails")
    };
    assert_eq!(error.raw_os_error(), Some(28));
    assert_eq!(
        rig.store().load().expect("load"),
        Loaded::Current(Box::new(first))
    );
}

/// Catches a launcher that runs something else for an additive step it does not know: the default
/// starts nothing and names the step and the service.
#[test]
fn a_launcher_runs_no_additive_step_by_default() {
    let rig = Rig::new("resume-additive");
    let call = AdditiveCall {
        service: "svc",
        step: "restore",
        hook_args: &["update-hook".to_owned()],
        params: Path::new("/state/svc/params.toml"),
        executable: Path::new("/packages/svc/1.0.0/hook"),
        unit: "updater-svc-restore-0",
        timeout_secs: 300,
    };
    let Err(LaunchError::NoStart(why)) = rig.fake.run_additive(&call) else {
        panic!("refused")
    };
    assert_eq!(
        why,
        "this launcher runs no additive step, so `restore` of svc did not start"
    );
    assert!(rig.fake.events().is_empty());
}
