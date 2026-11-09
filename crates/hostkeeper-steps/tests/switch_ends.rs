//! A switch that ends its step (`SwitchError::Ended`) and built-in steps run through a port
//! (`StepRunner::with_builtin`).

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{
    BUILTIN_EXIT, Bound, Builtin, Finished, Loaded, RunError, StepExit, StepRunner, Switch,
    SwitchError, Toward, UpdateOrder,
};
use support::{FAIL, Reply, Rig};

type Seen<T> = Arc<Mutex<Vec<T>>>;

fn ended(code: i32, line: &str) -> SwitchError {
    SwitchError::Ended(StepExit {
        code: Some(code),
        last_line: Some(line.to_owned()),
    })
}

/// A runner of `rig` whose switch records each direction and answers `answer(toward)`.
fn switching(
    rig: &Rig,
    answer: impl Fn(Toward) -> Result<(), SwitchError> + Send + Sync + 'static,
) -> (StepRunner, Seen<Toward>) {
    let seen = Seen::default();
    let log = Arc::clone(&seen);
    let switch: Switch = Arc::new(move |toward: Toward, _: &UpdateOrder| {
        log.lock().expect("the log").push(toward);
        answer(toward)
    });
    let runner = rig.runner().with_switch(switch);
    (runner, seen)
}

fn recorded(rig: &Rig) -> Finished {
    let Ok(Loaded::Current(rec)) = rig.store().load() else {
        panic!("no record");
    };
    let State::Finished { verdict, reason } = rec.state else {
        panic!("not finished: {:?}", rec.state);
    };
    Finished { verdict, reason }
}

/// A cutover that the switch ended with exit 1 and a release report rolls back as a failed
/// cutover does: the rollback is switched Back and run, the cutover hook is never launched, and
/// the verdict is `Failed(before_commit, release)` with the report's reason.
#[test]
fn a_switch_that_ends_cutover_in_failure_rolls_back_with_its_reason() {
    let rig = Rig::new("ended-cutover");
    let (runner, seen) = switching(&rig, |toward| match toward {
        Toward::Target => Err(ended(1, r#"{"class":"release","reason":"dpkg_error"}"#)),
        _ => Ok(()),
    });
    let got = runner.run(&rig.order()).expect("the update ends");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release,
            },
            reason: Some("dpkg_error".to_owned()),
        }
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);
    assert_eq!(*seen.lock().unwrap(), [Toward::Target, Toward::Back]);
    assert_eq!(recorded(&rig), got);
}

/// A cutover that the switch ended with exit 75 is a deferral: rollback, then `Deferred` with
/// the hook's wait and reason.
#[test]
fn a_switch_that_ends_cutover_with_75_defers_the_update() {
    let rig = Rig::new("ended-deferred");
    let (runner, _) = switching(&rig, |toward| match toward {
        Toward::Target => Err(ended(
            75,
            r#"{"class":"environment","reason":"apt_busy","retry_after_secs":100}"#,
        )),
        _ => Ok(()),
    });
    let got = runner.run(&rig.order()).expect("the update ends");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(100)
            },
            reason: Some("apt_busy".to_owned()),
        }
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);
}

/// A switch that ends the step with no code (killed) is a failure like a hook that was killed.
#[test]
fn a_switch_that_ends_cutover_without_a_code_fails_the_step() {
    let rig = Rig::new("ended-no-code");
    let (runner, _) = switching(&rig, |toward| match toward {
        Toward::Target => Err(SwitchError::Ended(StepExit {
            code: None,
            last_line: None,
        })),
        _ => Ok(()),
    });
    let got = runner.run(&rig.order()).expect("the update ends");
    assert!(matches!(got.verdict, Verdict::Failed { .. }), "{got:?}");
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);
}

/// A rollback that the switch ended follows the rollback rows: exit 1 before the commit is a
/// halt, and the hook of the rollback is not launched.
#[test]
fn a_switch_that_ends_the_rollback_follows_the_rollback_rows() {
    let rig = Rig::new("ended-rollback");
    rig.fake.say(Step::Cutover, &[FAIL]);
    let (runner, seen) = switching(&rig, |toward| match toward {
        Toward::Back => Err(ended(1, r#"{"reason":"dpkg_error"}"#)),
        _ => Ok(()),
    });
    let got = runner.run(&rig.order()).expect("the update ends");
    assert_eq!(got.verdict, Verdict::Halt);
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Cutover]);
    assert_eq!(*seen.lock().unwrap(), [Toward::Target, Toward::Back]);
}

/// An `Ended` from the confirming call is a failed switch: `RunError::Switch` with the text,
/// no verdict written, the record still at `cleanup` done.
#[test]
fn an_ended_confirmation_stops_the_runner_and_writes_no_verdict() {
    let rig = Rig::new("ended-confirmed");
    let (runner, seen) = switching(&rig, |toward| match toward {
        Toward::Confirmed => Err(ended(1, "no")),
        _ => Ok(()),
    });
    let error = runner.run(&rig.order()).expect_err("the switch ends");
    assert!(
        matches!(&error, RunError::Switch(why)
            if why == "the step ended in the switch: exit Some(1), last line Some(\"no\")"),
        "{error:?}"
    );
    assert_eq!(seen.lock().unwrap().last(), Some(&Toward::Confirmed));
    let Ok(Loaded::Current(rec)) = rig.store().load() else {
        panic!("no record");
    };
    assert_eq!(rec.step, Step::Cleanup);
    assert!(matches!(rec.state, State::Done { .. }), "{:?}", rec.state);
}

/// A `Failed` switch keeps its behaviour: the runner stops with `RunError::Switch(why)`, the
/// step stays pending and the cutover hook did not run.
#[test]
fn a_failed_switch_still_stops_the_runner_with_its_text() {
    let rig = Rig::new("ended-failed");
    let (runner, _) = switching(&rig, |_| Err(SwitchError::Failed("no link".to_owned())));
    let error = runner.run(&rig.order()).expect_err("the switch fails");
    assert!(
        matches!(&error, RunError::Switch(why) if why == "no link"),
        "{error:?}"
    );
    assert_eq!(rig.fake.steps(), [Step::Prepare]);
    let Ok(Loaded::Current(rec)) = rig.store().load() else {
        panic!("no record");
    };
    assert_eq!((rec.step, rec.state), (Step::Cutover, State::Pending));
}

/// A `String` is a `Failed`, and the error prints as its text.
#[test]
fn a_string_is_a_failed_switch() {
    let error = SwitchError::from("why".to_owned());
    assert_eq!(error, SwitchError::Failed("why".to_owned()));
    assert_eq!(error.to_string(), "why");
}

type Calls = Seen<(Step, String, Bound)>;

/// A runner of `rig` with a builtin port that records its calls and answers `exit(step)`.
fn building(
    rig: &Rig,
    exit: impl Fn(Step) -> StepExit + Send + Sync + 'static,
) -> (StepRunner, Calls) {
    let calls = Calls::default();
    let log = Arc::clone(&calls);
    let builtin: Builtin = Arc::new(move |step: Step, order: &UpdateOrder, bound: Bound| {
        log.lock()
            .expect("the log")
            .push((step, order.service.clone(), bound));
        exit(step)
    });
    (rig.runner().with_builtin(builtin), calls)
}

fn builtin_order(rig: &Rig) -> UpdateOrder {
    let mut order = rig.order();
    order.builtin_steps = true;
    order.hook_binary = PathBuf::new();
    order
}

/// Built-in steps with no hook program run through the port: each step of the update is called
/// with its step, the order and its bound, nothing is launched, and the update succeeds.
#[test]
fn builtin_steps_without_a_hook_program_run_through_the_port() {
    let rig = Rig::new("builtin-port");
    let (runner, calls) = building(&rig, |_| BUILTIN_EXIT);
    let got = runner.run(&builtin_order(&rig)).expect("the update ends");
    assert_eq!(
        got.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    let seen = calls.lock().unwrap().clone();
    let steps: Vec<Step> = seen.iter().map(|call| call.0).collect();
    assert_eq!(
        steps,
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert!(seen.iter().all(|call| call.1 == "svc"));
    assert_eq!(
        seen[1].2,
        Bound::Secs(std::num::NonZeroU64::new(Rig::bounds().cutover_secs).unwrap())
    );
    assert!(rig.fake.events().is_empty());
}

/// Built-in steps with no port end as before: exit 0, nothing launched, even with no hook program.
#[test]
fn builtin_steps_without_a_port_still_exit_zero() {
    let rig = Rig::new("builtin-no-port");
    let got = rig
        .runner()
        .run(&builtin_order(&rig))
        .expect("the update ends");
    assert!(matches!(got.verdict, Verdict::Succeeded { .. }), "{got:?}");
    assert!(rig.fake.events().is_empty());
}

/// A port that fails `cutover` makes the update roll back: the rollback is a built-in step too.
#[test]
fn a_builtin_that_fails_cutover_rolls_back() {
    let rig = Rig::new("builtin-fails");
    let (runner, calls) = building(&rig, |step| match step {
        Step::Cutover => StepExit {
            code: Some(1),
            last_line: Some(r#"{"class":"release","reason":"units_down"}"#.to_owned()),
        },
        _ => BUILTIN_EXIT,
    });
    let got = runner.run(&builtin_order(&rig)).expect("the update ends");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class: Class::Release,
            },
            reason: Some("units_down".to_owned()),
        }
    );
    let steps: Vec<Step> = calls.lock().unwrap().iter().map(|call| call.0).collect();
    assert_eq!(steps, [Step::Prepare, Step::Cutover, Step::Rollback]);
}

/// Built-in steps check the tree, not a hook: a release changed after sealing halts the update
/// with `artifact_changed` and the port is never called.
#[test]
fn builtin_steps_check_the_tree_before_each_step() {
    let rig = Rig::new("builtin-changed");
    let order = builtin_order(&rig);
    std::fs::write(rig.new_dir.join("lib/data"), "changed").unwrap();
    let (runner, calls) = building(&rig, |_| BUILTIN_EXIT);
    let got = runner.run(&order).expect("the update ends");
    assert_eq!(
        got,
        Finished {
            verdict: Verdict::Halt,
            reason: Some("artifact_changed".to_owned()),
        }
    );
    assert!(calls.lock().unwrap().is_empty());
}

/// A hook run is not affected by a port: with `builtin_steps` off the hook launches and the port
/// is not called.
#[test]
fn a_port_is_not_called_for_hook_steps() {
    let rig = Rig::new("builtin-off");
    rig.fake.say(Step::Prepare, &[Reply::Exit(0, None)]);
    let (runner, calls) = building(&rig, |_| BUILTIN_EXIT);
    let got = runner.run(&rig.order()).expect("the update ends");
    assert!(matches!(got.verdict, Verdict::Succeeded { .. }), "{got:?}");
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(rig.fake.steps().first(), Some(&Step::Prepare));
}
