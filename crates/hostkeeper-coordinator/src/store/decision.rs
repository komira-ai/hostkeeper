//! A heartbeat's deciding transaction (`docs/design/coordinator-multi-instance.md`, sections 5.2
//! and 5.3): what it read under the rollout gate, and what it writes back.
//!
//! Over a backend that other coordinators share (Postgres), a heartbeat is decided in one
//! transaction. It takes the gate with `SKIP LOCKED` (a beat never waits: a gate held, or asked
//! for by a waiting operator's edit, is [`Opened::Busy`], and the host is told nothing new until
//! its next beat), then the rows of the parts a decision reads and writes and the audit counter,
//! in the one lock order ([`super::Scope::DECIDE`]). Under those locks it reads every input
//! afresh: each part this coordinator's copy is older than, and the fleet's rows ([`Rows`]). The
//! stores' changes the decision makes (a halt, a stage entered, a version marked bad and the
//! rollback to the last good one, a verdict acknowledged) are written in that transaction as they
//! are made; the orders it gives, and the tries of those it no longer counts, are written at its
//! end ([`Decision::finish`]), and it commits. A decision that cannot be made, or whose commit
//! fails, gives no order: the host keeps what it runs and asks again.
//!
//! Over the files or the in-memory store, which one process owns, there is nothing to take:
//! [`Opened::Alone`], and the heartbeat is decided over the fleet in memory as it always was.

use super::postgres::decide::Open;
use super::{BackendError, Fresh};
use crate::desired;
use crate::ops::OpsState;
use crate::store::Facts;
use crate::verdict_file;

/// A row of `fleet_hosts`: a host's last beat, as the coordinator that heard it kept it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostRow {
    pub(crate) host_id: String,
    pub(crate) role: String,
    /// The heartbeat as that coordinator read it ([`super::beat::HostBeat::beat`]).
    pub(crate) beat: Vec<u8>,
    /// When it came, by the database's clock.
    pub(crate) seen_unix: i64,
    /// The host's own clock in it.
    pub(crate) sent_unix: i64,
    /// The instance that heard it.
    pub(crate) instance: String,
}

/// A row of `fleet_bakes`: a host's healthy beats in a row at one version of a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BakeRow {
    pub(crate) host_id: String,
    pub(crate) service: String,
    pub(crate) target: String,
    pub(crate) beats: i64,
    pub(crate) at_unix: i64,
}

/// A row of `fleet_orders`: an order a host has not yet reported done, which holds its slot. A
/// host holds one at most.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrderRow {
    pub(crate) host_id: String,
    pub(crate) service: String,
    /// The version ordered; for an uninstall, the version the host ran when it was ordered.
    pub(crate) target: String,
    /// The order id of an uninstall.
    pub(crate) uninstall_id: Option<String>,
    /// When it was given, by the database's clock.
    pub(crate) at_unix: i64,
    pub(crate) order_try: i64,
}

/// A row of `fleet_tries`: the highest try of an order of a host and service that no coordinator
/// counts as outstanding any more, which the next order numbers past.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TryRow {
    pub(crate) host_id: String,
    pub(crate) service: String,
    pub(crate) order_try: i64,
}

/// The fleet's rows, read under the gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rows {
    /// The database's clock when they were read, in unix seconds.
    pub(crate) now_unix: i64,
    /// Until when the fleet is quiet after a gap in the beats (`cluster_quiet`).
    pub(crate) quiet_until: i64,
    /// The rows of hosts another coordinator heard last; those this one heard last it noted
    /// itself.
    pub(crate) hosts: Vec<HostRow>,
    pub(crate) bakes: Vec<BakeRow>,
    pub(crate) orders: Vec<OrderRow>,
    pub(crate) tries: Vec<TryRow>,
}

/// What a decision writes of the fleet's orders at its end: the difference between the orders it
/// read and those it holds now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Writes {
    /// Orders that ended or aged out, as they were read: each deleted if it is still so.
    pub(crate) ended: Vec<OrderRow>,
    /// A host's order replaced by another (a new try, a new target): the row as read, then the new
    /// one; written only if the row is still as read.
    pub(crate) replaced: Vec<(OrderRow, OrderRow)>,
    /// Orders of hosts that held none: each inserted only if the host still holds none.
    pub(crate) given: Vec<OrderRow>,
    /// Tries no longer outstanding, each kept as the highest of its host and service.
    pub(crate) tries: Vec<TryRow>,
}

impl Writes {
    /// Whether it writes nothing.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.ended.is_empty()
            && self.replaced.is_empty()
            && self.given.is_empty()
            && self.tries.is_empty()
    }
}

/// A stage a decision enters: the compare-and-set of section 5.3 on the plan's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StageClaim<'a> {
    /// The host group whose plan it is; none for a service of the rollout file.
    pub(crate) group: Option<&'a str>,
    pub(crate) service: &'a str,
    pub(crate) version: &'a str,
    /// The stage the decision read the plan at, for this version; none when it read no plan at
    /// this version (the plan's first entry).
    pub(crate) from: Option<u64>,
    /// The stage it enters.
    pub(crate) to: u64,
}

/// How a heartbeat's decision opened.
pub(crate) enum Opened {
    /// The backend is one process's own: decide over memory, as always.
    Alone,
    /// The gate is held, or asked for by an operator's edit: nothing new is decided at this beat.
    Busy,
    /// The decision's transaction, holding its locks.
    Deciding(Box<Decision>),
}

/// A decision under way: its transaction, what it read afresh, and the fleet's rows.
pub(crate) struct Decision {
    fresh: Fresh,
    rows: Rows,
    open: Open,
}

impl Decision {
    pub(crate) fn new(open: Open, fresh: Fresh, rows: Rows) -> Decision {
        Decision { fresh, rows, open }
    }

    /// The fleet's rows it read.
    #[must_use]
    pub(crate) fn rows(&self) -> &Rows {
        &self.rows
    }

    /// The desired state read afresh, for the store's memory.
    pub(crate) fn take_desired(&mut self) -> Option<desired::Held> {
        let taken = self.fresh.desired.take()?;
        self.open.taken(super::Part::Desired);
        Some(taken)
    }

    /// The operations state and facts generations read afresh, for the store's memory.
    pub(crate) fn take_ops(&mut self) -> Option<(OpsState, Facts)> {
        let taken = self.fresh.ops.take()?;
        self.open.taken(super::Part::Ops);
        Some(taken)
    }

    /// The verdicts read afresh, for the store's memory.
    pub(crate) fn take_verdicts(&mut self) -> Option<verdict_file::Held> {
        let taken = self.fresh.verdicts.take()?;
        self.open.taken(super::Part::Verdicts);
        Some(taken)
    }

    /// Writes `writes` and commits the decision, with every change the stores made in it.
    ///
    /// # Errors
    /// [`BackendError`]: a change or an order was refused (a host that holds an order already, a
    /// row no longer as read), or the commit did not land, or its outcome is not known: nothing
    /// of the decision is to be answered.
    pub(crate) fn finish(self, writes: &Writes) -> Result<(), BackendError> {
        self.open.finish(writes)
    }
}
