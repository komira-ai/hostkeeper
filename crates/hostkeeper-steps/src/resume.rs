//! The resume table of the daemon's start (docs/design/hostkeeper-design.md, section 6.5): what
//! becomes of an update the step record holds unfinished, after the daemon died and after the
//! host rebooted.
//!
//! | Record | Daemon died, host did not reboot | Host rebooted (boot id changed) |
//! |---|---|---|
//! | `prepare` pending or running | stop the unit by name, run `prepare` again | run `rollback`, then report Deferred `rebooted` |
//! | `cutover` pending or running, nothing committed | stop the unit, run `cutover` again | run `rollback` before any unit starts, then Failed `before_commit` |
//! | `cutover` or later, a step has committed | run the step again | restart of the new release; continue with the next step |
//! | `verify` running | run `verify` again | nothing committed: `rollback`, Failed `gate_release`; committed: continue `verify` with a fresh window |
//! | `rollback` or `cleanup` running | run it again | run it again |
//! | `done` | recompute the next step | recompute the next step |
//!
//! [`resume_action`] is the table, a pure function of the record and whether the host rebooted.
//! [`resume`] rewrites a record for its action, so that [`crate::StepRunner::run`] then does what
//! the cell says: the runner already stops the unit of a step found `running` and runs it again,
//! and decides what follows a step found `done`; a cell it does not do by itself is written into
//! the record here (a step cut off by the reboot is read as killed, or moved to the step the cell
//! names). The caller writes a changed record before anything runs.

use hostkeeper_hook::{Phase, Step, Verdict};

use crate::clock::{Clock, Stamp};
use crate::record::{HookSel, Record, State};

/// The reason of the `Deferred` an update reports when a reboot cut off its `prepare`.
pub const REASON_REBOOTED: &str = "rebooted";

/// What the start pass does with an update the step record holds: one cell of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAction {
    /// Stop the unit of the step by name, then run the step again.
    StopUnitAndRerun,
    /// Run `rollback`, then report `Deferred` with this reason.
    RollbackThenDeferred { reason: &'static str },
    /// Run `rollback`, then report `Failed` in this phase.
    RollbackThenFailed { phase: Phase },
    /// The reboot restarted the new release, so the step it cut off is over: continue with the
    /// next step, `verify`, its window begun now.
    RestartNewReleaseContinueNext,
    /// Run `verify` again, its window begun now.
    ContinueVerifyFreshWindow,
    /// Run the step: it had not started, or its unit ended with the host.
    RerunStep,
    /// The step ended: what follows is decided from how it ended.
    RecomputeNext,
    /// The update is over: there is nothing to resume.
    NothingFinished,
    /// The post-success watcher runs again (section 6.4); after a reboot its ceiling begins now.
    WatcherRestart,
}

/// The cell of the table for `rec`: `rebooted` is whether the host booted since the step was
/// cut off.
pub fn resume_action(rec: &Record, rebooted: bool) -> ResumeAction {
    use ResumeAction::{
        ContinueVerifyFreshWindow, NothingFinished, RecomputeNext, RerunStep,
        RestartNewReleaseContinueNext, RollbackThenDeferred, RollbackThenFailed, StopUnitAndRerun,
        WatcherRestart,
    };
    let watching = rec.progress.watching.is_some();
    let running = match (&rec.state, watching) {
        // Not a row of the table: the update has its verdict.
        (State::Finished { .. }, _) => return NothingFinished,
        // Not a row of the table either: the post-success watcher is outside it, and it never
        // runs `rollback` (section 6.4). "Then ... watchers restart."
        (_, true) => return WatcherRestart,
        // | `done` | recompute the next step | recompute the next step |
        (State::Done { .. }, false) => return RecomputeNext,
        (State::Pending, false) => false,
        (State::Running { .. }, false) => true,
    };
    // In the column "daemon died", a step found `running` has its unit stopped by name before it
    // runs again; one found `pending` never started a unit.
    let again = match running {
        true => StopUnitAndRerun,
        false => RerunStep,
    };
    match (rec.step, rec.progress.committed, rebooted) {
        // | `prepare` pending or running | stop the unit by name, run `prepare` again | run
        // `rollback`, then report Deferred `rebooted` |
        // (`prepare` is the first step: nothing can have committed before it.)
        (Step::Prepare, _, false) => again,
        (Step::Prepare, _, true) => RollbackThenDeferred {
            reason: REASON_REBOOTED,
        },
        // | `cutover` pending or running, nothing committed | stop the unit, run `cutover`
        // again | run `rollback` before any unit starts, then Failed `before_commit` |
        (Step::Cutover, false, false) => again,
        (Step::Cutover, false, true) => RollbackThenFailed {
            phase: Phase::BeforeCommit,
        },
        // | `cutover` or later, a step has committed | run the step again | restart of the new
        // release; continue with the next step |
        // (The later steps have rows of their own below, which this row leaves to them.)
        (Step::Cutover, true, false) => again,
        (Step::Cutover, true, true) => RestartNewReleaseContinueNext,
        // | `verify` running | run `verify` again | nothing committed: `rollback`, Failed
        // `gate_release`; committed: continue `verify` with a fresh window |
        // (A `verify` pending between two runs is read the same: before the commit a reboot is
        // an abort, section 8.3.)
        (Step::Verify, _, false) => again,
        (Step::Verify, false, true) => RollbackThenFailed {
            phase: Phase::GateRelease,
        },
        (Step::Verify, true, true) => ContinueVerifyFreshWindow,
        // | `rollback` or `cleanup` running | run it again | run it again |
        // (After a reboot its unit ended with the host: nothing is left to stop.)
        // `uninstall` is no step of an update and no record of one holds it; were one to, it is
        // read as these two are: run again.
        (Step::Rollback | Step::Cleanup | Step::Uninstall, _, false) => again,
        (Step::Rollback | Step::Cleanup | Step::Uninstall, _, true) => RerunStep,
    }
}

/// Reads the record of an unfinished update at the daemon's start and rewrites it for its cell of
/// the table. `rebooted` is whether the host booted since the last start pass that ended; a step
/// the record holds as running that started in this boot was not cut off by the boot (a start
/// after it ran the step and was itself stopped before it ended), and is read as after a death of
/// the daemon. A window or a ceiling begun in another boot begins now: the time that passed
/// before the boot is unknown, and a stamp of another boot never runs out in this one
/// ([`Stamp::elapsed_secs`]).
///
/// Returns the cell's action and whether the record changed: a changed record must be written
/// before anything of the update runs. A finished record is never changed.
pub fn resume(rec: &mut Record, rebooted: bool, clock: &dyn Clock) -> (ResumeAction, bool) {
    let now = Stamp::now(clock);
    let started_now = matches!(
        &rec.state,
        State::Running { started, .. } if started.boot_id == now.boot_id
    );
    let action = resume_action(rec, rebooted & !started_now);
    if action == ResumeAction::NothingFinished {
        return (action, false);
    }
    let before = rec.clone();
    restamp(&mut rec.progress.verify_started, &now);
    restamp(&mut rec.progress.watching, &now);
    match action {
        ResumeAction::RollbackThenDeferred { reason } => {
            move_to_rollback(rec);
            rec.progress.then = Some(Verdict::Deferred {
                retry_after_secs: None,
            });
            rec.progress.reason = Some(reason.to_owned());
        }
        // The runner reads a step that ended with no exit code as killed: after `cutover` and
        // before the commit that is `rollback`, then `Failed(before_commit)`, and after
        // `verify` it is `rollback`, then `Failed(gate_release)` (the table of the contract).
        ResumeAction::RollbackThenFailed { .. } => read_as_killed(rec),
        ResumeAction::RestartNewReleaseContinueNext => {
            rec.step = Step::Verify;
            rec.attempt = 1;
            rec.state = State::Pending;
            rec.progress.retry = 0;
            rec.progress.verify_started = Some(now);
        }
        ResumeAction::ContinueVerifyFreshWindow => {
            rec.progress.verify_started = Some(now);
            restart(rec);
        }
        ResumeAction::RerunStep => restart(rec),
        ResumeAction::StopUnitAndRerun
        | ResumeAction::RecomputeNext
        | ResumeAction::NothingFinished
        | ResumeAction::WatcherRestart => {}
    }
    let changed = *rec != before;
    (action, changed)
}

/// The step ended with no exit code and no last line, as a step killed by the reboot.
fn read_as_killed(rec: &mut Record) {
    rec.state = State::Done {
        code: None,
        last_line: None,
    };
}

/// `rollback` from the new release is the next step, its retries not yet used.
fn move_to_rollback(rec: &mut Record) {
    rec.step = Step::Rollback;
    rec.attempt = 1;
    rec.state = State::Pending;
    rec.hook = HookSel::New;
    rec.progress.retry = 0;
}

/// A step whose unit ended with the host runs again as its next attempt (a count that has
/// reached its largest value stays there); a pending one is left.
fn restart(rec: &mut Record) {
    if let State::Running { .. } = rec.state {
        rec.attempt = rec.attempt.saturating_add(1);
        rec.state = State::Pending;
    }
}

/// A stamp of another boot becomes `now`.
fn restamp(stamp: &mut Option<Stamp>, now: &Stamp) {
    if let Some(stale) = stamp.as_mut().filter(|stamp| stamp.boot_id != now.boot_id) {
        *stale = now.clone();
    }
}
