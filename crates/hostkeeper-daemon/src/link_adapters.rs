//! The daemon behind two ports of the heartbeat loop (`hostkeeper_link`): the executor and the
//! held halts.
//!
//! [`DaemonOrders`] is the loop's [`OrderSink`]: it writes the loop's order as the protobuf
//! `Order` that [`Daemon::order`] takes ([`wire_order`]), and reads the daemon's [`Reply`] as
//! the verdict the loop reports ([`order_result`]). It decides nothing about the order: the
//! checks, the locks and the try rules are the daemon's.
//!
//! [`DaemonHalts`] is the loop's [`HeldHalts`] over the very store the executor reads
//! ([`crate::executor::Ctx::halts`]): a generation the loop raises is the one the next order
//! of the service is judged against.

use std::io;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Phase, Verdict};
use hostkeeper_link::{HeldHalts, Order, OrderResult, OrderSink, Outcome, Reason, Verb};
use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_steps::runner::DEFAULT_RETRY_SECS;

use crate::executor::{Daemon, Reply};
use crate::halt::HaltError;

/// The most bytes of a verdict's `detail` (`UpdateResult.detail` of `hostkeeper.agent.v1`).
pub const MAX_DETAIL: usize = 512;

/// The executor port over the daemon.
pub struct DaemonOrders {
    daemon: Arc<Daemon>,
}

impl DaemonOrders {
    pub fn new(daemon: Arc<Daemon>) -> DaemonOrders {
        DaemonOrders { daemon }
    }
}

impl OrderSink for DaemonOrders {
    fn execute(&self, order: &Order) -> OrderResult {
        order_result(order, &self.daemon.order(&wire_order(order)))
    }
}

/// `order` as the protobuf `Order` the daemon takes, every field as it is: the id, the try and
/// the target's service, version and digest. An update is an `update`, a forced update a `force`.
/// The loop's order has no halt generation, so a `force` carries 0 and lifts no halt.
pub fn wire_order(order: &Order) -> wire::Order {
    let target = order.target();
    let service = target.service.as_str().to_owned();
    let version = target.version.as_str().to_owned();
    let package_digest = target.digest.as_str().to_owned();
    let verb = match &order.verb {
        Verb::Update(_) => wire::order::Verb::Update(wire::Update {
            service,
            version,
            package_digest,
        }),
        Verb::Force(_) => wire::order::Verb::Force(wire::Force {
            service,
            version,
            package_digest,
            halt_generation: 0,
        }),
    };
    wire::Order {
        order_id: order.id.as_str().to_owned(),
        r#try: order.order_try,
        verb: Some(verb),
    }
}

/// The verdict the loop reports for `order`, from the daemon's `reply`.
///
/// - A finished order is its verdict, with its reason when that is a token: a halt is `failed`
///   in the phase `halt`; a deferral keeps its retry time, or the runner's default when the hook
///   gave none. A reason that is not a token is left out and said in the detail.
/// - A refused or interrupted order is `failed` with the refusal's token as its reason and the
///   daemon's text as its detail, in no phase: a refusal is not a new halt, and the halt that
///   caused `halted` was reported when it was raised.
/// - An order that is already running is not started: it is answered `busy`, as the loop
///   answers an order that arrives while another one runs.
///
/// The order's id, try and version, and whether it is forced, are the order's own. The detail
/// is cut to [`MAX_DETAIL`] bytes.
pub fn order_result(order: &Order, reply: &Reply) -> OrderResult {
    let base = OrderResult {
        order: order.reference(),
        target: order.target().version.clone(),
        outcome: Outcome::Failed,
        phase: None,
        class: None,
        reason: None,
        forced: order.is_forced(),
        detail: None,
    };
    match reply {
        Reply::Running => OrderResult::busy(order),
        Reply::Refused { refusal, detail } | Reply::Interrupted { refusal, detail } => {
            OrderResult {
                reason: Reason::new(refusal.token()).ok(),
                detail: Some(bounded(detail)),
                ..base
            }
        }
        Reply::Finished {
            verdict, reason, ..
        } => finished(base, verdict, reason.as_deref()),
    }
}

/// `base` as the order ended with `verdict` and `reason`.
fn finished(base: OrderResult, verdict: &Verdict, reason: Option<&str>) -> OrderResult {
    let (outcome, phase, class) = match verdict {
        Verdict::Succeeded { .. } => (Outcome::Succeeded, None, None),
        Verdict::Failed { phase, class } => (Outcome::Failed, Some(*phase), Some(*class)),
        Verdict::Deferred { retry_after_secs } => (
            Outcome::Deferred {
                retry_after_secs: retry_after_secs.unwrap_or(DEFAULT_RETRY_SECS),
            },
            None,
            None,
        ),
        Verdict::Halt => (Outcome::Failed, Some(Phase::Halt), None),
    };
    let (reason, detail) = match reason.map(Reason::new) {
        None => (None, None),
        Some(Ok(reason)) => (Some(reason), None),
        Some(Err(refused)) => (None, Some(bounded(&refused.to_string()))),
    };
    OrderResult {
        outcome,
        phase,
        class,
        reason,
        detail,
        ..base
    }
}

/// `text` cut to at most [`MAX_DETAIL`] bytes, at a character boundary.
fn bounded(text: &str) -> String {
    text.chars()
        .scan(0, |used, c| {
            *used += c.len_utf8();
            (*used <= MAX_DETAIL).then_some(c)
        })
        .collect()
}

/// The held-halt port over the daemon's own store.
pub struct DaemonHalts {
    daemon: Arc<Daemon>,
}

impl DaemonHalts {
    pub fn new(daemon: Arc<Daemon>) -> DaemonHalts {
        DaemonHalts { daemon }
    }
}

impl HeldHalts for DaemonHalts {
    /// The generation the store holds. A store that cannot be read holds every generation: the
    /// loop raises a generation only above the one held, so a halt is never lifted on a file
    /// that cannot be read, and the executor refuses the service's orders (`halt_unreadable`)
    /// until it can.
    fn held(&self, service: &ServiceName) -> u64 {
        self.daemon.ctx().halts.get(service).unwrap_or(u64::MAX)
    }

    /// Raises the store's generation. A generation below the one held changes nothing and is
    /// not an error; a store that cannot be read or written is.
    fn raise(&self, service: &ServiceName, generation: u64) -> io::Result<()> {
        match self.daemon.ctx().halts.raise(service, generation) {
            Ok(()) | Err(HaltError::Lower { .. }) => Ok(()),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}
