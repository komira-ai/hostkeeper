//! The ports the daemon gives the runner: its switch around the hook's steps and the code of the
//! built-in steps. The runner calls them ([`crate::StepRunner::with_switch`],
//! [`crate::StepRunner::with_builtin`]); this file holds only their types.

use std::sync::Arc;

use hostkeeper_hook::{Outcome, Report, Step};

use crate::launcher::{Bound, StepExit};
use crate::runner::{REASON_NO_PREVIOUS_RELEASE, UpdateOrder};

/// Which way the daemon switches a service around a step of the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// To the target, immediately before `cutover`.
    Target,
    /// Back, immediately before `rollback`.
    Back,
    /// After a `cleanup` that succeeded, before the verdict is written: the daemon's switch does
    /// nothing here, as no installer places anything for it to commit. It is called again by a
    /// run that finds the `cleanup` done, so a switch that does something must be idempotent.
    Confirmed,
}

/// Why a switch did not return.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SwitchError {
    /// The switch could not be done: nothing of the step ran and it stays pending; the runner
    /// stops with [`crate::RunError::Switch`].
    #[error("{0}")]
    Failed(String),
    /// The switch's installer half ran and failed: the step ends with this exit, as if its hook
    /// had exited so, and the hook is not run. Not for [`Toward::Confirmed`], where it is read as
    /// [`SwitchError::Failed`] with this text.
    #[error("the step ended in the switch: exit {:?}, last line {:?}", .0.code, .0.last_line)]
    Ended(StepExit),
}

impl From<String> for SwitchError {
    fn from(why: String) -> SwitchError {
        SwitchError::Failed(why)
    }
}

/// The daemon's switch of `current` and `previous` around the hook's steps
/// ([`crate::StepRunner::with_switch`]): called with [`Toward::Target`] before each run of
/// `cutover`, [`Toward::Back`] before each run of `rollback` and [`Toward::Confirmed`] after a
/// `cleanup` that succeeded, with the order. [`SwitchError::Failed`] says why it could not
/// switch: the step does not run (for `Confirmed`, the verdict is not written), and the runner
/// stops with [`crate::RunError::Switch`]. [`SwitchError::Ended`] is the step's own end: the hook
/// is not run and the exit is read as the hook's.
pub type Switch = Arc<dyn Fn(Toward, &UpdateOrder) -> Result<(), SwitchError> + Send + Sync>;

/// The code of a built-in step ([`UpdateOrder::builtin_steps`]): called with the step, the order
/// and the step's bound instead of launching a command, and it gives the exit a hook would.
pub type Builtin = Arc<dyn Fn(Step, &UpdateOrder, Bound) -> StepExit + Send + Sync>;

/// The removal of a first install's package, after a run of `step` ended with `exit`. Only the hook's
/// `rollback` of a first install is followed by it (any other step, and an update, pass through):
/// the switch's `Back`, made after the hook so that the hook ran while the package and its units
/// were still on the host. It is made when the hook succeeded or declined the install
/// (`no_previous_release`); a hook that failed or deferred leaves the package in place, for its
/// retry or for whoever looks at the halt. A removal that ended the step (a package manager that
/// stayed busy) gives its exit in place of the hook's; one that failed is the `Err`, the step
/// still pending and run again (the hook's `rollback` is idempotent, as is the removal).
pub(crate) fn remove_after(
    switch: Option<&Switch>,
    order: &UpdateOrder,
    step: Step,
    exit: StepExit,
) -> Result<StepExit, String> {
    let Some(switch) = switch.filter(|_| (step == Step::Rollback) & order.previous.is_none())
    else {
        return Ok(exit);
    };
    let report = Report::read(exit.last_line.as_deref().unwrap_or(""));
    let declined = report.reason.as_deref() == Some(REASON_NO_PREVIOUS_RELEASE);
    match Outcome::from_exit(exit.code) {
        Outcome::Success => {}
        Outcome::Failure if declined => {}
        _ => return Ok(exit),
    }
    match switch(Toward::Back, order) {
        Ok(()) => Ok(exit),
        Err(SwitchError::Ended(ended)) => Ok(ended),
        Err(SwitchError::Failed(why)) => Err(why),
    }
}

/// The removal of a first install's package when the hook's `rollback` could not be started:
/// there is nothing to stop, so the switch's `Back` is made at once. `Some` is the exit a removal
/// that ended the step gave; `None` when it was made, or when this is not a first install's
/// `rollback`. A failed removal is the `Err`, the step still pending.
pub(crate) fn remove_unstarted(
    switch: Option<&Switch>,
    order: &UpdateOrder,
    step: Step,
) -> Result<Option<StepExit>, String> {
    let Some(switch) = switch.filter(|_| (step == Step::Rollback) & order.previous.is_none())
    else {
        return Ok(None);
    };
    match switch(Toward::Back, order) {
        Ok(()) => Ok(None),
        Err(SwitchError::Ended(ended)) => Ok(Some(ended)),
        Err(SwitchError::Failed(why)) => Err(why),
    }
}
