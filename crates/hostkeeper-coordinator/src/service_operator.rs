//! The operator's queued orders over the fleet's rows (`docs/design/coordinator-multi-instance.md`,
//! section 5): an `abort` (`Hosts.Abort`) and a `clear_record` (`Hosts.ClearRecord`) are rows of
//! `fleet_operator_orders`, so the coordinator that hears the host's next heartbeat sends them,
//! whichever took the operator's call, and a coordinator that stops forgets none.
//!
//! A call writes the row as it queues the order in memory. A heartbeat decided under the rollout
//! gate first writes the orders this coordinator could not write before, then makes the host's
//! queues in memory the rows' ([`Coordinator::take_in_operator_orders`]), so an order another
//! coordinator already sent is not sent again from a stale memory and one another coordinator
//! queued is sent here; when its answer carries an order of the operator's, it deletes that order's
//! row in the same transaction ([`Coordinator::end_sent_operator_order`]). A transaction that fails
//! sends nothing and leaves the row, so the order goes at a later beat.
//!
//! A call whose row cannot be written (the database is out of reach) still queues the order in
//! memory and answers, as an abort never refuses for the store; the order is remembered as not yet
//! in the rows ([`Unwritten`]) and written by the next heartbeat this coordinator decides under the
//! gate, in its transaction. Coordinators over files have no rows: the queues in memory are the
//! only ones and nothing here runs.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_wire::HostId;

use super::Coordinator;
use crate::fleet::Fleet;
use crate::orders::Order;
use crate::store::postgres::fleet_rows::{OperatorKind, OperatorOrderRow};
use crate::store::postgres::fleet_session::Gate;

/// The host, the kind and the service of an operator's order: the key of its row.
type Key = (HostId, OperatorKind, String);

/// The orders the operator queued through this coordinator whose rows could not be written, each
/// with its order id: the order of one host, kind and service is remembered once, the first.
#[derive(Default)]
pub(super) struct Unwritten(Mutex<BTreeMap<Key, String>>);

impl Unwritten {
    fn held(&self) -> std::sync::MutexGuard<'_, BTreeMap<Key, String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Remembers the order `order_id`, unless one of the same host, kind and service is
    /// remembered already.
    fn remember(&self, key: Key, order_id: &str) {
        self.held()
            .entry(key)
            .or_insert_with(|| order_id.to_owned());
    }

    /// The order id remembered for the host `host`, the kind and the service, if one is.
    fn of(&self, host: &HostId, kind: OperatorKind, service: &str) -> Option<String> {
        self.held()
            .get(&(host.clone(), kind, service.to_owned()))
            .cloned()
    }

    /// The orders remembered now, which the decision about to begin writes first.
    pub(super) fn pending(&self) -> Pending {
        Pending(
            self.held()
                .iter()
                .map(|(key, order_id)| (key.clone(), order_id.clone()))
                .collect(),
        )
    }

    /// Forgets the orders of `written`, which the transaction that wrote them has committed.
    pub(super) fn forget(&self, written: &Pending) {
        let mut held = self.held();
        for (key, _) in &written.0 {
            held.remove(key);
        }
    }
}

/// What a decision writes of the operator's orders before it decides, and forgets once it has
/// committed: the orders not yet in the rows, as they were when the decision began.
pub(super) struct Pending(Vec<(Key, String)>);

impl Pending {
    /// Writes the orders in the transaction of `gate`, and then makes the queues of `fleet` for
    /// the host `host` the orders the rows hold for it: what another coordinator sent is gone
    /// from them, and what another queued is in them, aborts first, as [`crate::orders::next`]
    /// takes them.
    ///
    /// # Errors
    /// The client's error: the transaction is then rolled back, and nothing is sent.
    pub(super) fn write_and_take_in(
        &self,
        gate: &mut Gate<'_>,
        fleet: &mut Fleet,
        host: &HostId,
    ) -> Result<(), postgres::Error> {
        for ((key_host, kind, service), order_id) in &self.0 {
            gate.queue_operator_order(key_host.as_str(), *kind, service, order_id)?;
        }
        let rows = gate.operator_orders_of(host.as_str())?;
        while fleet.take_abort(host).is_some() {}
        while fleet.clear_records.take(host).is_some() {}
        for OperatorOrderRow {
            kind,
            service,
            order_id,
        } in rows
        {
            match kind {
                OperatorKind::Abort => fleet.queue_abort(host.clone(), service, order_id),
                OperatorKind::ClearRecord => {
                    fleet.clear_records.queue(host.clone(), service, order_id);
                }
            }
        }
        Ok(())
    }
}

/// Deletes the row of the operator's order `order` for the host `host` in the transaction of
/// `gate`, when it is an abort or a clear-record: the order is sent once, by the beat whose
/// transaction commits with the row gone.
///
/// # Errors
/// The client's error: the transaction is then rolled back, and nothing is sent.
pub(super) fn end_sent(
    gate: &mut Gate<'_>,
    host: &HostId,
    order: Option<&Order>,
) -> Result<(), postgres::Error> {
    match order {
        Some(Order::Abort { service, .. }) => {
            gate.take_operator_order(host.as_str(), OperatorKind::Abort, service)
        }
        Some(Order::ClearRecord { service, .. }) => {
            gate.take_operator_order(host.as_str(), OperatorKind::ClearRecord, service)
        }
        _ => Ok(()),
    }
}

impl Coordinator {
    /// Queues the operator's order `order_id` of `kind` for the host `host` and `service` in the
    /// rows, and answers the order id that stands there: the first one queued by any coordinator,
    /// so asking twice queues one order. Over files, or when the rows cannot be written, it
    /// answers `order_id` and queues nothing; in the second case the order is remembered, and the
    /// next heartbeat this coordinator decides under the gate writes it.
    pub(crate) fn queue_operator_order(
        &self,
        host: &HostId,
        kind: OperatorKind,
        service: &str,
        order_id: &str,
    ) -> String {
        let Some(rows) = &self.rows else {
            return order_id.to_owned();
        };
        match rows
            .rows
            .queue_operator_order(host.as_str(), kind, service, order_id)
        {
            Ok(standing) => standing,
            Err(why) => {
                tracing::warn!(node_id = %host, service, %why, "an operator's order is not written to the rows: it is written at the next heartbeat decided here");
                rows.unwritten
                    .remember((host.clone(), kind, service.to_owned()), order_id);
                order_id.to_owned()
            }
        }
    }

    /// The order id of the `clear_record` of `service` queued for the host `host`, if one waits.
    /// Over the fleet's rows the rows say, whichever coordinator queued it, and then the orders
    /// this coordinator could not write; over files the queue in `fleet` does.
    pub(crate) fn queued_clear_record(
        &self,
        fleet: &Fleet,
        host: &HostId,
        service: &str,
    ) -> Option<String> {
        let Some(rows) = &self.rows else {
            return fleet.clear_records.queued(host, service).map(str::to_owned);
        };
        let found = match rows.rows.operator_orders_of(host.as_str()) {
            Ok(found) => found
                .into_iter()
                .find(|order| order.kind == OperatorKind::ClearRecord && order.service == service)
                .map(|order| order.order_id),
            Err(why) => {
                tracing::warn!(node_id = %host, %why, "the operator's orders of a host are not read from the rows");
                None
            }
        };
        found.or_else(|| rows.unwritten.of(host, OperatorKind::ClearRecord, service))
    }
}
