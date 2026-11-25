//! `maintain`: a host's turn of a maintenance run (docs/design/maintenance-updates.md, sections
//! 4.2, 5.1 to 5.4 and 5.7), run by the daemon's sequencer ([`crate::maintenance_run`]) over the
//! run record. The order is checked as it is decoded ([`crate::verb::Maintained`]: an order id of
//! 1 to 64 bytes, the action `RUN`, `RETRY` or `PUT_BACK`, a run id and 1 to 4 known kinds).
//!
//! A `RUN` starts the host's turn. Then, in this order:
//!
//! 1. **One sequencer.** While a sequencer runs a turn on the host, a resend of that turn's order
//!    is answered `Running` and starts nothing; any other `maintain` is refused `busy`.
//! 2. **The order the record holds.** A record that does not read is refused
//!    `record_unreadable`, one of a newer hostkeeper `record_newer`. An order the record already
//!    holds (its order id) is never started twice: a lower try is refused `stale_try`; the same
//!    or a higher try resumes the turn from where its record stands, or, for a turn that ended,
//!    answers its end again (`resent`).
//! 3. **One turn at a time.** Another order while the record's turn is unfinished, or halted
//!    (a halted host stays out of service until an operator acts on that turn), is refused
//!    `busy`.
//! 4. **No package step at once.** A new turn is refused `busy` while any package step is
//!    unfinished on the host: a service's lock held, its step record not finished, or its
//!    uninstall under way. The coordinator gives no such host a turn (`busy_elsewhere`); the
//!    daemon refuses one too.
//! 5. **The plan.** The services to drain are each installed service whose release names a
//!    `drain` and a `resume`, by name ([`crate::maintenance_run::launch::planned_services`]); a
//!    release that cannot be read is refused `release_unreadable`. The run's parameters are
//!    written beside the record, then the record of the new turn: a write that fails is refused
//!    `store_failed`, and nothing has run.
//!
//! A `RETRY` or `PUT_BACK` acts on the halted turn its order took (section 4.2), under a new try of
//! that order. After step 1 above:
//!
//! - **The record of the order.** A record that does not read is refused as above. No record, or
//!   the record of another order id or run id, is refused `not_halted`.
//! - **The try.** A lower try than the record's is refused `stale_try`. The record's own try is a
//!   resend: the turn is answered, or resumed, from where its record stands, as for a `RUN`, so an
//!   action delivered twice acts once.
//! - **The halt.** A higher try on a turn that is not halted (it runs, waits for a reboot, or has
//!   ended) is refused `not_halted`, and nothing is written. On a halted turn, the record is
//!   written with the halt lifted and the order's try ([`Event::Retried`], [`Event::PutBack`]):
//!   a `RETRY` launches the step that failed again and goes on from there; a `PUT_BACK` resumes
//!   every drained service in reverse order and ends the turn, done with the halt's reason kept
//!   as its note, each kind left applied or not as it was. Neither runs a `rollback`. A write that
//!   fails is refused `store_failed`, and nothing has run.
//!
//! The turn then runs on the caller's thread to its end, or to a reboot it waits for, and the
//! answer says which: `Finished` (succeeded for done, deferred, or halt, with the record's reason),
//! `Running` while a reboot is waited for, `Interrupted` (`store_failed`) when the record could not
//! be written. The heartbeat's maintenance report says where the turn is at every beat.

use std::collections::BTreeMap;

use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::maintenance::{
    Action, Ending, Event, Kind, MaintenanceRecord, MaintenanceRecordError, MaintenanceStore,
    next_action, transition,
};
use hostkeeper_steps::record::{Loaded, State};

use crate::executor::{Ctx, Reply};
use crate::files;
use crate::installed;
use crate::maintenance_hook::record_kind;
use crate::maintenance_run::{self, launch};
use crate::refusal::Refusal;
use crate::verb::{Maintained, TurnAction};

pub fn run(ctx: &Ctx, order: &Maintained) -> Reply {
    let _hold = match ctx.maintenance.hold(&order.order_id) {
        Ok(hold) => hold,
        Err(holder) if holder == order.order_id => return Reply::Running,
        Err(holder) => {
            return Reply::refused(
                Refusal::Busy,
                format!("the maintenance turn of order {holder} runs on this host"),
            );
        }
    };
    let store = ctx.maintenance.store(&ctx.dirs.state);
    let held = match maintenance_run::load(&store) {
        Ok(held) => held,
        Err(error) => return Reply::refused(record_refusal(&error), error),
    };
    if order.action != TurnAction::Run {
        return act_on_halt(ctx, &store, held, order);
    }
    match held {
        Some(record) if record.order_id == order.order_id => {
            if order.order_try.get() < record.order_try {
                return Reply::refused(
                    Refusal::StaleTry,
                    format!(
                        "order {} try {} is older than try {}, which the maintenance record holds",
                        order.order_id, order.order_try, record.order_try
                    ),
                );
            }
            let resent = !maintenance_run::unfinished(&record);
            maintenance_run::drive(ctx, &store, record).reply(resent)
        }
        Some(record) if holds_host(&record) => Reply::refused(
            Refusal::Busy,
            format!(
                "the maintenance turn of order {} (run {}) stands at {}",
                record.order_id,
                record.run_id,
                next_action(&record)
            ),
        ),
        _ => new_turn(ctx, &store, order),
    }
}

/// A `RETRY` or `PUT_BACK` of the halted turn `held` (the module's second list).
fn act_on_halt(
    ctx: &Ctx,
    store: &MaintenanceStore,
    held: Option<MaintenanceRecord>,
    order: &Maintained,
) -> Reply {
    let record = match held {
        Some(record)
            if (record.order_id == order.order_id) & (record.run_id == order.params.run_id()) =>
        {
            record
        }
        Some(record) => {
            return Reply::refused(
                Refusal::NotHalted,
                format!(
                    "the maintenance record holds order {} of run {}, not order {} of run {}",
                    record.order_id,
                    record.run_id,
                    order.order_id,
                    order.params.run_id()
                ),
            );
        }
        None => {
            return Reply::refused(
                Refusal::NotHalted,
                "no maintenance turn is held on this host",
            );
        }
    };
    let order_try = order.order_try.get();
    if order_try < record.order_try {
        return Reply::refused(
            Refusal::StaleTry,
            format!(
                "order {} try {} is older than try {}, which the maintenance record holds",
                order.order_id, order_try, record.order_try
            ),
        );
    }
    if order_try == record.order_try {
        let resent = !maintenance_run::unfinished(&record);
        return maintenance_run::drive(ctx, store, record).reply(resent);
    }
    let event = match order.action {
        TurnAction::Retry => Event::Retried {
            order_try,
            boot: ctx.clock.boot_id().ok(),
        },
        _ => Event::PutBack { order_try },
    };
    let Ok((next, action)) = transition(&record, event) else {
        return Reply::refused(
            Refusal::NotHalted,
            format!(
                "the maintenance turn of order {} (run {}) stands at {}, and is not halted",
                record.order_id,
                record.run_id,
                next_action(&record)
            ),
        );
    };
    if let Err(error) = store.save(&next) {
        return Reply::refused(Refusal::StoreFailed, error);
    }
    let what = match order.action {
        TurnAction::Retry => "retries",
        _ => "puts back",
    };
    ctx.log(&format!(
        "maintenance run {}: order {} try {} {what} the turn halted at {}; next: {action}",
        next.run_id,
        next.order_id,
        next.order_try,
        record.halted.as_deref().unwrap_or_default(),
    ));
    maintenance_run::drive(ctx, store, next).reply(false)
}

/// Whether the turn of `record` keeps another turn off the host: unfinished, or halted.
fn holds_host(record: &MaintenanceRecord) -> bool {
    matches!(
        next_action(record),
        Action::Launch(_) | Action::Wait(_) | Action::End(Ending::Halted(_))
    )
}

/// Starts the turn of `order` (steps 4 and 5 of the module's list) and runs it.
fn new_turn(ctx: &Ctx, store: &MaintenanceStore, order: &Maintained) -> Reply {
    if let Some(why) = package_step_unfinished(ctx) {
        return Reply::refused(Refusal::Busy, why);
    }
    let services = match launch::planned_services(ctx) {
        Ok(services) => services,
        Err(why) => return Reply::refused(Refusal::ReleaseUnreadable, why),
    };
    let kinds: Vec<Kind> = order.params.kinds().map(record_kind).collect();
    // The record of a new turn cannot break its rules: the order's ids and kinds are checked as
    // it is decoded, and the services come from a map of names.
    let written = MaintenanceRecord::new(
        order.params.run_id(),
        &order.order_id,
        order.order_try.get(),
        &kinds,
        services,
    )
    .map_err(|error| (Refusal::InvalidOrder, error.to_string()))
    .and_then(|record| {
        files::make_private(store.dir())
            .map_err(|error| error.to_string())
            .and_then(|()| launch::write_params(store.dir(), order.params.maintain()))
            .and_then(|()| store.save(&record).map_err(|error| error.to_string()))
            .map(|()| record)
            .map_err(|why| (Refusal::StoreFailed, why))
    });
    let record = match written {
        Ok(record) => record,
        Err((refusal, why)) => return Reply::refused(refusal, why),
    };
    ctx.log(&format!(
        "maintenance run {}: order {} try {} starts the host's turn",
        record.run_id, record.order_id, record.order_try
    ));
    maintenance_run::drive(ctx, store, record).reply(false)
}

/// Why a package step is unfinished on the host, if one is: a service's lock held, a step record
/// that is not finished (or does not read), or an uninstall under way.
fn package_step_unfinished(ctx: &Ctx) -> Option<String> {
    let mut names: BTreeMap<String, ServiceName> =
        installed::service_dirs(&ctx.dirs.state).unwrap_or_default();
    names.extend(installed::service_dirs(&ctx.dirs.packages).unwrap_or_default());
    for service in names.values() {
        if let Some(held) = ctx.locks.holder(service) {
            return Some(format!("{service} is locked by {}", held.holder));
        }
        let unfinished = match ctx.store(service).load() {
            Ok(Loaded::Current(record)) => !matches!(record.state, State::Finished { .. }),
            Ok(Loaded::Absent) => false,
            Ok(Loaded::Newer { .. }) | Err(_) => true,
        };
        if unfinished {
            return Some(format!("a package step of {service} is unfinished"));
        }
    }
    let uninstalling = ctx
        .placer()
        .store()
        .load()
        .map(|record| record.uninstalling)
        .unwrap_or_default();
    uninstalling
        .keys()
        .next()
        .map(|name| format!("the uninstall of {name} is unfinished"))
}

/// The refusal of a record that could not be read.
fn record_refusal(error: &MaintenanceRecordError) -> Refusal {
    match error {
        MaintenanceRecordError::Newer { .. } => Refusal::RecordNewer,
        MaintenanceRecordError::Corrupt(_) => Refusal::RecordUnreadable,
        _ => Refusal::StoreFailed,
    }
}
