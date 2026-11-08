//! What ends a wait for a version, the limit of a held gRPC call, and how long a connection is
//! kept for the waits it holds. Every bound is on tokio's paused clock: a test races the bound
//! against a sleep just inside it and one just past it, so nothing depends on elapsed time.

use std::future::pending;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::WaitOutcome;
use tokio::time::sleep;

use super::{CallLimit, ConnectionWaits, outcome_of, unless};
use crate::converge::{Convergence, State};
use crate::testutil::version;

const PATIENCE: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(1);

fn convergence(desired: Option<&str>, state: State) -> Convergence {
    Convergence {
        picks: None,
        desired: desired.map(version),
        quiet: false,
        state,
        roles: Vec::new(),
        halt: None,
        hosts: None,
        commit: "unknown",
    }
}

/// Catches: a wait answered `CONVERGED` or `HALTED` for a version that is no longer desired, a
/// service with no desired version read as the caller's, a converged or halted state that keeps
/// the call waiting, and a blocked or in-progress one that ends it. The desired version is told
/// first, as the `hostkeeper ci` of older releases tells it.
#[test]
fn a_read_ends_the_wait_by_the_desired_version_first_then_by_the_state() {
    let table = [
        (Some("2.0"), State::Converged, Some(WaitOutcome::Converged)),
        (Some("2.0"), State::Halted, Some(WaitOutcome::Halted)),
        (Some("2.0"), State::Blocked, None),
        (Some("2.0"), State::InProgress, None),
        (Some("3.0"), State::Converged, Some(WaitOutcome::Superseded)),
        (Some("3.0"), State::Halted, Some(WaitOutcome::Superseded)),
        (
            Some("3.0"),
            State::InProgress,
            Some(WaitOutcome::Superseded),
        ),
        (
            Some("2.0+1"),
            State::Converged,
            Some(WaitOutcome::Superseded),
        ),
        (None, State::InProgress, Some(WaitOutcome::Superseded)),
    ];
    for (desired, state, want) in table {
        assert_eq!(
            outcome_of(&convergence(desired, state), "2.0"),
            want,
            "{desired:?} {state:?}"
        );
    }
}

/// Catches: a call cut before its patience, or one never cut: a call that is not held is
/// answered when it ends within one patience and cut when it would end after it.
#[tokio::test(start_paused = true)]
async fn a_call_not_held_is_cut_at_one_patience() {
    let limit = CallLimit::new(PATIENCE);
    assert_eq!(limit.bound(sleep(PATIENCE - TICK)).await, Some(()));
    let limit = CallLimit::new(PATIENCE);
    assert_eq!(limit.bound(sleep(PATIENCE + TICK)).await, None);
    let limit = CallLimit::new(PATIENCE);
    assert_eq!(limit.bound(pending::<()>()).await, None);
}

/// Catches: a held call cut at one patience (the bound every other call has), or a held call
/// with no bound at all: once the call is held for a wait, its limit is the wait plus one
/// patience from when it came in, however late in the call it was moved.
#[tokio::test(start_paused = true)]
async fn a_held_call_is_cut_at_its_wait_plus_one_patience() {
    let wait = Duration::from_secs(60);
    for (after_hold, answered) in [
        (wait + PATIENCE - Duration::from_secs(5) - TICK, true),
        (wait + PATIENCE - Duration::from_secs(5) + TICK, false),
    ] {
        let limit = CallLimit::new(PATIENCE);
        let held = limit.clone();
        let serving = async move {
            sleep(Duration::from_secs(5)).await;
            held.hold_for(wait);
            sleep(after_hold).await;
        };
        assert_eq!(
            limit.bound(serving).await.is_some(),
            answered,
            "{after_hold:?}"
        );
    }
}

/// Catches: a connection that holds no wait kept longer than one patience, or cut before it;
/// and a connection cut while a wait is held on it, or kept for good after its last wait ends.
/// It is kept until one patience after the last of its held waits ends, two of them here.
#[tokio::test(start_paused = true)]
async fn a_connection_is_kept_one_patience_after_its_last_held_wait() {
    let waits = ConnectionWaits::default();
    assert_eq!(
        unless(waits.quiet_for(PATIENCE), sleep(PATIENCE - TICK)).await,
        Some(())
    );
    assert_eq!(
        unless(waits.quiet_for(PATIENCE), sleep(PATIENCE + TICK)).await,
        None
    );

    for (after_last, kept) in [(PATIENCE - TICK, true), (PATIENCE + TICK, false)] {
        let waits = ConnectionWaits::default();
        let first = waits.hold();
        let second = waits.hold();
        let serving = async move {
            sleep(PATIENCE * 5).await;
            drop(first);
            sleep(PATIENCE * 5).await;
            drop(second);
            sleep(after_last).await;
        };
        assert_eq!(
            unless(waits.quiet_for(PATIENCE), serving).await.is_some(),
            kept,
            "{after_last:?}"
        );
    }
}

/// Catches: a wait taken after the connection went quiet not keeping it: the patience starts
/// again from the end of the later wait.
#[tokio::test(start_paused = true)]
async fn a_wait_taken_while_quiet_keeps_the_connection_again() {
    let waits = ConnectionWaits::default();
    let taken = waits.clone();
    let serving = async move {
        sleep(PATIENCE / 2).await;
        let hold = taken.hold();
        sleep(PATIENCE * 3).await;
        drop(hold);
        sleep(PATIENCE - TICK).await;
    };
    assert_eq!(unless(waits.quiet_for(PATIENCE), serving).await, Some(()));
}
