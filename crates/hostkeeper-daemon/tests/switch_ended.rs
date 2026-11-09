//! The retry around the daemon's switch (`place::retried`): a failed switch is tried again, and a
//! switch that ended its step (an installer's verdict) is returned at once.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::place::retried;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_hook::Params;
use hostkeeper_steps::Staged;
use hostkeeper_steps::{Bounds, Clock, StepExit, Switch, SwitchError, Toward, UpdateOrder};

fn order(dir: &Path) -> UpdateOrder {
    fs::write(dir.join("hook"), "#!hook\n").expect("a file");
    UpdateOrder {
        service: "web".to_owned(),
        target_version: "2.0.0".to_owned(),
        previous_version: None,
        artifact_digest: "ab".repeat(32),
        staged: Staged::seal(dir).expect("a sealed tree"),
        previous: None,
        hook_binary: "hook".into(),
        hook_args: Vec::new(),
        previous_hook: None,
        bounds: Bounds {
            prepare_secs: 1,
            cutover_secs: 1,
            verify_secs: 1,
            rollback_secs: 1,
            cleanup_secs: 1,
            verify_deadline_secs: 1,
            promotion_ceiling_secs: 1,
            cleanup_retries: 0,
        },
        declared_params: Params::default(),
        min_updater_protocol: None,
        order_try: None,
        builtin_steps: false,
    }
}

/// A switch that answers `answer(calls so far)` and a log of the lines `retried` wrote.
fn run(
    answer: impl Fn(u32) -> Result<(), SwitchError> + Send + Sync + 'static,
) -> (Result<(), SwitchError>, u32, u64, Vec<String>) {
    let scratch =
        hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "switch-ended")
            .expect("a scratch directory");
    let order = order(scratch.path());
    let calls = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&calls);
    let inner: Switch = Arc::new(move |_: Toward, _: &UpdateOrder| {
        answer(counted.fetch_add(1, Ordering::SeqCst) + 1)
    });
    let clock = FakeClock::new();
    let began = clock.mono_secs();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&lines);
    let switch = retried(
        inner,
        clock.clone(),
        Arc::new(move |line: &str| log.lock().expect("the log").push(line.to_owned())),
    );
    let got = switch(Toward::Target, &order);
    let slept = clock.mono_secs() - began;
    let lines = lines.lock().expect("the log").clone();
    (got, calls.load(Ordering::SeqCst), slept, lines)
}

/// An installer's verdict is not a transient failure: the switch is called once, nothing is
/// logged or waited for, and the `Ended` comes back as it was.
#[test]
fn an_ended_switch_is_returned_at_once_without_a_retry() {
    let exit = StepExit {
        code: Some(1),
        last_line: Some(r#"{"reason":"dpkg_error"}"#.to_owned()),
    };
    let ended = exit.clone();
    let (got, calls, slept, lines) = run(move |_| Err(SwitchError::Ended(ended.clone())));
    assert_eq!(got, Err(SwitchError::Ended(exit)));
    assert_eq!((calls, slept), (1, 0));
    assert!(lines.is_empty(), "{lines:?}");
}

/// A failure is still retried: one failure then success is two calls, one logged line and one
/// wait; a switch that always fails is tried to the last attempt and its last failure returned.
#[test]
fn a_failed_switch_is_still_retried_until_it_passes_or_the_last_try() {
    let (got, calls, slept, lines) = run(|call| match call {
        1 => Err(SwitchError::Failed("busy".to_owned())),
        _ => Ok(()),
    });
    assert_eq!((got, calls, slept), (Ok(()), 2, 2));
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("alert=switch_retried"), "{lines:?}");
    assert!(lines[0].ends_with(": busy"), "{lines:?}");

    let (got, calls, _, lines) = run(|call| Err(SwitchError::Failed(format!("no {call}"))));
    assert_eq!(got, Err(SwitchError::Failed("no 5".to_owned())));
    assert_eq!((calls, lines.len()), (5, 4));
}
