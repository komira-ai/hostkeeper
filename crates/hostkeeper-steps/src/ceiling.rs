//! The updater's ceiling on a `prepare` whose manifest bound is 0.
//!
//! A manifest may declare `prepare = 0` so that the hook's drain waits for the service's work to
//! end however long it runs. A `prepare` that never returns would then keep the host `updating`
//! for ever, and the coordinator would keep the service's rollout live, with every other service's
//! rollout waiting behind it. So the runner gives such a `prepare` the bound
//! [`PREPARE_CEILING_SECS`] ([`crate::Bounds::bound`]): the dead-man stops it there, and the
//! unit's own bound (`RuntimeMaxSec=`, and the launchd and OpenRC launchers' bounds) ends it if the
//! dead-man is gone.
//!
//! A `prepare` of 0 that ended with no exit code at or after its ceiling was stopped by it, and
//! the runner reads it as the hook's deferral with the reason [`REASON_PREPARE_CEILING`]
//! ([`read`]): nothing of the release is at fault. As for any deferral of `prepare`, `rollback`
//! runs next and undoes the drain, the update ends `Deferred` with that reason, and the
//! coordinator offers the version again after its back-off. The service's running work is not
//! cut: stopping the step ends the hook's wait, not the service. A `prepare` with a bound of its
//! own keeps that bound, and is never read this way.

use std::num::NonZeroU64;

use hostkeeper_hook::{Outcome, Step};

use crate::launcher::StepExit;
use crate::runner::UpdateOrder;

/// The ceiling on a `prepare` whose manifest bound is 0: an hour. It is the longest wait a hook
/// can ask for between two runs of a step ([`crate::runner::MAX_RETRY_SECS`]).
pub const PREPARE_CEILING_SECS: u64 = 3600;

/// [`PREPARE_CEILING_SECS`] as the bound of a step.
pub(crate) const PREPARE_CEILING: NonZeroU64 =
    NonZeroU64::MIN.saturating_add(PREPARE_CEILING_SECS - 1);

/// The reason of the `Deferred` that ends an update whose `prepare` reached the ceiling.
pub const REASON_PREPARE_CEILING: &str = "prepare_ceiling";

/// What the runner reads of `exit`, the end of the step `step` of `order`; `reached` says that the
/// step ended at or after its deadline. A `prepare` whose manifest bound is 0 that ended there
/// with no exit code reads as exit `75` with the reason [`REASON_PREPARE_CEILING`]; every other
/// end reads as it is.
pub(crate) fn read(order: &UpdateOrder, step: Step, exit: StepExit, reached: bool) -> StepExit {
    let ceiling = step == Step::Prepare && order.bounds.prepare_secs == 0;
    match ceiling && reached && exit.code.is_none() {
        true => StepExit {
            code: Some(Outcome::Deferred.code()),
            last_line: Some(format!(r#"{{"reason":"{REASON_PREPARE_CEILING}"}}"#)),
        },
        false => exit,
    }
}
