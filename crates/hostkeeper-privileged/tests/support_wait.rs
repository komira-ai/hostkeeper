//! The bounded wait of the privileged tests' support ([`support::wait_looks`], under
//! [`support::wait_until`] and [`support::wait_for_file`]), with no systemd and no root: a state
//! that never comes fails the test after the bound's looks, naming the bound and the line that
//! called the wait. Part of the pull-request set, so that a wait that looks for good again is
//! caught before the privileged run, where it would hang the job until its `timeout-minutes`.

mod support;

use std::panic::{AssertUnwindSafe, Location, catch_unwind};
use std::path::Path;
use std::time::Duration;

use hostkeeper_testkit::Scratch;
use support::{wait_for_file, wait_for_file_looks, wait_looks};

/// What a wait did: how many looks it made, and the message it failed with.
#[derive(Debug, PartialEq)]
struct Waited {
    looks: u32,
    failed: Option<String>,
}

/// The message of a panic caught by [`catch_unwind`].
fn message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// The message of a wait called at line `line` of this file that did not reach its state in
/// `looks` looks.
fn not_reached(line: u32, looks: u32) -> String {
    format!(
        "a wait for a state of systemd or the fixture at {}:{line}: not reached in {looks} looks",
        file!()
    )
}

/// Runs `wait_looks(looks, ZERO, ..)` over a state that holds from look `holds_at` on (never when
/// `None`), and gives what it did and the line it called the wait at.
fn wait(looks: u32, holds_at: Option<u32>) -> (Waited, u32) {
    let mut count = 0;
    let mut line = 0;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // The call of the wait opens on the next line.
        line = Location::caller().line() + 1;
        wait_looks(looks, Duration::ZERO, || {
            count += 1;
            holds_at.is_some_and(|at| count >= at)
        });
    }));
    let failed = outcome.err().map(|panic| message(&*panic));
    (
        Waited {
            looks: count,
            failed,
        },
        line,
    )
}

/// Catches a wait that looks for good, or stops early or late: a state that never comes fails
/// after exactly the bound's looks, with the bound and the calling line in the message.
#[test]
fn a_state_that_never_comes_fails_after_exactly_its_looks() {
    let (waited, line) = wait(3, None);
    assert_eq!(
        waited,
        Waited {
            looks: 3,
            failed: Some(not_reached(line, 3)),
        }
    );
}

/// The bound of zero looks fails at once, without looking.
#[test]
fn a_bound_of_no_looks_fails_without_looking() {
    let (waited, line) = wait(0, Some(1));
    assert_eq!(
        waited,
        Waited {
            looks: 0,
            failed: Some(not_reached(line, 0)),
        }
    );
}

/// A state that holds at the first look returns at once.
#[test]
fn a_state_that_already_holds_returns_at_the_first_look() {
    assert_eq!(
        wait(10, Some(1)).0,
        Waited {
            looks: 1,
            failed: None
        }
    );
}

/// A wait returns at the look the state holds at and looks no more.
#[test]
fn a_wait_returns_at_the_look_the_state_holds_at() {
    assert_eq!(
        wait(10, Some(4)).0,
        Waited {
            looks: 4,
            failed: None
        }
    );
}

/// Catches an off-by-one at the bound: a state that comes at the last look allowed passes.
#[test]
fn a_state_that_comes_at_the_last_look_passes() {
    assert_eq!(
        wait(5, Some(5)).0,
        Waited {
            looks: 5,
            failed: None
        }
    );
}

/// Catches a step that ended before it wrote its file being waited on for good: the wait fails at
/// its first look, naming the file.
#[test]
fn a_file_whose_step_ended_first_fails_at_once() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "support_wait_ended")
        .expect("scratch directory");
    let path = scratch.path().join("never-written");
    let asked = std::cell::Cell::new(0);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        wait_for_file(&path, || {
            asked.set(asked.get() + 1);
            true
        });
    }));
    assert_eq!(
        outcome.err().map(|panic| message(&*panic)),
        Some(format!("the step ended before it wrote {}", path.display()))
    );
    assert_eq!(asked.get(), 1);
}

/// Catches a file wait that looks for good, or whose failure points into the support module: a
/// file that never comes while the step runs fails after the bound's looks, naming the test's
/// line.
#[test]
fn a_file_that_never_comes_fails_after_its_looks_naming_the_line() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "support_wait_never")
        .expect("scratch directory");
    let path = scratch.path().join("never-written");
    let mut line = 0;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // The call of the wait opens on the next line.
        line = Location::caller().line() + 1;
        wait_for_file_looks(3, Duration::ZERO, &path, || false);
    }));
    assert_eq!(
        outcome.err().map(|panic| message(&*panic)),
        Some(not_reached(line, 3))
    );
}

/// A file that is there returns at once, whatever the step.
#[test]
fn a_file_that_is_there_returns_at_once() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "support_wait_there")
        .expect("scratch directory");
    let path = scratch.path().join("pid");
    std::fs::write(&path, "1").expect("write the file");
    wait_for_file_looks(1, Duration::ZERO, &path, || true);
}
