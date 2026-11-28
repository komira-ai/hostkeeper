//! The verify window: how much of it is spent and left, the deferrals of `verify` in it
//! ([`Progress::deferrals`]), and how a window that ends with the hook still deferring is
//! reported.
//!
//! A `verify` that defers (exit `75`) is run again after its wait until `verify_deadline_secs`
//! after `cutover` succeeded. Each deferral is counted in the step record, with the class, the
//! reason and the detail of its line, and written before the wait, so that the heartbeat shows
//! what the host waits for while it waits. The window ends with the hook still deferring when a
//! deferral leaves no time for another run, or when the last run is stopped by the window
//! (its bound is what the window has left) or not started at all because nothing is left. Such a
//! window is reported as `Failed` of the class of the last deferral, with its reason
//! ([`REASON_VERIFY_DEADLINE`] when it gave none) and the detail
//! `verify deferred N times over S seconds; last reason: R; last detail: D`: never the class
//! `release` of a run the window cut off, which wrote no line.

use std::num::NonZeroU64;
use std::sync::PoisonError;

use hostkeeper_hook::{Class, Next, Outcome, Phase, Report, Step, Verdict};

use super::{DEFAULT_RETRY_SECS, RunError, StepRunner, UpdateOrder};
use crate::clock::Stamp;
use crate::launcher::Bound;
use crate::order_util::wait_secs;
use crate::record::{Deferrals, HookDetail, Progress, Record, State};

/// The reason of the `Failed` of a verify window that ended with the hook deferring, when its
/// last deferral gave no reason of its own.
pub const REASON_VERIFY_DEADLINE: &str = "verify_deadline";

/// A verify window that ended while `verify` was deferring: the deferrals, and the seconds the
/// window ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Ended {
    deferrals: Deferrals,
    spent_secs: u64,
}

impl Ended {
    /// `rollback`, then `Failed` of the class of the last deferral: in the phase `gate_release` or
    /// `gate_environment` by that class, `after_commit` once a step committed.
    pub(super) fn rollback(&self, committed: bool) -> Next {
        let class = self.deferrals.class;
        let phase = match (committed, class) {
            (true, _) => Phase::AfterCommit,
            (false, Class::Release) => Phase::GateRelease,
            (false, Class::Environment) => Phase::GateEnvironment,
        };
        Next::Rollback {
            then: Verdict::Failed { phase, class },
        }
    }

    /// The reason the window's `Failed` is reported with: the last deferral's, or
    /// [`REASON_VERIFY_DEADLINE`].
    fn reason(&self) -> String {
        self.deferrals
            .reason
            .clone()
            .unwrap_or_else(|| REASON_VERIFY_DEADLINE.to_owned())
    }

    /// The detail the window's `Failed` is reported with, in words: how many deferrals over how
    /// many seconds, and the last one's reason and detail (`none` for one it did not give).
    pub(super) fn text(&self) -> String {
        let Deferrals {
            count,
            reason,
            detail,
            ..
        } = &self.deferrals;
        let times = match count {
            1 => "time",
            _ => "times",
        };
        format!(
            "verify deferred {count} {times} over {} seconds; last reason: {}; last detail: {}",
            self.spent_secs,
            reason.as_deref().unwrap_or("none"),
            detail.as_deref().unwrap_or("none"),
        )
    }

    /// Makes the window's reason and detail the ones `progress` holds for the `rollback` that
    /// follows: the reason the verdict is reported with, and the detail kept under it.
    pub(super) fn keep(&self, progress: &mut Progress) {
        let reason = self.reason();
        progress.reason = Some(reason.clone());
        progress.detail = Some(HookDetail {
            reason: Some(reason),
            text: self.text(),
        });
    }
}

/// The reason a verdict reached after `step` ended as `outcome` is reported with, `said` the
/// reason of that step's line: for a `rollback` that succeeded, the failure's that sent the
/// update to it ([`Progress::restore_cause`]), or else its own; for any other step its own, or
/// else the one `progress` holds.
pub(super) fn verdict_reason(
    step: Step,
    outcome: Outcome,
    said: Option<String>,
    progress: &mut Progress,
) -> Option<String> {
    match (step, outcome) {
        (Step::Rollback, Outcome::Success) => progress.restore_cause().or(said),
        _ => said.or(progress.reason.clone()),
    }
}

impl StepRunner {
    /// The bound of the step about to run: its own, and for `verify` before the update has
    /// succeeded no more than the time left of the verify window. `None` when nothing is left.
    pub(super) fn bound(&self, order: &UpdateOrder, rec: &Record) -> Option<Bound> {
        let own = order.bounds.bound(rec.step)?;
        match (&rec.progress.verify_started, own) {
            (Some(began), Bound::Secs(secs)) if rec.step == Step::Verify => {
                let spent = self.spent(began);
                let left = order.bounds.verify_deadline_secs.saturating_sub(spent);
                NonZeroU64::new(secs.get().min(left)).map(Bound::Secs)
            }
            _ => Some(own),
        }
    }

    /// The seconds spent of the window begun at `began`: on the clock when it is of this boot
    /// ([`Stamp::elapsed_secs`]). One that may be of another boot, its boot id or the clock's
    /// being another or unknown, begins again when this runner first meets it, and from then is
    /// measured on the monotonic clock of this process (which no boot outlives): a boot id that
    /// stays unreadable restarts the window once, never at every read.
    pub(super) fn spent(&self, began: &Stamp) -> u64 {
        if let Some(secs) = began.elapsed_secs(&*self.clock) {
            return secs;
        }
        let now = self.clock.mono_secs();
        let mut met = self.met.lock().unwrap_or_else(PoisonError::into_inner);
        let first = *met
            .entry((began.boot_id.clone(), began.mono_secs))
            .or_insert(now);
        now.saturating_sub(first)
    }

    /// Whether another run of `verify` after the wait would fall outside the verify window.
    pub(super) fn verify_window_passed(
        &self,
        order: &UpdateOrder,
        rec: &Record,
        report: &Report,
    ) -> bool {
        let Some(began) = &rec.progress.verify_started else {
            return false;
        };
        let wait = wait_secs(report.retry_after_secs, DEFAULT_RETRY_SECS);
        let spent = self.spent(began);
        spent.saturating_add(wait) >= order.bounds.verify_deadline_secs
    }

    /// After a run of `verify` in the verify window ended as `outcome` (`code` its exit code,
    /// `report` its line): a deferral counted in `rec` ([`Deferrals::after`]; once per run, so a
    /// run read again after a restart is not counted twice), and the window, when it ended with
    /// the hook deferring: a deferral that leaves no time for another run (`passed`, as
    /// [`StepRunner::verify_window_passed`] says), or a run with no exit code (stopped by its
    /// bound, or not started) once the window is spent, after at least one deferral. `None` for
    /// any other step or ending: a `verify` that fails or halts of its own is reported as it
    /// says.
    pub(super) fn window_ended(
        &self,
        order: &UpdateOrder,
        rec: &mut Record,
        exit: (Outcome, Option<i32>),
        report: &Report,
        passed: bool,
    ) -> Option<Ended> {
        let began = rec
            .progress
            .verify_started
            .clone()
            .filter(|_| rec.step == Step::Verify)?;
        let (outcome, code) = exit;
        let attempt = rec.attempt;
        let counted = rec
            .progress
            .deferrals
            .as_ref()
            .is_some_and(|deferrals| deferrals.attempt == attempt);
        if (outcome == Outcome::Deferred) & !counted {
            let before = rec.progress.deferrals.take();
            let now = Stamp::now(&*self.clock);
            rec.progress.deferrals = Some(Deferrals::after(before, report, now, attempt));
        }
        let spent_secs = self.spent(&began);
        let ended = match outcome {
            Outcome::Deferred => passed,
            Outcome::Failure => code.is_none() & (spent_secs >= order.bounds.verify_deadline_secs),
            Outcome::Success | Outcome::Halt => false,
        };
        let deferrals = rec.progress.deferrals.clone().filter(|_| ended)?;
        Some(Ended {
            deferrals,
            spent_secs,
        })
    }

    /// Writes `rec`, a `verify` that deferred, before its wait, so that the deferral is on disk
    /// for the heartbeat while the host waits. `true` when the write closed the update (a force
    /// flagged it, or an abort asked to end it): nothing more is done of it.
    pub(super) fn written_before_the_wait(&self, rec: &mut Record) -> Result<bool, RunError> {
        self.write(rec)?;
        Ok(matches!(rec.state, State::Finished { .. }))
    }
}
