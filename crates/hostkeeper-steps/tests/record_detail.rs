//! The hook's `detail` in the step record (`Progress::detail`): kept under the reason of its
//! line, given with a finished verdict of that reason, and additive on disk; and what the record
//! keeps of a `verify` that defers (`Progress::deferrals`) and of the failure a `rollback` follows
//! (`Progress::cause`), and how the runner reports them.

mod support;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use hostkeeper_hook::{Class, Phase, Report, Step, Verdict};
use hostkeeper_steps::clock::Stamp;
use hostkeeper_steps::record::{
    Deferrals, HookDetail, HookSel, Loaded, Progress, REASON_FORCED_OVER, RECORD_FILE, SCHEMA,
    State,
};
use hostkeeper_steps::runner::REASON_VERIFY_DEADLINE;
use hostkeeper_steps::{CrashPoint, Finished, Record};
use support::{DEFER, IRREVERSIBLE_OK, OK, Reply, Rig};

fn record(state: State, progress: Progress) -> Record {
    Record {
        schema: SCHEMA,
        service: "js-agent".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "abc".to_owned(),
        tarball_digest: "0123".to_owned(),
        previous_digest: None,
        generation: 1,
        params: "/state/params.toml".into(),
        step: Step::Prepare,
        attempt: 1,
        state,
        hook: HookSel::New,
        progress,
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

fn halted(reason: Option<&str>) -> State {
    State::Finished {
        verdict: Verdict::Halt,
        reason: reason.map(str::to_owned),
    }
}

fn kept(reason: Option<&str>, text: &str) -> Option<HookDetail> {
    Some(HookDetail {
        reason: reason.map(str::to_owned),
        text: text.to_owned(),
    })
}

/// Catches a line without a detail wiping the one kept, and a detail kept under the wrong
/// reason: each line with a detail replaces the kept one under its own reason (none when it gave
/// none), and a line with none leaves it.
#[test]
fn a_line_with_a_detail_replaces_the_kept_one_and_a_line_without_leaves_it() {
    let mut progress = Progress::default();
    progress.keep_detail(&Report::read(r#"{"reason":"bad_unit","detail":"first"}"#));
    assert_eq!(progress.detail, kept(Some("bad_unit"), "first"));
    progress.keep_detail(&Report::read(r#"{"reason":"drain_bound"}"#));
    progress.keep_detail(&Report::read(""));
    assert_eq!(progress.detail, kept(Some("bad_unit"), "first"));
    progress.keep_detail(&Report::read(r#"{"detail":"second"}"#));
    assert_eq!(progress.detail, kept(None, "second"));
}

/// Catches the prose of a step that succeeded being kept, and later shown beside a verdict it
/// does not explain: the line of a step that exited 0 keeps nothing, and that of a step that
/// exited otherwise, or did not exit (killed), keeps its detail; either way the report is the
/// line's.
#[test]
fn only_the_line_of_a_step_that_did_not_succeed_keeps_its_detail() {
    let said = r#"{"reason":"bad_unit","detail":"why"}"#;
    let mut progress = Progress::default();
    let report = progress.read_line(Some(0), Some(r#"{"detail":"migrated 3 rows"}"#));
    assert_eq!(report.detail.as_deref(), Some("migrated 3 rows"));
    assert_eq!(progress.detail, None);
    for code in [Some(1), Some(75), Some(76), None] {
        let mut progress = Progress::default();
        let report = progress.read_line(code, Some(said));
        assert_eq!(report.reason.as_deref(), Some("bad_unit"), "{code:?}");
        assert_eq!(progress.detail, kept(Some("bad_unit"), "why"), "{code:?}");
    }
    let mut progress = Progress::default();
    assert_eq!(progress.read_line(Some(1), None).detail, None);
    assert_eq!(progress.detail, None);
}

/// Catches a detail given with a verdict it does not explain: it is given when the record is
/// finished, not `Succeeded`, with the reason it was kept under (none and none included), and not
/// when the record is unfinished, succeeded, is finished with another reason (one the updater
/// gave of its own), or kept none.
#[test]
fn the_finished_detail_is_the_one_kept_under_the_verdicts_reason() {
    let under = |reason, state| {
        let progress = Progress {
            detail: kept(reason, "why"),
            ..Progress::default()
        };
        record(state, progress)
    };
    assert_eq!(
        under(Some("state_invalid"), halted(Some("state_invalid"))).finished_detail(),
        Some("why")
    );
    assert_eq!(under(None, halted(None)).finished_detail(), Some("why"));
    assert_eq!(
        under(Some("state_invalid"), halted(Some("artifact_changed"))).finished_detail(),
        None
    );
    assert_eq!(
        under(None, halted(Some("artifact_changed"))).finished_detail(),
        None
    );
    assert_eq!(
        under(Some("state_invalid"), State::Pending).finished_detail(),
        None
    );
    let succeeded = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    assert_eq!(under(None, succeeded).finished_detail(), None);
    let none = record(halted(Some("state_invalid")), Progress::default());
    assert_eq!(none.finished_detail(), None);
}

/// Catches a format change that is not additive: a record with a kept detail writes it as
/// `progress.detail` (`reason` left out when none), a record without one writes no key, and a
/// record written before the field existed loads with none.
#[test]
fn the_detail_is_written_only_when_kept_and_an_older_record_loads_without_it() {
    let with = record(
        halted(Some("state_invalid")),
        Progress {
            detail: kept(Some("state_invalid"), "why"),
            ..Progress::default()
        },
    );
    let json = serde_json::to_value(&with).expect("the record as JSON");
    assert_eq!(
        json["progress"]["detail"],
        serde_json::json!({"reason": "state_invalid", "text": "why"})
    );
    let reasonless = HookDetail {
        reason: None,
        text: "why".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&reasonless).expect("the detail as JSON"),
        r#"{"text":"why"}"#
    );
    let without = record(halted(Some("state_invalid")), Progress::default());
    let json = serde_json::to_value(&without).expect("the record as JSON");
    assert!(json["progress"].get("detail").is_none(), "{json}");
    let older = serde_json::to_string(&without).expect("the record as text");
    let loaded: Record = serde_json::from_str(&older).expect("an older record loads");
    assert_eq!(loaded.progress.detail, None);
    assert_eq!(loaded, without);
}

/// A deferral of `verify` for the environment's fault, as tierstore's hook writes one: the
/// node's mount provider is busy.
const BUSY: Reply = Reply::Exit(
    75,
    Some(
        r#"{"class":"environment","reason":"provider_busy","retry_after_secs":100,"detail":"the tree was not made: too many placements"}"#,
    ),
);
/// The next deferral, with another detail: the one an operator needs.
const REFUSED: Reply = Reply::Exit(
    75,
    Some(
        r#"{"class":"environment","reason":"provider_busy","retry_after_secs":100,"detail":"read of /Volumes/build/js.job: Operation not permitted"}"#,
    ),
);
/// The last detail `REFUSED` gives.
const REFUSED_DETAIL: &str = "read of /Volumes/build/js.job: Operation not permitted";

fn finished(phase: Phase, class: Class, reason: &str) -> Finished {
    Finished {
        verdict: Verdict::Failed { phase, class },
        reason: Some(reason.to_owned()),
    }
}

/// The record `rig` holds now.
fn on_disk(rig: &Rig) -> Record {
    match rig.store().load().expect("the record loads") {
        Loaded::Current(rec) => *rec,
        other => panic!("no current record: {other:?}"),
    }
}

/// The bounds of the runs of `verify`, in order.
fn verify_bounds(rig: &Rig) -> Vec<u64> {
    rig.fake
        .runs()
        .iter()
        .filter_map(|e| match e {
            support::Event::Run {
                step: Step::Verify,
                bound,
                ..
            } => Some(support::secs_of(*bound)),
            _ => None,
        })
        .collect()
}

/// Catches the farm's lost reason, the defect that left a host's last update and its alert at
/// `unspecified` and blamed the release: a `verify` that defers twice for the environment and
/// whose third run the window stops (it writes no line) is reported `Failed` of the environment's
/// class, in the phase `gate_environment`, with the last deferral's reason, and the detail says
/// how often it deferred over how long, with that reason and detail.
#[test]
fn a_verify_cut_off_by_its_window_after_deferrals_is_reported_with_the_last_one() {
    let rig = Rig::new("deferrals-cut-off");
    let mut order = rig.order();
    order.bounds.verify_deadline_secs = 250;
    rig.fake.say(Step::Verify, &[BUSY, REFUSED, Reply::Killed]);
    let (clock, verifies) = (rig.clock.clone(), Arc::new(AtomicU32::new(0)));
    rig.fake.on_run(move |call| {
        let verify = call.invocation.step == Step::Verify;
        if verify && verifies.fetch_add(1, Ordering::SeqCst) == 2 {
            // The third run is stopped at its bound, the end of the window.
            clock.advance(support::secs_of(call.bound));
        }
    });
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(
        got,
        finished(Phase::GateEnvironment, Class::Environment, "provider_busy")
    );
    assert_eq!(verify_bounds(&rig), [250, 150, 50]);
    assert_eq!(rig.fake.steps().last(), Some(&Step::Rollback));
    let rec = on_disk(&rig);
    let said = format!(
        "verify deferred 2 times over 250 seconds; last reason: provider_busy; last detail: \
         {REFUSED_DETAIL}"
    );
    assert_eq!(rec.finished_detail(), Some(said.as_str()));
    let deferrals = rec.progress.deferrals.expect("the deferrals are kept");
    assert_eq!(
        (deferrals.count, deferrals.class, deferrals.attempt),
        (2, Class::Environment, 2)
    );
    assert_eq!(deferrals.since.mono_secs, 1000);
}

/// Catches a window that ends on a deferral reported without words, and the count's boundary: a
/// `verify` that defers once with no class, reason or detail, leaving no time for another run, is
/// `Failed` of the release's class with the reason `verify_deadline`, and the detail says one
/// time, and `none` for what it did not give; once a step committed, the phase is
/// `after_commit`, of the class of the deferrals.
#[test]
fn a_window_that_ends_on_a_deferral_says_so_in_words() {
    let rig = Rig::new("deferrals-once");
    let mut order = rig.order();
    order.bounds.verify_deadline_secs = 100;
    rig.fake.say(Step::Verify, &[DEFER]);
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(REASON_VERIFY_DEADLINE, "verify_deadline");
    assert_eq!(
        got,
        finished(Phase::GateRelease, Class::Release, REASON_VERIFY_DEADLINE)
    );
    assert_eq!(
        on_disk(&rig).finished_detail(),
        Some("verify deferred 1 time over 0 seconds; last reason: none; last detail: none")
    );
    assert!(rig.clock.slept().is_empty());

    let rig = Rig::new("deferrals-committed");
    let mut order = rig.order();
    order.bounds.verify_deadline_secs = 150;
    rig.fake.say(Step::Cutover, &[IRREVERSIBLE_OK]);
    rig.fake.say(Step::Verify, &[BUSY, REFUSED]);
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(
        got,
        finished(Phase::AfterCommit, Class::Environment, "provider_busy")
    );
    let said = format!(
        "verify deferred 2 times over 100 seconds; last reason: provider_busy; last detail: \
         {REFUSED_DETAIL}"
    );
    assert_eq!(on_disk(&rig).finished_detail(), Some(said.as_str()));
}

/// Catches the class rule reaching past the window: a `verify` that defers for the environment
/// and then fails of its own, inside the window, is reported as its line says (the release's
/// class, its own reason and detail); one stopped by its own bound while the window still has
/// time is the release's failure with no reason, as before.
#[test]
fn a_verify_that_fails_of_its_own_after_deferrals_is_reported_as_it_says() {
    let rig = Rig::new("deferrals-then-fail");
    let failed = Reply::Exit(
        1,
        Some(r#"{"class":"release","reason":"wrong_build","detail":"the gauge says 0.3.211"}"#),
    );
    rig.fake.say(Step::Verify, &[BUSY, failed]);
    let got = rig.runner().run(&rig.order()).expect("a verdict");
    assert_eq!(
        got,
        finished(Phase::GateRelease, Class::Release, "wrong_build")
    );
    assert_eq!(
        on_disk(&rig).finished_detail(),
        Some("the gauge says 0.3.211")
    );

    let rig = Rig::new("deferrals-then-hung");
    let mut order = rig.order();
    order.bounds.verify_secs = 50;
    rig.fake.say(Step::Verify, &[BUSY, Reply::Killed]);
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Release
            },
            reason: None
        }
    );
    assert_eq!(on_disk(&rig).finished_detail(), None);
}

/// Catches the cause of a failure lost to the `rollback` that follows it: a `verify` that fails at
/// once, and a `prepare` that fails, each saying why, followed by a `rollback` that defers once
/// with a text of its own and then succeeds saying a reason of its own, are reported with the
/// reason and the detail of the step that failed.
#[test]
fn a_failure_keeps_its_reason_and_detail_through_a_rollback_that_says_its_own() {
    let draining = Reply::Exit(
        75,
        Some(r#"{"reason":"draining","retry_after_secs":10,"detail":"the old instance drains"}"#),
    );
    let rolled_back = Reply::Exit(0, Some(r#"{"reason":"rolled_back"}"#));
    let cases = [
        (
            Step::Verify,
            r#"{"class":"release","reason":"wrong_build","detail":"the gauge says 0.3.211"}"#,
            finished(Phase::GateRelease, Class::Release, "wrong_build"),
            "the gauge says 0.3.211",
        ),
        (
            Step::Prepare,
            r#"{"class":"environment","reason":"config_missing","detail":"no /etc/tierstore/node.toml"}"#,
            finished(Phase::BeforeCommit, Class::Environment, "config_missing"),
            "no /etc/tierstore/node.toml",
        ),
    ];
    for (step, line, want, detail) in cases {
        let rig = Rig::new(&format!("cause-{}", step.name()));
        rig.fake.say(step, &[Reply::Exit(1, Some(line))]);
        rig.fake
            .say(Step::Rollback, &[draining.clone(), rolled_back.clone()]);
        let got = rig.runner().run(&rig.order()).expect("a verdict");
        assert_eq!(got, want, "{step:?}");
        assert_eq!(rig.clock.slept(), [10], "{step:?}");
        let rec = on_disk(&rig);
        assert_eq!(rec.finished_detail(), Some(detail), "{step:?}");
        assert_eq!(rec.progress.cause, None, "{step:?}");
    }
}

/// Catches a deferral that is only on disk once the wait is over, and one counted twice: the
/// record is written with the deferral counted before the runner waits (the heartbeat reads it
/// there), and an updater that dies just after that write and runs the order again counts the
/// same run of `verify` once.
#[test]
fn a_deferral_is_on_disk_before_the_wait_and_counted_once_across_a_restart() {
    let rig = Rig::new("deferrals-on-disk");
    rig.fake.say(Step::Verify, &[BUSY, OK]);
    let path = rig.state.join(RECORD_FILE);
    let died = Arc::new(AtomicBool::new(false));
    let dies = Arc::clone(&died);
    let gate = Arc::new(move |point: CrashPoint| {
        let CrashPoint::AfterWrite(_) = point else {
            return false;
        };
        let rec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("the record")).expect("JSON");
        let waiting = (rec["state"]["state"] == "done") & !rec["progress"]["deferrals"].is_null();
        waiting && !dies.swap(true, Ordering::SeqCst)
    });
    let order = rig.order();
    assert!(rig.runner_gated(gate).run(&order).is_err());
    assert!(died.load(Ordering::SeqCst));
    assert!(rig.clock.slept().is_empty());
    let rec = on_disk(&rig);
    assert!(matches!(rec.state, State::Done { code: Some(75), .. }));
    assert_eq!(
        rec.progress.deferrals,
        Some(Deferrals {
            count: 1,
            since: Stamp {
                boot_id: BootId::new("a test", "boot-a").ok(),
                mono_secs: 1000,
            },
            class: Class::Environment,
            reason: Some("provider_busy".to_owned()),
            detail: Some("the tree was not made: too many placements".to_owned()),
            attempt: 1,
        })
    );
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None
        }
    );
    assert_eq!(rig.clock.slept(), [100]);
    let counted = on_disk(&rig).progress.deferrals.map(|d| d.count);
    assert_eq!(counted, Some(1));
}

/// A clock that asks the runner, once armed, to abort its update at the next read of the boot id:
/// a read the runner makes as it stamps a deferral of `verify`, after the run ended and before
/// the record is written for the wait.
struct AbortingClock {
    inner: Arc<support::FakeClock>,
    armed: AtomicBool,
    runner: OnceLock<Arc<StepRunner>>,
}

impl Clock for AbortingClock {
    fn mono_secs(&self) -> u64 {
        self.inner.mono_secs()
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            let runner = self.runner.get().expect("the runner");
            assert!(runner.abort().expect("the abort is asked").is_some());
        }
        self.inner.boot_id()
    }

    fn sleep_secs(&self, secs: u64) {
        self.inner.sleep_secs(secs);
    }
}

/// Catches a runner that waits out a deferral of an update an abort has already ended: an abort
/// asked as a `verify` deferral is read, before the wait, is read by the write that puts the
/// deferral on disk, and the update is closed `aborted_by_operator` with no wait and nothing more
/// run.
#[test]
fn an_abort_read_before_the_wait_of_a_deferral_ends_the_update_without_it() {
    let rig = Rig::new("deferrals-aborted");
    rig.fake.say(Step::Verify, &[BUSY]);
    let clock = Arc::new(AbortingClock {
        inner: rig.clock.clone(),
        armed: AtomicBool::new(false),
        runner: OnceLock::new(),
    });
    let runner = Arc::new(StepRunner::new(
        rig.fake.clone(),
        clock.clone(),
        rig.store(),
    ));
    clock.runner.set(Arc::clone(&runner)).unwrap_or_default();
    let arms = Arc::clone(&clock);
    rig.fake.on_run(move |call| {
        if call.invocation.step == Step::Verify {
            arms.armed.store(true, Ordering::SeqCst);
        }
    });
    let got = runner.run(&rig.order()).expect("a verdict");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Halt,
            reason: Some(REASON_ABORTED.to_owned())
        }
    );
    assert!(rig.clock.slept().is_empty());
    assert_eq!(
        rig.fake.steps(),
        [Step::Prepare, Step::Cutover, Step::Verify]
    );
    assert_eq!(on_disk(&rig).progress.deferrals.map(|d| d.count), Some(1));
}

/// Catches the cause kept under the wrong reason, or an empty one kept: the cause is the reason
/// and the detail kept under it, none when the detail was kept under another reason or is empty,
/// and a `rollback` that succeeded makes it the kept detail again, under the failure's reason.
#[test]
fn the_cause_is_the_reason_with_the_detail_kept_under_it() {
    let mut progress = Progress {
        reason: Some("wrong_build".to_owned()),
        detail: kept(Some("wrong_build"), "the gauge says 0.3.211"),
        ..Progress::default()
    };
    progress.keep_cause();
    assert_eq!(
        progress.cause,
        kept(Some("wrong_build"), "the gauge says 0.3.211")
    );
    progress.detail = kept(Some("draining"), "the old instance drains");
    assert_eq!(progress.restore_cause().as_deref(), Some("wrong_build"));
    assert_eq!(
        progress.detail,
        kept(Some("wrong_build"), "the gauge says 0.3.211")
    );
    assert_eq!(progress.cause, None);
    assert_eq!(progress.restore_cause().as_deref(), Some("wrong_build"));

    progress.detail = kept(Some("draining"), "the old instance drains");
    progress.keep_cause();
    assert_eq!(progress.cause, None);
    progress.detail = kept(Some("wrong_build"), "");
    progress.keep_cause();
    assert_eq!(progress.cause, None);
}

/// Catches a format change that is not additive: deferrals and a cause are written as
/// `progress.deferrals` and `progress.cause` (a reason or a detail left out when none), a record
/// without them writes no key, and a record written before they existed loads with none.
#[test]
fn deferrals_and_a_cause_are_written_only_when_kept_and_an_older_record_loads_without_them() {
    let stamp = Stamp {
        boot_id: BootId::new("a test", "b").ok(),
        mono_secs: 7,
    };
    let deferrals = Deferrals::after(None, &Report::read(""), stamp.clone(), 3);
    let deferrals = Deferrals::after(
        Some(deferrals),
        &Report::read(r#"{"class":"environment","reason":"provider_busy","detail":"why"}"#),
        Stamp {
            boot_id: None,
            mono_secs: 9,
        },
        4,
    );
    let with = record(
        halted(Some("provider_busy")),
        Progress {
            deferrals: Some(deferrals),
            cause: kept(Some("provider_busy"), "why"),
            ..Progress::default()
        },
    );
    let json = serde_json::to_value(&with).expect("the record as JSON");
    assert_eq!(
        json["progress"]["deferrals"],
        serde_json::json!({
            "count": 2,
            "since": {"boot_id": "b", "mono_secs": 7},
            "class": "environment",
            "reason": "provider_busy",
            "detail": "why",
            "attempt": 4,
        })
    );
    assert_eq!(
        json["progress"]["cause"],
        serde_json::json!({"reason": "provider_busy", "text": "why"})
    );
    let quiet = Deferrals::after(None, &Report::read(""), stamp, 1);
    assert_eq!(
        serde_json::to_value(&quiet).expect("the deferrals as JSON"),
        serde_json::json!({
            "count": 1,
            "since": {"boot_id": "b", "mono_secs": 7},
            "class": "release",
            "attempt": 1,
        })
    );
    let loaded: Record =
        serde_json::from_value(json).expect("a record with deferrals and a cause loads");
    assert_eq!(loaded, with);
    let without = record(halted(None), Progress::default());
    let json = serde_json::to_value(&without).expect("the record as JSON");
    assert!(json["progress"].get("deferrals").is_none(), "{json}");
    assert!(json["progress"].get("cause").is_none(), "{json}");
    let older = serde_json::to_string(&without).expect("the record as text");
    let loaded: Record = serde_json::from_str(&older).expect("an older record loads");
    assert_eq!(
        (loaded.progress.deferrals, loaded.progress.cause),
        (None, None)
    );
}

/// Catches a verify window that is not enforced: a `verify` that keeps deferring past
/// `verify_deadline_secs` is followed by `rollback`, and each run is bounded by the time the
/// window has left; it is reported with the reason `verify_deadline` when its deferrals gave none.
#[test]
fn verify_that_defers_past_the_window_rolls_back() {
    let rig = Rig::new("run-window");
    let mut order = rig.order();
    order.bounds.verify_deadline_secs = 250;
    rig.fake.say(Step::Verify, &[DEFER, DEFER, DEFER, DEFER]);
    let got = rig.runner().run(&order).unwrap();
    assert_eq!(
        got,
        finished(Phase::GateRelease, Class::Release, REASON_VERIFY_DEADLINE)
    );
    assert_eq!(verify_bounds(&rig), [250, 150, 50]);
    assert_eq!(rig.fake.steps().last(), Some(&Step::Rollback));
    assert_eq!(rig.clock.slept(), [100, 100]);
}
