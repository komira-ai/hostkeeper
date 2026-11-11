//! `Hosts.ClearRecord` (`docs/design/two-programs.md` section 3.2): an operator orders a host to
//! set aside its step record of a service when that record does not read, the `clear_record`
//! verb of `hostkeeper.agent.v1`.
//!
//! The order goes only to a daemon that announced [`FEATURE`] in its last heartbeat: the call
//! is refused `daemon_too_old` for any other host and nothing is ordered, as a force skips such
//! a host ([`crate::force`]). An accepted call writes the audit line `record_cleared` (the
//! operator, the host, the service) before the order is queued, so a call refused
//! `store_failed` or `audit_unavailable` orders nothing either. The order then waits in
//! [`Queue`] for the host's next heartbeat on a transport that carries typed orders
//! ([`crate::orders::next`]), takes no update slot and is sent once, as an `abort` is. Asking
//! again while it waits answers the same order id and writes nothing. Like the other orders it
//! is kept in memory only: a restart of the coordinator forgets an order not yet sent, and the
//! operator asks again.
//!
//! What the daemon does with it is the daemon's (design section 3.2). Its verdict, the
//! service's `last_update` under the order's id, is acknowledged as any order's
//! ([`crate::verdict_file::acknowledge`]) and shown by `Hosts.Get`, but it is no word on any
//! release: it is taken out of the heartbeat before any rule reads it ([`Queue::take_verdicts`]),
//! so the gate's `failed_here`, the update slot, the bake, the halt and the counting read the
//! host's standing verdict instead, and it never becomes the standing verdict itself. Like a
//! refusal for the try, it says how the host answered an order, not how a version ran there.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::{Heartbeat, UpdateResult};

use crate::operator::Caller;
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;
use crate::verdict_file::Heard;

/// The feature a daemon announces to be sent a `clear_record`.
pub const FEATURE: &str = "clear-record-v1";

/// The audit action of an accepted `Hosts.ClearRecord`.
pub const ACTION: &str = "record_cleared";

/// The prefix of every order id [`order_id`] makes.
const ORDER_ID_PREFIX: &str = "clear-record-";

/// The order id of the `clear_record` audited as the entry `seq`.
#[must_use]
pub fn order_id(seq: u64) -> String {
    format!("{ORDER_ID_PREFIX}{seq}")
}

/// Whether `report` is the verdict of a `clear_record` order: its order id is one [`order_id`]
/// makes, `clear-record-` and a number.
#[must_use]
pub(crate) fn is_verdict(report: &UpdateResult) -> bool {
    report.order_id.as_ref().is_some_and(|id| {
        id.as_str()
            .strip_prefix(ORDER_ID_PREFIX)
            .is_some_and(|seq| !seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The `clear_record` orders an operator queued, per host and service, each with its order id:
/// sent once, at the host's next heartbeat that can carry it. And the verdict of the last one
/// each host reported, per service, which `Hosts.Get` shows until the host reports a verdict of
/// another order of the service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queue {
    queued: BTreeMap<(HostId, String), String>,
    shown: BTreeMap<(HostId, String), UpdateResult>,
}

impl Queue {
    /// The order id queued for the host `id` and `service`, if one waits.
    #[must_use]
    pub fn queued(&self, id: &HostId, service: &str) -> Option<&str> {
        self.queued
            .get(&(id.clone(), service.to_owned()))
            .map(String::as_str)
    }

    /// Queues a `clear_record` of `service` for the host `id` as the order `order_id`, unless
    /// one waits for them already: the first stands.
    pub fn queue(&mut self, id: HostId, service: String, order_id: String) {
        self.queued.entry((id, service)).or_insert(order_id);
    }

    /// The verdict of a `clear_record` of `service` that `Hosts.Get` shows for the host `id`.
    #[must_use]
    pub(crate) fn shown(&self, id: &HostId, service: &str) -> Option<&UpdateResult> {
        self.shown.get(&(id.clone(), service.to_owned()))
    }

    /// Takes the verdict of a `clear_record` out of each service entry of `beat` of the host
    /// `id` (and out of its top level, the service `legacy`), so that no rule reads it, and
    /// returns them, each with its service, to be acknowledged. Each is the one shown for its
    /// service from now on; an entry that carries a verdict of another order ends the one shown.
    pub(crate) fn take_verdicts(
        &mut self,
        id: &HostId,
        legacy: Option<&str>,
        beat: &mut Heartbeat,
    ) -> Vec<Heard> {
        let mut taken = Vec::new();
        let mut take = |name: &str, last: &mut Option<UpdateResult>| {
            let key = (id.clone(), name.to_owned());
            match last.take_if(|report| is_verdict(report)) {
                Some(report) => {
                    self.shown.insert(key, report.clone());
                    taken.push((name.to_owned(), Some(report)));
                }
                None if last.is_some() => {
                    self.shown.remove(&key);
                }
                None => {}
            }
        };
        if let Some(name) = legacy {
            take(name, &mut beat.last_update);
        }
        for one in beat.services.iter_mut().flatten() {
            take(&one.name, &mut one.last_update);
        }
        taken
    }

    /// Takes a `clear_record` queued for the host `id`, the first by service name: the service
    /// and its order id. Taken, it is not sent again.
    pub fn take(&mut self, id: &HostId) -> Option<(String, String)> {
        let key = self.queued.keys().find(|(host, _)| host == id).cloned()?;
        self.queued
            .remove_entry(&key)
            .map(|((_, service), order_id)| (service, order_id))
    }
}

impl Coordinator {
    /// `Hosts.ClearRecord`: queues a `clear_record` of the service for the host's next
    /// heartbeat, after its `record_cleared` audit line is written, and answers the order's id.
    /// A second call while the first order waits answers that order's id and changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id), `unknown_service`,
    /// `unknown_host` (not heard from since the coordinator started), `daemon_too_old` (its
    /// daemon did not announce [`FEATURE`] in its last heartbeat), `store_failed` and
    /// `audit_unavailable` (the audit line was not written). A refused call orders nothing and
    /// writes nothing.
    pub fn clear_record(
        &self,
        caller: &Caller,
        request: &proto::ClearRecordRequest,
    ) -> Result<proto::ClearRecordResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::HostsRefusal::UnknownService)?;
        // Under the fleet's lock, as an abort is queued: no heartbeat takes the queue between
        // the check of what the host announced and the order.
        let mut fleet = self.lock_fleet();
        if fleet.node(&id).is_none() {
            return Err(proto::HostsRefusal::UnknownHost);
        }
        if !fleet.announced(&id, FEATURE) {
            return Err(proto::HostsRefusal::DaemonTooOld);
        }
        if let Some(order_id) = fleet.clear_records.queued(&id, &def.name) {
            return Ok(proto::ClearRecordResponse {
                order_id: order_id.to_owned(),
            });
        }
        let mut audit = AuditEntry::new(by, ACTION);
        audit.service = Some(def.name.clone());
        audit.node_id = Some(id.to_string());
        let done = self.ops.try_change(Write::BeforeMemory, audit, |_| {
            Ok::<_, proto::HostsRefusal>(())
        })?;
        let order_id = order_id(done.seq);
        fleet
            .clear_records
            .queue(id, def.name.clone(), order_id.clone());
        Ok(proto::ClearRecordResponse { order_id })
    }
}
