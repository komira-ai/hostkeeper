//! `uninstall`: stop the service's units, run its `uninstall` hook step, unlink and remove its
//! package, its placed paths and its step records, in the order of design section 4.7
//! ([`crate::uninstall`]). It runs under the service's lock, never waited for: another order of
//! the service is `busy`, and a resend while the uninstall runs answers that it runs. How it
//! ended is kept for `Status` while the daemon runs ([`Ctx::record_uninstall`]).

use hostkeeper_config::names::ServiceName;

use crate::executor::{Ctx, Reply};
use crate::locks::Holder;
use crate::refusal::Refusal;
use crate::verb::Unhooked;

pub fn run(ctx: &Ctx, order: Unhooked) -> Reply {
    locked(ctx, &order.service, order.order_try.get())
}

/// The uninstall of `service` under its lock, for the order's try `order_try` (0 when the
/// daemon's start resumes it). A step that could not be stopped keeps the service locked.
pub fn locked(ctx: &Ctx, service: &ServiceName, order_try: u64) -> Reply {
    let guard = match ctx.locks.take_for(service, Holder::Uninstall) {
        Ok(guard) => guard,
        Err(held) => return super::update::while_held(service, &held, &Holder::Uninstall),
    };
    let reply = crate::uninstall::uninstall(ctx, service);
    ctx.record_uninstall(service, &reply, order_try);
    match reply.refusal() {
        Some(Refusal::Unstopped) => guard.keep(),
        _ => drop(guard),
    }
    reply
}
