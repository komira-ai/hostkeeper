//! The ports the daemon gives the runner: its switch around the hook's steps and the code of the
//! built-in steps. The runner calls them ([`crate::StepRunner::with_switch`],
//! [`crate::StepRunner::with_builtin`]); this file holds only their types.

use std::sync::Arc;

use hostkeeper_hook::Step;

use crate::launcher::{Bound, StepExit};
use crate::runner::UpdateOrder;

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
