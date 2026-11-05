//! `abort` (design sections 6.3 and 8.5): stop the service's running step and end its update
//! `Finished{Halt}`. A coordinator's order and the admin socket's `Abort` both come here
//! ([`crate::Daemon::abort`]). It takes neither the service's lock (the order it stops holds it)
//! nor the step lock, and it never waits for a step to end by itself.
//!
//! - **An order holds the service's lock** and its update is unfinished: the update's runner is
//!   asked to end it ([`hostkeeper_steps::StepRunner::abort`], under the record lock, as a
//!   `force` flags a record), so it launches nothing more, and when the record says a step is
//!   `running` that step's unit is stopped by name: tried [`ABORT_STOP_ATTEMPTS`] times, with
//!   [`ABORT_RETRY_SECS`] on the daemon's clock between tries. The runner then closes the update
//!   `Finished{Halt, aborted_by_operator}`, which is the verdict the order's own caller gets; the
//!   abort waits for the order to let go of the lock and answers with the record. With no step
//!   running (the runner is between two steps) the ask alone ends the update at the runner's
//!   next look. A unit that every try fails to stop closes the record
//!   `Finished{Halt, unstoppable}` at once, with an alert; the order keeps the lock until its step
//!   ends.
//! - **A step that could not be stopped holds the lock for good** (the runner stopped
//!   `Unstopped`, and the record still says `running`): the unit the record names is stopped with
//!   the same tries, the record is closed `Finished{Halt, aborted_by_operator}`, or `unstoppable`
//!   with an alert when every try failed, the params file is removed and the lock let go.
//! - **Otherwise** nothing runs: `nothing_running`, and nothing is written.
//!
//! Either way the record then says Halt, so the service's later orders are `halted` until
//! `clear_halt`. The farm's slot is the coordinator's, which lets go of it when it sees the
//! verdict.
//!
//! A stop that reaches the service manager in the instant between the runner's write of
//! `running` and the start of the unit finds no unit: the abort then waits for a step that runs
//! on. Another abort stops it, and ends both.

use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::LaunchError;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::runner::{REASON_ABORTED, REASON_UNSTOPPABLE};

use crate::boot::ALERT_UNSTOPPABLE;
use crate::checks;
use crate::executor::{Ctx, Refused, Reply};
use crate::refusal::Refusal;

/// How many times an abort tries to stop a unit before it gives up.
pub const ABORT_STOP_ATTEMPTS: u32 = 3;
/// The wait between two tries to stop a unit, on the daemon's clock.
pub const ABORT_RETRY_SECS: u64 = 2;

/// How an abort ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aborted {
    /// The unit the abort stopped, or tried to; `None` when no step was running.
    pub unit: Option<String>,
    /// The step record once the abort was over.
    pub record: Box<Record>,
}

/// The coordinator's `abort`: the update's verdict, from the record the abort left. A record
/// still unfinished (the runner stopped for another reason before it looked again) is `Running`:
/// a second abort ends it.
pub fn run(ctx: &Ctx, service: ServiceName) -> Reply {
    let verdict = |finished: hostkeeper_steps::Finished| Reply::Finished {
        verdict: finished.verdict,
        reason: finished.reason,
        resent: false,
    };
    abort(ctx, &service).map_or_else(Reply::from, |aborted| {
        checks::finished_of(&aborted.record).map_or(Reply::Running, verdict)
    })
}

/// Aborts the update of `service` that an order runs, or whose step could not be stopped.
pub fn abort(ctx: &Ctx, service: &ServiceName) -> Result<Aborted, Refused> {
    match ctx.locks.holder(service) {
        Some(held) if held.stuck => unstopped(ctx, service),
        Some(_) => running(ctx, service),
        None => Err(nothing_running(service)),
    }
}

/// An order holds the lock: ask its runner to end the update, stop the step that runs, wait for
/// the order to end.
fn running(ctx: &Ctx, service: &ServiceName) -> Result<Aborted, Refused> {
    // An order that has not reached its runner yet has no step to stop.
    let runner = ctx
        .made_runner(service)
        .ok_or_else(|| nothing_running(service))?;
    let asked = runner.abort().map_err(store_failed)?;
    let rec = asked.ok_or_else(|| nothing_running(service))?;
    let unit = matches!(rec.state, State::Running { .. }).then(|| rec.unit(rec.step));
    if let Some(running) = &unit
        && let Err(error) = stop(ctx, running)
    {
        // The runner stays in the step until its unit ends; the record says why now.
        let record = close(ctx, service, REASON_UNSTOPPABLE)?;
        alert(ctx, service, running, &error);
        return Ok(Aborted { unit, record });
    }
    // The order ends, or keeps the lock for good when its runner stopped `Unstopped` before it
    // looked at the ask again (the record then still says `running`, and a second abort ends it).
    ctx.locks.wait_let_go(service);
    let record = current(ctx, service)?;
    Ok(Aborted { unit, record })
}

/// A step that could not be stopped holds the lock: stop the unit its record names, close the
/// record, let go of the lock.
fn unstopped(ctx: &Ctx, service: &ServiceName) -> Result<Aborted, Refused> {
    let rec = current(ctx, service)?;
    // The lock is held for something the record does not name (the boot step): not this verb's.
    if !matches!(rec.state, State::Running { .. }) {
        return Err(nothing_running(service));
    }
    let unit = rec.unit(rec.step);
    let stopped = stop(ctx, &unit);
    let reason = match &stopped {
        Ok(()) => REASON_ABORTED,
        Err(_) => REASON_UNSTOPPABLE,
    };
    let record = close(ctx, service, reason)?;
    if let Err(error) = &stopped {
        alert(ctx, service, &unit, error);
    }
    ctx.store(service).remove_params().map_err(store_failed)?;
    ctx.locks.release_stuck(service);
    Ok(Aborted {
        unit: Some(unit),
        record,
    })
}

/// Stops `unit`, trying [`ABORT_STOP_ATTEMPTS`] times; the error of the last try.
fn stop(ctx: &Ctx, unit: &str) -> Result<(), LaunchError> {
    let mut tried = 1;
    loop {
        match ctx.launcher.stop(unit) {
            Ok(()) => return Ok(()),
            Err(error) if tried >= ABORT_STOP_ATTEMPTS => return Err(error),
            Err(_) => {
                ctx.clock.sleep_secs(ABORT_RETRY_SECS);
                tried += 1;
            }
        }
    }
}

/// The alert of a unit an abort could not stop: one line at error level.
fn alert(ctx: &Ctx, service: &ServiceName, unit: &str, error: &LaunchError) {
    let detail = format!("service={service} unit={unit}: an abort could not stop it: {error}");
    ctx.alert(ALERT_UNSTOPPABLE, &detail);
}

/// Closes the record `Finished{Halt, reason}`.
fn close(ctx: &Ctx, service: &ServiceName, reason: &str) -> Result<Box<Record>, Refused> {
    let closed = ctx.store(service).close_halted(reason);
    let closed = closed.map_err(store_failed)?;
    closed.map(Box::new).ok_or_else(no_record)
}

/// The service's step record, which a running or unstopped update has.
fn current(ctx: &Ctx, service: &ServiceName) -> Result<Box<Record>, Refused> {
    match ctx.store(service).load().map_err(store_failed)? {
        Loaded::Current(rec) => Ok(rec),
        Loaded::Absent | Loaded::Newer { .. } => Err(no_record()),
    }
}

fn nothing_running(service: &ServiceName) -> Refused {
    Refused::new(
        Refusal::NothingRunning,
        format!("no step of {service} is running"),
    )
}

fn no_record() -> Refused {
    Refused::new(
        Refusal::StoreFailed,
        "the step record of the aborted update is gone",
    )
}

fn store_failed(error: impl ToString) -> Refused {
    Refused::new(Refusal::StoreFailed, error)
}
