//! The runner's abort ([`StepRunner::abort`]) and the record's close of an aborted update
//! ([`RecordStore::close_halted`]): an abort is read before any step is launched and when a step
//! returns, nothing more is launched, the update is closed `Halt` with the reason the design
//! names, a record the abort already closed is kept as it is, and an abort never ends another
//! update.

mod support;

use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::{Loaded, RECORD_FILE, State};
use hostkeeper_steps::runner::{REASON_ABORTED, REASON_HALT_CLEARED, REASON_UNSTOPPABLE};
use hostkeeper_steps::{DeadMan, Finished, RecordStore, RunError, StepRunner};
use support::{Event, Reply, Rig, Scratch};

fn halt(reason: &str) -> Finished {
    Finished {
        verdict: Verdict::Halt,
        reason: Some(reason.to_owned()),
    }
}

fn halt_state(reason: &str) -> State {
    State::Finished {
        verdict: Verdict::Halt,
        reason: Some(reason.to_owned()),
    }
}

/// The record in `store`, which must be a current one.
fn current(store: &RecordStore) -> hostkeeper_steps::Record {
    match store.load().expect("load") {
        Loaded::Current(rec) => *rec,
        other => panic!("no current record: {other:?}"),
    }
}

/// The runner of `rig`, shared with what its launcher does inside a step.
fn shared(rig: &Rig) -> (Arc<StepRunner>, Arc<OnceLock<Arc<StepRunner>>>) {
    let runner = Arc::new(rig.runner());
    let cell = Arc::new(OnceLock::new());
    cell.set(Arc::clone(&runner)).unwrap_or_default();
    (runner, cell)
}

/// Catches either spelling renamed: the reasons are wire values that coordinators and operators
/// match on.
#[test]
fn abort_reasons_are_the_exact_tokens_in_the_design() {
    assert_eq!(REASON_ABORTED, "aborted_by_operator");
    assert_eq!(REASON_UNSTOPPABLE, "unstoppable");
}

/// Catches an abort read only after the next step is launched, or a `rollback` or `cleanup`
/// run for an aborted update: an abort asked while `prepare` runs is read as the step returns;
/// no other step runs, the update ends `Halt` `aborted_by_operator`, the record says so, and the
/// params file is gone.
#[test]
fn an_abort_asked_during_a_step_ends_the_update_as_the_step_returns() {
    let rig = Rig::new("abort-during");
    let (runner, cell) = shared(&rig);
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            let rec = cell.get().expect("the runner").abort().expect("asked");
            assert!(matches!(
                rec.map(|rec| rec.state),
                Some(State::Running { .. })
            ));
        }
    });
    let ended = runner.run(&rig.order()).expect("a verdict");
    assert_eq!(ended, halt(REASON_ABORTED));
    assert_eq!(rig.fake.steps(), [Step::Prepare]);
    let rec = current(&rig.store());
    assert_eq!(rec.state, halt_state(REASON_ABORTED));
    assert_eq!(rec.step, Step::Prepare);
    assert!(!rig.store().params_path().exists());
}

/// Catches an abort of an update that is not running lost, or one that launches its step again:
/// an update left `running` by a launcher that failed is aborted between two runs; the next run
/// closes it `Halt` before it stops or launches anything.
#[test]
fn an_abort_asked_between_two_runs_is_read_before_anything_is_launched() {
    let rig = Rig::new("abort-between-runs");
    rig.fake.lose_run.store(1, Ordering::SeqCst);
    let runner = rig.runner();
    assert!(matches!(runner.run(&rig.order()), Err(RunError::Launch(_))));
    let events = rig.fake.events();
    let asked = runner
        .abort()
        .expect("asked")
        .expect("an unfinished update");
    assert!(matches!(asked.state, State::Running { .. }));
    assert_eq!(
        runner.run(&rig.order()).expect("closed"),
        halt(REASON_ABORTED)
    );
    assert_eq!(rig.fake.events(), events);
    assert_eq!(current(&rig.store()).state, halt_state(REASON_ABORTED));
}

/// Catches a runner that writes over the record an abort closed `unstoppable` (its unit could
/// not be stopped): when the step returns, or when the runner next looks, the record is kept as
/// the abort closed it, and the update ends `Halt` `unstoppable`.
#[test]
fn a_record_the_abort_closed_unstoppable_is_kept_as_it_is() {
    let rig = Rig::new("abort-kept-in-step");
    let (runner, cell) = shared(&rig);
    let store = rig.store();
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            cell.get().expect("the runner").abort().expect("asked");
            store.close_halted(REASON_UNSTOPPABLE).expect("closed");
        }
    });
    assert_eq!(
        runner.run(&rig.order()).expect("a verdict"),
        halt(REASON_UNSTOPPABLE)
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare]);
    assert_eq!(current(&rig.store()).state, halt_state(REASON_UNSTOPPABLE));

    let rig = Rig::new("abort-kept-between");
    rig.fake.lose_run.store(1, Ordering::SeqCst);
    let runner = rig.runner();
    assert!(runner.run(&rig.order()).is_err());
    runner.abort().expect("asked");
    rig.store()
        .close_halted(REASON_UNSTOPPABLE)
        .expect("closed");
    let before = fs::read(rig.state.join(RECORD_FILE)).expect("the record");
    assert_eq!(
        runner.run(&rig.order()).expect("a verdict"),
        halt(REASON_UNSTOPPABLE)
    );
    assert_eq!(
        fs::read(rig.state.join(RECORD_FILE)).expect("the record"),
        before
    );
}

/// Catches an aborted step that ends `Unstopped` because the dead-man had failed to stop it at
/// its bound: the abort wins, and the update ends `Halt` `aborted_by_operator`.
#[test]
fn an_aborted_step_the_dead_man_could_not_stop_ends_aborted_not_unstopped() {
    let rig = Rig::new("abort-deadman");
    let (runner, cell) = shared(&rig);
    let deadman = DeadMan::new(runner.watch(), rig.fake.clone(), rig.clock.clone());
    let clock = rig.clock.clone();
    rig.fake.fail_stops(1);
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            clock.advance(601);
            assert!(deadman.tick().is_err(), "the dead-man's stop fails");
            cell.get().expect("the runner").abort().expect("asked");
        }
    });
    assert_eq!(
        runner.run(&rig.order()).expect("a verdict"),
        halt(REASON_ABORTED)
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare]);
}

/// Catches an abort that reaches another update: an abort asked for the update of generation 1
/// and never read (its record was closed for it) does not end the update of generation 2 that
/// runs after the halt is cleared.
#[test]
fn an_abort_never_ends_another_update() {
    let rig = Rig::new("abort-other");
    rig.fake.lose_run.store(1, Ordering::SeqCst);
    let runner = rig.runner();
    assert!(runner.run(&rig.order()).is_err());
    runner.abort().expect("asked");
    rig.store()
        .close_halted(REASON_UNSTOPPABLE)
        .expect("closed");
    assert!(runner.clear_halt().expect("cleared"));
    assert_eq!(
        current(&rig.store()).state,
        State::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: None
            },
            reason: Some(REASON_HALT_CLEARED.to_owned()),
        }
    );
    let ended = runner.run(&rig.order()).expect("a verdict");
    assert_eq!(
        ended.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(current(&rig.store()).generation, 2);
}

/// Catches an abort asked of an update that is not there to end: no record, a finished one, one
/// of a newer schema and one that does not read each ask nothing (the last is an error), and a
/// later update runs to its end.
#[test]
fn an_abort_with_no_unfinished_update_asks_nothing() {
    let rig = Rig::new("abort-nothing");
    let runner = rig.runner();
    assert!(runner.abort().expect("no record").is_none());
    let ended = runner.run(&rig.order()).expect("a verdict");
    assert!(matches!(ended.verdict, Verdict::Succeeded { .. }));
    assert!(runner.abort().expect("finished").is_none());
    let path = rig.state.join(RECORD_FILE);
    fs::write(&path, r#"{"schema": 9}"#).expect("a newer record");
    assert!(runner.abort().expect("newer").is_none());
    fs::write(&path, "{").expect("a corrupt record");
    assert!(runner.abort().is_err());
}

/// Catches a close that rewrites an ended update or one a newer daemon wrote: an absent record
/// closes nothing; an unfinished one is closed `Halt` with the reason given and nothing else of
/// it changes; one already finished is returned as it is, byte for byte; a newer one is left.
#[test]
fn close_halted_closes_only_an_unfinished_update() {
    let rig = Rig::new("close-halted");
    let store = rig.store();
    assert!(
        store
            .close_halted(REASON_ABORTED)
            .expect("absent")
            .is_none()
    );
    rig.fake.lose_run.store(1, Ordering::SeqCst);
    assert!(rig.runner().run(&rig.order()).is_err());
    let running = current(&store);
    let closed = store
        .close_halted(REASON_ABORTED)
        .expect("closed")
        .expect("a record");
    assert_eq!(closed.state, halt_state(REASON_ABORTED));
    assert_eq!(
        (closed.generation, closed.step),
        (running.generation, running.step)
    );
    assert_eq!(current(&store), closed);
    let before = fs::read(rig.state.join(RECORD_FILE)).expect("the record");
    let again = store.close_halted(REASON_UNSTOPPABLE).expect("finished");
    assert_eq!(again.map(|rec| rec.state), Some(halt_state(REASON_ABORTED)));
    assert_eq!(
        fs::read(rig.state.join(RECORD_FILE)).expect("the record"),
        before
    );

    let scratch = Scratch::new("close-halted-newer");
    let newer = scratch.path().join(RECORD_FILE);
    fs::write(&newer, r#"{"schema": 9}"#).expect("a newer record");
    let store = RecordStore::new(scratch.path());
    assert!(store.close_halted(REASON_ABORTED).expect("newer").is_none());
    assert_eq!(fs::read(&newer).expect("left"), br#"{"schema": 9}"#);
}

/// Catches an abort that reads a step's report: the launcher's report of the aborted step (here
/// a failure, which would run `rollback`) is not read, and no step follows it.
#[test]
fn an_aborted_steps_report_is_not_read() {
    let rig = Rig::new("abort-report");
    let (runner, cell) = shared(&rig);
    rig.fake.say(Step::Prepare, &[Reply::Exit(1, None)]);
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Prepare {
            cell.get().expect("the runner").abort().expect("asked");
        }
    });
    assert_eq!(
        runner.run(&rig.order()).expect("a verdict"),
        halt(REASON_ABORTED)
    );
    let runs: Vec<Event> = rig.fake.runs();
    assert_eq!(runs.len(), 1);
}

/// The record a fixture of `tests/fixtures/aborted` holds.
fn aborted_fixture() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/aborted/finished.json"
    );
    fs::read_to_string(path).expect("the fixture")
}

/// Catches the abort's reason made a field of its own or a value an older reader refuses: a
/// record closed `Halt` with the reason `aborted_by_operator`, as this binary writes it (a
/// committed fixture of schema 1, never regenerated), loads with the record reader, which no
/// change of this kind touches, and is written back as the same JSON value, so that a reader of
/// schema 1 written before the abort existed reads it too.
#[test]
fn a_record_with_the_abort_reason_loads_under_the_previous_reader() {
    let scratch = Scratch::new("abort-fixture");
    let text = aborted_fixture();
    fs::write(scratch.path().join(RECORD_FILE), &text).expect("the fixture");
    let rec = current(&RecordStore::new(scratch.path()));
    assert_eq!(rec.schema, 1);
    assert_eq!(rec.state, halt_state(REASON_ABORTED));
    let written: serde_json::Value = serde_json::to_value(&rec).expect("serialised");
    let fixture: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(written, fixture);
}
