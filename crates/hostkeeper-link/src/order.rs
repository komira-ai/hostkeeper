//! The coordinator's reply, the order it may carry, and the verdict the daemon reports for an
//! order (design 4.1 and 5.2).
//!
//! An [`Order`] is a verb and typed arguments only: an order id, a try, and a [`Target`] of a
//! service name, a version, a package digest and optionally an index digest, each a word with a grammar. No field of it, at
//! any depth, is a free string, so an order cannot carry a command, a path or a URL; the daemon
//! finds everything else in its own configuration and in the manifest of the package whose
//! digest verified. The verbs are those of the protobuf `Order` the loop runs: `update`, the
//! forced update, `install`, `uninstall` and `abort`; `force_rollback` and `clear_halt` are added
//! here when the loop runs them.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase};
use serde_json::{Value, json};

use crate::beat::{Feature, Features};
use crate::names::{OrderId, PackageDigest, Reason};

/// The coordinator's identity for one try of an order: what a verdict names, and what an
/// acknowledgement echoes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderRef {
    pub id: OrderId,
    /// The coordinator's number for this try of the order, from 1.
    pub order_try: u64,
}

/// A verdict the coordinator has stored: the service it is about and the order try. Order ids
/// are per service on the coordinator, so the service is part of the identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acknowledged {
    pub service: ServiceName,
    pub order: OrderRef,
}

/// What an order acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub service: ServiceName,
    /// The version the service is to run.
    pub version: Version,
    /// The digest its package must have; nothing runs from a package that does not match it.
    pub digest: PackageDigest,
    /// The SHA-256 of the release index its package was listed in: the daemon refuses an index
    /// that hashes to another. None from a coordinator that does not send one (and on the JSON
    /// route): the index is not checked.
    pub index_digest: Option<PackageDigest>,
}

/// The verbs the loop runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    /// Fetch, verify and unpack the package, read its manifest and run its hook's steps.
    Update(Target),
    /// The hookless update (`forced` in a JSON reply), run only while the coordinator's last
    /// reply advertised `forced-v1`.
    Force(Target),
    /// Add a service the host does not run, by the update path with no previous version.
    Install(Target),
    /// Stop the service's units, run its `uninstall` hook and remove its package and placed
    /// paths. Its verdict names the version the service ran.
    Uninstall(ServiceName),
    /// Stop the service's running step and end its order in the halt phase. It is not an order
    /// of its own for the verdicts: the order it stops reports how it ended.
    Abort(ServiceName),
}

/// One order of the coordinator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    /// The coordinator's number for this try, from 1: a resend keeps it, a new try raises it.
    pub order_try: u64,
    pub verb: Verb,
}

impl Order {
    /// The service the order is about.
    pub fn service(&self) -> &ServiceName {
        match &self.verb {
            Verb::Update(target) | Verb::Force(target) | Verb::Install(target) => &target.service,
            Verb::Uninstall(service) | Verb::Abort(service) => service,
        }
    }

    /// The version and package the order runs; none for an `uninstall` and an `abort`, which
    /// name only the service.
    pub fn target(&self) -> Option<&Target> {
        match &self.verb {
            Verb::Update(target) | Verb::Force(target) | Verb::Install(target) => Some(target),
            Verb::Uninstall(_) | Verb::Abort(_) => None,
        }
    }

    /// Its order id and try.
    pub fn reference(&self) -> OrderRef {
        OrderRef {
            id: self.id.clone(),
            order_try: self.order_try,
        }
    }

    /// Whether it is the hookless update.
    pub fn is_forced(&self) -> bool {
        matches!(self.verb, Verb::Force(_))
    }
}

/// How an order ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The host runs the target and confirmed it healthy.
    Succeeded,
    /// It did not; where the service is now is in its version.
    Failed,
    /// The host declined for a reason about its moment, not about the release; nothing was
    /// switched. The same target is not run again for `retry_after_secs`.
    Deferred { retry_after_secs: u64 },
}

impl Outcome {
    /// Its word in a heartbeat.
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Deferred { .. } => "deferred",
        }
    }

    /// The retry time of a deferral, in seconds; none for the other outcomes.
    pub const fn retry_after_secs(self) -> Option<u64> {
        match self {
            Outcome::Deferred { retry_after_secs } => Some(retry_after_secs),
            Outcome::Succeeded | Outcome::Failed => None,
        }
    }
}

/// The verdict of an order: the shape of a heartbeat's `last_update` (`UpdateResult` of
/// `hostkeeper.agent.v1`), what the executor answers and the step record keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderResult {
    /// The order and try this is the verdict of.
    pub order: OrderRef,
    /// The version the order named.
    pub target: Version,
    pub outcome: Outcome,
    /// Where a failure happened; none when not reported.
    pub phase: Option<Phase>,
    /// Whom a failure is blamed on; none when not reported.
    pub class: Option<Class>,
    /// The check or cause, as a token; none when not reported.
    pub reason: Option<Reason>,
    /// The verdict of a forced update.
    pub forced: bool,
    /// What the hook or the daemon said of the outcome.
    pub detail: Option<String>,
}

impl OrderResult {
    /// The answer to an order that arrived while another one ran: it is not started, and it
    /// fails with the reason `busy`, the answer the executor gives for the same case. `target`
    /// is the version the verdict names: the order's own, or the one an `uninstall` would remove.
    pub fn busy(order: &Order, target: Version) -> OrderResult {
        OrderResult {
            order: order.reference(),
            target,
            outcome: Outcome::Failed,
            phase: None,
            class: None,
            reason: Some(Reason::busy()),
            forced: order.is_forced(),
            detail: None,
        }
    }

    /// This verdict as a coordinator that advertised `features` reads it. Without `deferred-v1`
    /// a deferral is a failure before the commit point; without `halt-v1` a halt is a plain
    /// failure, with no phase. These are the shapes kbf's coordinator reads.
    pub fn shaped(&self, features: &Features) -> OrderResult {
        let mut shaped = self.clone();
        let deferred = matches!(shaped.outcome, Outcome::Deferred { .. });
        if deferred & !features.has(Feature::DeferredV1) {
            shaped.outcome = Outcome::Failed;
            shaped.phase = Some(Phase::BeforeCommit);
        }
        if (shaped.phase == Some(Phase::Halt)) & !features.has(Feature::HaltV1) {
            shaped.phase = None;
        }
        shaped
    }

    /// The verdict as JSON, with the wire's names; what the heartbeat's size is measured on.
    pub fn to_json(&self) -> Value {
        json!({
            "order_id": self.order.id.as_str(),
            "order_try": self.order.order_try,
            "target": self.target.as_str(),
            "outcome": self.outcome.as_str(),
            "retry_after_secs": self.outcome.retry_after_secs(),
            "phase": self.phase,
            "class": self.class.map(Class::as_str),
            "reason": self.reason.as_ref().map(Reason::as_str),
            "forced": self.forced,
            "detail": self.detail,
        })
    }
}

/// A deferral as the heartbeat reports it, while its retry time is ahead: the target the host
/// did not take, why, and how many seconds are left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredReport {
    pub target: Version,
    pub reason: Reason,
    pub retry_after_secs: u64,
}

impl DeferredReport {
    /// The report as JSON, with the wire's names.
    pub fn to_json(&self) -> Value {
        json!({
            "target": self.target.as_str(),
            "reason": self.reason.as_str(),
            "retry_after_secs": self.retry_after_secs,
        })
    }
}

/// The coordinator's answer to a heartbeat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// What to do now; none: nothing.
    pub order: Option<Order>,
    /// The features of those announced that the coordinator supports. A name the daemon does
    /// not know is dropped by the link.
    pub features: Vec<Feature>,
    /// How many times an operator has cleared a halt of each service.
    pub halt_generations: Vec<(ServiceName, u64)>,
    /// The verdicts the coordinator has stored, by service, order id and try: these are not
    /// sent again. An acknowledgement drops only the verdict of the service it names (and,
    /// from a coordinator that advertises `verdict-acks-v1`, the older ones of that service).
    pub acknowledged: Vec<Acknowledged>,
}
