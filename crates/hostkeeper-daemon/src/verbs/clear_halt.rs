//! `clear_halt` (design sections 5.4 and 6.2): lift the service's halt for a halt generation
//! strictly above the one the service is held under; and what each heartbeat reply's halt
//! generation of a service does to it ([`heard`], [`crate::link_adapters::DaemonHalts`]).
//!
//! The held generation is the service's `held-halt` file ([`crate::halt`]), which exists only
//! while its step record holds a halt. Every change here is under the service's lock, never
//! waited for.
//!
//! The `clear_halt` order: a service with an order under way is `busy`, and nothing changes. The
//! held generation is read (a missing file is 0; one that is not a number is `halt_unreadable`
//! and lifts nothing). A generation not above it is `stale_halt_generation`, and nothing is
//! written. A greater one lifts the halt ([`lift`]): a `Finished{Halt}` step record is turned into
//! the `halt_cleared` deferral ([`hostkeeper_steps::StepRunner::clear_halt`]; a service with no
//! halt record is fine), then the file is removed. Nothing is written for a service that holds no
//! halt.
//!
//! A reply's generation `g` of a service ([`heard`]), with `before`, the generation the reply
//! before it named (0 when it named none; unknown for the first reply since the daemon started),
//! judged from the step record and the file:
//! - no halt and no file: nothing; the generation is not written;
//! - no halt and a file: the file is left from a lift that did not finish, or from a daemon that
//!   wrote every generation it heard, and is removed;
//! - a halt and no file: the halt came after the reply before, so it is held under `before`, the
//!   last generation heard before it, as a daemon that wrote every generation held it: a `g`
//!   above `before` (a clear since) lifts it ([`lift`]), and any other writes `before` as the
//!   generation it is held under. With `before` unknown it is held under `g`: a clear it missed
//!   is not taken for one of this halt, and the next clear lifts it;
//! - a halt held under `h`: a `g` above `h` lifts it ([`lift`]); any other leaves it.
//!
//! A record or file that cannot be read changes nothing (the executor refuses the service's orders
//! under `record_unreadable` or `halt_unreadable`), and a service whose lock is held (an order
//! under way) is judged again at the next reply.

use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::Verdict;
use hostkeeper_steps::record::State;
use hostkeeper_steps::runner::REASON_HALT_CLEARED;
use hostkeeper_steps::{Loaded, RecordError, RunError, StepRunner};

use crate::executor::{Ctx, Refused, Reply};
use crate::locks::Holder;
use crate::refusal::Refusal;

/// A halt lifted by a `clear_halt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lifted {
    /// The generation that lifted it.
    pub generation: u64,
    /// A halt record was cleared; `false` when the service had none, and nothing was held.
    pub cleared: bool,
}

/// What a heartbeat reply's halt generation did to a service ([`heard`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// Nothing: the service holds no halt and has no file, its halt is held under this
    /// generation or a greater one, or its record or file cannot be read.
    Unchanged,
    /// The service's halt, held under no generation, is held under the one the reply before
    /// named from now on (under this one when no reply came before since the daemon started).
    Held,
    /// The service's halt was lifted, and its file removed.
    Lifted,
    /// The service holds no halt: the file left of one was removed.
    Removed,
    /// The service's lock is held (an order under way): the next reply judges it again.
    Busy,
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
    let cleared = lift(ctx, service)?;
    Ok(Lifted {
        generation,
        cleared,
    })
}

/// A heartbeat reply named `generation` as the halt generation of `service`, and the reply before
/// it `before` (`None` for the first reply since the daemon started): the halt it holds is held,
/// or lifted, and a file of no halt is removed (the module's list).
pub fn heard(
    ctx: &Ctx,
    service: &ServiceName,
    before: Option<u64>,
    generation: u64,
) -> Result<Heard, Refused> {
    let Ok(_guard) = ctx.locks.take_for(service, Holder::ClearHalt) else {
        return Ok(Heard::Busy);
    };
    let stored = |e| Refused::new(Refusal::StoreFailed, e);
    let judged = judge(ctx, service, before, generation);
    match judged {
        Heard::Held => ctx
            .halts
            .hold(service, before.unwrap_or(generation))
            .map_err(stored)?,
        Heard::Lifted => lift(ctx, service).map(drop)?,
        Heard::Removed => ctx.halts.remove(service).map_err(stored)?,
        Heard::Unchanged | Heard::Busy => {}
    }
    Ok(judged)
}

/// What `generation` asks of `service`, after `before`, from its step record and its file as
/// they are now: never [`Heard::Busy`].
fn judge(ctx: &Ctx, service: &ServiceName, before: Option<u64>, generation: u64) -> Heard {
    let halted = match ctx.store(service).load() {
        Ok(Loaded::Absent) => false,
        Ok(Loaded::Current(rec)) => matches!(
            rec.state,
            State::Finished {
                verdict: Verdict::Halt,
                ..
            }
        ),
        Ok(Loaded::Newer { .. }) | Err(_) => return Heard::Unchanged,
    };
    let Ok(held) = ctx.halts.read(service) else {
        return Heard::Unchanged;
    };
    match (halted, held) {
        (false, None) => Heard::Unchanged,
        (false, Some(_)) => Heard::Removed,
        (true, held) => match held.or(before) {
            Some(bar) if generation > bar => Heard::Lifted,
            _ if held.is_none() => Heard::Held,
            _ => Heard::Unchanged,
        },
    }
}

/// Lifts the halt of `service`, under its lock, which the caller holds: a `Finished{Halt}` step
/// record is turned into the `halt_cleared` deferral ([`clear_record`]), then the held-halt file is
/// removed. In that order: a daemon that dies between the two is left with a file of no halt,
/// which the next reply removes ([`heard`]); the other order would leave a halt held under no
/// generation, which the next reply could hold under a clear meant for it. `false` when the record held no
/// halt.
pub(crate) fn lift(ctx: &Ctx, service: &ServiceName) -> Result<bool, Refused> {
    let cleared = clear_record(ctx, service)?;
    ctx.halts
        .remove(service)
        .map_err(|e| Refused::new(Refusal::StoreFailed, e))?;
    Ok(cleared)
}

/// Turns the service's `Finished{Halt}` step record into the `halt_cleared` deferral; `false`
/// when the record holds no halt. Written with a runner of its own: the service's runner would
/// stop a post-success watcher first.
pub(crate) fn clear_record(ctx: &Ctx, service: &ServiceName) -> Result<bool, Refused> {
    let runner = StepRunner::new(
        Arc::clone(&ctx.launcher),
        Arc::clone(&ctx.clock),
        ctx.store(service),
    );
    runner
        .clear_halt()
        .map_err(|e| Refused::new(record_refusal(&e), e))
}

/// The token of a step record that could not be cleared.
fn record_refusal(error: &RunError) -> Refusal {
    match error {
        RunError::RecordNewer(_) => Refusal::RecordNewer,
        RunError::Record(RecordError::Corrupt(_)) => Refusal::RecordUnreadable,
        _ => Refusal::StoreFailed,
    }
}
