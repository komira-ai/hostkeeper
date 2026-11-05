//! The failure reports an override acknowledged (`docs/design/host-updates.md`, section 3.8,
//! the shape of a halt's acks).
//!
//! A host keeps the report of its last failed update until it runs another, so when a version
//! the coordinator had marked bad is lifted by hand, the hosts that failed it still say so.
//! Counting those reports again would roll the override back at the next heartbeat, so the
//! reports standing at that moment are acked, one per host with the whole report; the acks are
//! in the ops file, so a restart keeps them. CI no longer lifts a mark (design section 8.1: only
//! the operator's unmark or a force does), so an ack is written by those, and read here from an
//! ops file that holds one. An ack matches one report: a report of the same host that differs
//! in any part (the target, phase, class or reason) is another report and counts. A host's ack
//! is dropped when the host reports something else ([`stale_acks`]) and when the host is next
//! ordered the service ([`release_ack`]), so a failure after the override counts as one.

use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::ops::{OpsError, OpsStore, ServiceOps, Write};
use crate::reconcile::{COORDINATOR, Change, entry};

/// The acks in `marks` whose host, as `service` last heard it, no longer makes the report the
/// ack is for. A host the coordinator has not heard from keeps its ack.
pub(crate) fn stale_acks(service: &str, marks: &ServiceOps, fleet: &Fleet) -> Vec<Change> {
    marks
        .failure_acked
        .iter()
        .filter(|ack| {
            fleet.hosts_of(service).any(|(id, host)| {
                id.to_string() == ack.node_id && host.beat.last_update.as_ref() != Some(&ack.report)
            })
        })
        .map(|ack| Change::DropAck(ack.node_id.clone()))
        .collect()
}

/// Drops the ack of the host `node`, in the order `write` says.
pub(crate) fn drop_ack(
    ops: &OpsStore,
    service: &str,
    node: &str,
    why: String,
    write: Write,
) -> Result<(), OpsError> {
    let audit = entry(COORDINATOR, "drop_failure_ack", service, why);
    ops.change(write, audit, |state| {
        let marks = state.services.entry(service.to_owned()).or_default();
        marks.failure_acked.retain(|ack| ack.node_id != node);
    })
    .map(drop)
}

/// The host `node` is about to be ordered `service`: drops its ack, if it has one, durably (in
/// the file before memory) and answers whether the order may go out. When the write fails the
/// ack is still in the file and in memory, the failure is logged, and the answer is no: an
/// order is never given while its host's ack is on disk, so after a crash a failure that is
/// byte for byte the one acked cannot come back hidden.
pub fn release_ack(ops: &OpsStore, service: &str, node: &HostId) -> bool {
    let name = node.to_string();
    if !ops
        .marks(service)
        .failure_acked
        .iter()
        .any(|ack| ack.node_id == name)
    {
        return true;
    }
    let why = format!("{name} is to be ordered");
    match drop_ack(ops, service, &name, why, Write::BeforeMemory) {
        Ok(()) => true,
        Err(why) => {
            tracing::error!(%service, node_id = %name, %why, "a failure ack was not dropped: no order");
            false
        }
    }
}
