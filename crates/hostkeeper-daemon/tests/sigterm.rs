//! The daemon's SIGTERM (`hostkeeper_daemon::terminate::sigterm`), with the real signal: this
//! binary sends SIGTERM to itself once the signal is taken, and the future ends instead of the
//! process. It is its own test binary, so no other test shares the process's signal.

use hostkeeper_daemon::terminate::sigterm;
use hostkeeper_link::testing::RecordingLog;
use rustix::process::{Signal, getpid, kill_process};

/// Proves the future ends at a SIGTERM the process is sent, and nothing is logged.
///
/// Catches: SIGTERM left to its default action, which ends the daemon before its clean-shutdown
/// marker is judged.
#[test]
fn the_future_ends_at_a_sigterm() {
    let log = RecordingLog::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let terminated = sigterm(&log);
        kill_process(getpid(), Signal::TERM).expect("SIGTERM sent");
        terminated.await;
    });
    assert!(log.lines().is_empty());
}
