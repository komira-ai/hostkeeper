//! The loop: one line per change of state, and one sleep of `RETRY_SECS` on the monotonic clock
//! after every step, whatever it did.

use hostkeeper_enroll::{Enroller, NOT_ENROLLED, RETRY_SECS};

use crate::support::{HOST, T0, WEEK, World};

/// Runs `enroller`'s loop for `steps` steps, calling `before(n)` before step `n`, and returns
/// the lines it logged.
fn run(enroller: &mut Enroller, steps: usize, mut before: impl FnMut(usize)) -> Vec<String> {
    let mut lines = Vec::new();
    let mut step = 0;
    enroller.run_loop(&mut |line: &str| lines.push(line.to_owned()), &mut || {
        let go = step < steps;
        if go {
            before(step);
        }
        step += 1;
        go
    });
    lines
}

/// Catches a log line per retry, and a retry without a sleep or with a sleep of zero: the
/// loop must neither flood the journal nor spin.
#[test]
fn an_unenrolled_daemon_logs_once_and_retries_without_spinning() {
    let world = World::new("pacing");
    let mut enroller = world.open();
    let lines = run(&mut enroller, 50, |_| {});
    assert_eq!(lines, [NOT_ENROLLED]);
    assert_eq!(NOT_ENROLLED, "not enrolled; waiting for approval");
    assert_ne!(RETRY_SECS, 0);
    assert_eq!(world.sleeps(), [RETRY_SECS; 50]);
    let c = world.coordinator();
    assert_eq!(c.enrolls.len(), 1, "posted once");
    assert_eq!(c.polls.len(), 100, "two polls a step");
}

/// Catches a denial taken for success, or polled again without the pacing.
#[test]
fn a_denied_request_is_not_retried_faster_than_the_pacing() {
    let world = World::new("denied");
    let mut enroller = world.open();
    let lines = run(&mut enroller, 4, |step| {
        if step == 1 {
            world.coordinator().deny("operator_denied");
        }
    });
    assert_eq!(lines, [NOT_ENROLLED, "enrollment denied: operator_denied"]);
    assert_eq!(world.sleeps(), [RETRY_SECS; 4]);
    assert_eq!(world.current(), None);
    assert_eq!(world.coordinator().polls.len(), 8);
}

/// Catches a change of state that is not logged, or a quiet step that logs: the lines follow
/// the host from pending to enrolled, renewed, a coordinator that cannot be reached (logged once
/// for two steps), and renewed again.
#[test]
fn the_loop_logs_each_change_once() {
    let world = World::new("changes");
    let mut enroller = world.open();
    let lines = run(&mut enroller, 8, |step| match step {
        1 => world.coordinator().approve(HOST),
        3 => world.set_now(T0 + WEEK / 2),
        5 => {
            // The next renewal is due, and the coordinator is down for two steps.
            world.set_now(T0 + WEEK);
            world.coordinator().down = Some("connection refused".into());
        }
        7 => world.coordinator().down = None,
        _ => {}
    });
    assert_eq!(
        lines,
        [
            NOT_ENROLLED,
            "enrolled as host:web-1 (generation 1)",
            "renewed the certificate of host:web-1 (generation 2)",
            "the coordinator could not be reached: connection refused",
            "renewed the certificate of host:web-1 (generation 3)",
        ]
    );
    assert_eq!(world.sleeps(), [RETRY_SECS; 8]);
}
