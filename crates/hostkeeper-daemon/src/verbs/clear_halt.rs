//! `clear_halt` (design sections 5.4 and 6.2): lift the service's halt for a halt generation
//! strictly above the one the service is held under. The coordinator's: a `clear_halt` order,
//! and the halt generations of its heartbeat replies ([`crate::link_adapters::DaemonHalts`]).
//!
//! Under the service's lock, never waited for (a service with an order under way is `busy`, and
//! nothing changes): the held generation is read (a missing file is 0; one that is not a number
//! is `halt_unreadable` and lifts nothing). A generation not above it is `stale_halt_generation`,
//! and nothing is written. A greater one first turns a `Finished{Halt}` step record into the
//! `halt_cleared` deferral ([`hostkeeper_steps::StepRunner::clear_halt`]; a service with no halt
//! record is fine), then raises the held generation. In that order: a daemon that dies between
//! the two still holds the old generation, so the same `clear_halt` sent again lifts it; the other
//! order would leave a halt record that no `clear_halt` of that generation could lift. Nothing
//! lowers the held generation.

use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::Verdict;
use hostkeeper_steps::runner::REASON_HALT_CLEARED;
use hostkeeper_steps::{RecordError, RunError, StepRunner};

use crate::executor::{Ctx, Refused, Reply};
use crate::locks::Holder;
use crate::refusal::Refusal;

/// A halt generation the service is now held under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lifted {
    /// The generation now held.
    pub generation: u64,
    /// A halt record was cleared; `false` when the service had none, and only the generation was
    /// raised.
    pub cleared: bool,
}

/// The coordinator's `clear_halt` order: `Finished{Deferred, halt_cleared}` once the halt is
/// lifted, the record's own word for it.
pub fn run(ctx: &Ctx, service: ServiceName, halt_generation: u64) -> Reply {
    let lifted = |_| Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_HALT_CLEARED.to_owned()),
        resent: false,
    };
    clear_halt(ctx, &service, halt_generation).map_or_else(Reply::from, lifted)
}

/// Lifts the halt of `service` for `generation`.
pub fn clear_halt(ctx: &Ctx, service: &ServiceName, generation: u64) -> Result<Lifted, Refused> {
    let _guard = ctx
        .locks
        .take_for(service, Holder::ClearHalt)
        .map_err(|held| {
            Refused::new(
                Refusal::Busy,
                format!("{service} is locked by {}", held.holder),
            )
        })?;
    let held = ctx
        .halts
        .get(service)
        .map_err(|e| Refused::new(Refusal::HaltUnreadable, e))?;
    if generation <= held {
        return Err(Refused::new(
            Refusal::StaleHaltGeneration,
            format!(
                "{service} is held under halt generation {held}; {generation} does not lift it"
            ),
        ));
    }
    // A runner of its own: the service's runner would stop a post-success watcher first.
    let runner = StepRunner::new(
        Arc::clone(&ctx.launcher),
        Arc::clone(&ctx.clock),
        ctx.store(service),
    );
    let cleared = runner
        .clear_halt()
        .map_err(|e| Refused::new(record_refusal(&e), e))?;
    ctx.halts
        .raise(service, generation)
        .map_err(|e| Refused::new(Refusal::StoreFailed, e))?;
    Ok(Lifted {
        generation,
        cleared,
    })
}

/// The token of a step record that could not be cleared.
fn record_refusal(error: &RunError) -> Refusal {
    match error {
        RunError::RecordNewer(_) => Refusal::RecordNewer,
        RunError::Record(RecordError::Corrupt(_)) => Refusal::RecordUnreadable,
        _ => Refusal::StoreFailed,
    }
}
