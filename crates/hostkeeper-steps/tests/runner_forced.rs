//! A record a `force` flagged `forced` (design section 5.6, steps 4 to 6; section 8.7): the
//! runner reads the flag before it launches any step and after every step returns, closes the
//! update `Failed`, `forced_over`, and launches nothing more.

mod support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{
    Loaded, PARAMS_FILE, REASON_FORCED_OVER, RECORD_FILE, Record, State,
};
use hostkeeper_steps::{CrashPoint, Finished, ForcePhase, ForceRecord, RecordStore};
use support::{Event, FAIL, IRREVERSIBLE_OK, Rig, io_err};

fn forced_over(phase: Phase) -> Finished {
    Finished {
        verdict: Verdict::Failed {
            phase,
            class: Class::Environment,
        },
        reason: Some(REASON_FORCED_OVER.to_owned()),
    }
}

fn record(rig: &Rig) -> Record {
    match rig.store().load().expect("the record") {
        Loaded::Current(rec) => *rec,
        other => panic!("no record: {other:?}"),
    }
}

/// Flags the record of `rig` when `step` starts, as a force does while the step runs.
fn flag_during(rig: &Rig, step: Step) {
    let state = rig.state.clone();
    rig.fake.on_run(move |call| {
        if call.invocation.step == step {
            let flagged = RecordStore::new(&state)
                .mark_forced()
                .expect("flag the record");
            assert!(flagged.is_some(), "an unfinished update is flagged");
        }
    });
}

/// Where the flag is set, how the rig is scripted for it, and the steps launched.
struct Row {
    name: &'static str,
    arm: fn(&Rig) -> bool,
    steps: &'static [Step],
    phase: Phase,
}

/// Catches a runner that reads the flag only after a step returns, or launches `rollback` for a
/// closed update: with the flag set before any step, after `prepare`, after `cutover`, after a
/// failed `cutover` that would run `rollback`, after a committed `cutover`, and on a resume of a
/// step left running, the update ends `Failed`, `forced_over` (after the commit once a step
/// committed), its record final and flagged, its params file gone, and no step launched after
/// the flag; the same order again is answered from the record.
#[test]
fn the_runner_checks_the_forced_flag_before_launching_a_step() {
    let rows = [
        Row {
            name: "before any step",
            arm: |rig| {
                let gate = Arc::new(|point: CrashPoint| point == CrashPoint::AfterWrite(1));
                assert!(rig.runner_gated(gate).run(&rig.order()).is_err());
                true
            },
            steps: &[],
            phase: Phase::BeforeCommit,
        },
        Row {
            name: "after prepare",
            arm: |rig| {
                flag_during(rig, Step::Prepare);
                false
            },
            steps: &[Step::Prepare],
            phase: Phase::BeforeCommit,
        },
        Row {
            name: "after cutover",
            arm: |rig| {
                flag_during(rig, Step::Cutover);
                false
            },
            steps: &[Step::Prepare, Step::Cutover],
            phase: Phase::BeforeCommit,
        },
        Row {
            name: "after a failed cutover",
            arm: |rig| {
                rig.fake.say(Step::Cutover, &[FAIL]);
                flag_during(rig, Step::Cutover);
                false
            },
            steps: &[Step::Prepare, Step::Cutover],
            phase: Phase::BeforeCommit,
        },
        Row {
            name: "after a committed cutover",
            arm: |rig| {
                rig.fake.say(Step::Cutover, &[IRREVERSIBLE_OK]);
                flag_during(rig, Step::Verify);
                false
            },
            steps: &[Step::Prepare, Step::Cutover, Step::Verify],
            phase: Phase::AfterCommit,
        },
        Row {
            name: "on resume",
            arm: |rig| {
                rig.fake.lose_run.store(2, Ordering::SeqCst);
                assert!(rig.runner().run(&rig.order()).is_err());
                true
            },
            steps: &[Step::Prepare, Step::Cutover],
            phase: Phase::BeforeCommit,
        },
    ];
    for row in rows {
        let rig = Rig::new("forced");
        let flag_first = (row.arm)(&rig);
        if flag_first {
            let flagged = rig.store().mark_forced().expect("flag the record");
            assert!(flagged.is_some(), "{}", row.name);
        }
        let events = rig.fake.events();
        let ended = rig.runner().run(&rig.order()).expect(row.name);
        assert_eq!(ended, forced_over(row.phase), "{}", row.name);
        assert_eq!(rig.fake.steps(), row.steps, "{}", row.name);
        if flag_first {
            assert_eq!(
                rig.fake.events(),
                events,
                "{}: nothing after the flag",
                row.name
            );
        }
        let rec = record(&rig);
        assert!(rec.forced, "{}", row.name);
        assert_eq!(
            rec.state,
            State::Finished {
                verdict: forced_over(row.phase).verdict,
                reason: Some(REASON_FORCED_OVER.to_owned()),
            },
            "{}",
            row.name
        );
        assert!(!rig.state.join(PARAMS_FILE).exists(), "{}", row.name);
        let again = rig.runner().run(&rig.order()).expect(row.name);
        assert_eq!(again, forced_over(row.phase), "{}", row.name);
        assert_eq!(rig.fake.steps(), row.steps, "{}", row.name);
        assert!(
            !rig.fake.events().iter().any(|event| matches!(
                event,
                Event::Run {
                    step: Step::Rollback | Step::Cleanup,
                    ..
                }
            )),
            "{}",
            row.name
        );
    }
}

/// Catches a flag written where no hooked update runs, written twice, or written over a record
/// that is not the runner's to close: no record, a finished one, a force's journal and one of a
/// newer schema are left as they are; an unfinished update is flagged once, and flagging it again
/// writes nothing.
#[test]
fn only_an_unfinished_hooked_update_is_flagged() {
    let rig = Rig::new("forced-mark");
    let store = rig.store().with_lock(Arc::default());
    assert!(store.mark_forced().expect("no record").is_none());

    rig.fake.lose_run.store(1, Ordering::SeqCst);
    assert!(rig.runner().run(&rig.order()).is_err());
    let flagged = store
        .mark_forced()
        .expect("flag")
        .expect("the update is unfinished");
    assert!(flagged.forced);
    assert!(record(&rig).forced);
    let no_writes =
        RecordStore::new(&rig.state).with_write_fault(Arc::new(|_: u64| Some(io_err())));
    assert!(
        no_writes
            .mark_forced()
            .expect("no write is tried")
            .is_some()
    );

    let path = rig.state.join(RECORD_FILE);
    let mut finished = record(&rig);
    finished.forced = false;
    finished.state = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    let mut journal = finished.clone();
    journal.state = State::Pending;
    journal.force = Some(ForceRecord {
        target_version: "2.0.0".to_owned(),
        target_digest: "ab".repeat(32),
        from_version: None,
        phase: ForcePhase::Started,
        order_try: 1,
        halt_generation: 0,
        order_id: None,
        verdict: None,
    });
    for rec in [finished, journal] {
        rig.store().save(&rec).expect("write the record");
        let bytes = fs::read(&path).expect("the record");
        assert!(store.mark_forced().expect("read").is_none(), "{rec:?}");
        assert_eq!(fs::read(&path).expect("the record"), bytes);
    }
    fs::write(&path, r#"{"schema": 2}"#).expect("a newer record");
    assert!(store.mark_forced().expect("read").is_none());
    assert_eq!(
        fs::read_to_string(&path).expect("the record"),
        r#"{"schema": 2}"#
    );
}
