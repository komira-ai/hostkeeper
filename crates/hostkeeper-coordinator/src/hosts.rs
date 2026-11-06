//! `Hosts.List`, `Hosts.Get` and `Hosts.Retire`, as plain functions over the messages of
//! `hostkeeper.operator.v1`.
//!
//! `List` and `Get` answer the hosts the coordinator has heard from since it started. Both read
//! what `Releases.Status` reads, in one snapshot under the fleet's lock
//! ([`Coordinator::status_in`]): every node a heartbeat noted, and for each how every service
//! that heard it, or does not count it, sees it ([`crate::grpc::report::host`]). They change
//! nothing.
//!
//! `Retire` revokes an enrolled host (design 5.7 step 7 and 6.1): its deny row is written
//! ([`crate::enrollment::Enrollment::deny_host`]), so its heartbeats and its `Renew` are refused
//! at once; its own entries in the operator's service assignments are dropped
//! ([`crate::assignment::ServiceAssignments::forget_host`]), with the `host_retire` audit entry
//! in the same write of the ops file; and its update slot is freed now when no update or order
//! of it is in flight ([`crate::fleet::Fleet::release_if_final`]), else when the host reports the
//! update's record final ([`Coordinator::final_report`]). It sends the host no `uninstall`.
//!
//! All three take the operator token only, as every call of [`crate::operator`] does.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::grpc::report;
use crate::holds::marks_of;
use crate::operator::{Caller, entry_proto, optional_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The audit action of a retirement.
pub const ACTION_RETIRE: &str = "host_retire";

/// The most hosts one `Hosts.List` answers: a `limit` of 0, or above it, is this.
pub const MAX_HOSTS_PAGE: usize = 256;

impl Coordinator {
    /// `Hosts.List`: the hosts heard from since the coordinator started, by node id, after the
    /// node id `page_token` names, at most `limit` ([`MAX_HOSTS_PAGE`] for 0 or above it), and
    /// the token of the next page (the last node id of this one; empty on the last page).
    /// Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no node id).
    pub fn list_hosts(
        &self,
        caller: &Caller,
        request: proto::ListHostsRequest,
    ) -> Result<proto::ListHostsResponse, proto::HostsRefusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(
                token
                    .parse::<HostId>()
                    .map_err(|_| proto::HostsRefusal::InvalidRequest)?,
            ),
        };
        let size = match request.limit as usize {
            0 => MAX_HOSTS_PAGE,
            limit => limit.min(MAX_HOSTS_PAGE),
        };
        let now = (self.clock)();
        let unix_now = self.ops.unix_now();
        let fleet = self.lock_fleet();
        let status = self.status_in(&fleet, now);
        let mut rest = fleet
            .nodes()
            .filter(|(id, _)| after.as_ref().is_none_or(|after| *id > after));
        let hosts: Vec<proto::Host> = rest
            .by_ref()
            .take(size)
            .map(|(id, node)| report::host(id, node, &status, now, unix_now))
            .collect();
        let next_page_token = match hosts.last() {
            Some(last) if rest.next().is_some() => last.node_id.clone(),
            _ => String::new(),
        };
        Ok(proto::ListHostsResponse {
            hosts,
            next_page_token,
        })
    }

    /// `Hosts.Get`: the host `node_id`, as `Hosts.List` answers it. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id), `unknown_host` (not heard
    /// from since the coordinator started).
    pub fn get_host(
        &self,
        caller: &Caller,
        request: &proto::GetHostRequest,
    ) -> Result<proto::GetHostResponse, proto::HostsRefusal> {
        caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let now = (self.clock)();
        let unix_now = self.ops.unix_now();
        let fleet = self.lock_fleet();
        let node = fleet.node(&id).ok_or(proto::HostsRefusal::UnknownHost)?;
        let status = self.status_in(&fleet, now);
        Ok(proto::GetHostResponse {
            host: Some(report::host(&id, node, &status, now, unix_now)),
        })
    }

    /// `Hosts.Retire`: revokes the enrolled host `node_id`, as the module says. The fleet's
    /// lock is held from the deny row to the slot, so two calls at once audit one retirement,
    /// and no assignment is written between the read and the drop. The first retirement is
    /// audited; a repeat writes and audits nothing more, unless the host still has entries in the
    /// assignments (the first call's write of them failed), which it drops and audits then.
    /// Answers whether an update is still in flight on the host, holding its slot.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id or the reason),
    /// `unknown_host` (no host enrolled with the id; also without enrollment), `store_failed`
    /// (the deny row, or the ops file), `audit_unavailable`. The deny row stands once written:
    /// a failure after it leaves the host refused, and asking again finishes the retirement.
    pub fn retire_host(
        &self,
        caller: &Caller,
        request: &proto::RetireRequest,
    ) -> Result<proto::RetireResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let why = optional_reason(&request.reason, proto::HostsRefusal::InvalidRequest)?;
        let enrollment = self.enrollment().ok_or(proto::HostsRefusal::UnknownHost)?;
        let mut fleet = self.lock_fleet();
        let first = enrollment.deny_host(&id)?;
        let node = id.as_str();
        let held = self.ops.read(|state| {
            state
                .assignments
                .values()
                .any(|entries| entries.names_host(node))
        });
        let entry = if first || held {
            let mut audit = AuditEntry::new(by, ACTION_RETIRE);
            audit.node_id = Some(node.to_owned());
            audit.reason = why;
            let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
                for entries in state.assignments.values_mut() {
                    entries.forget_host(node);
                }
                state.assignments.retain(|_, entries| !entries.is_empty());
                Ok::<(), proto::HostsRefusal>(())
            })?;
            fleet.set_marks(self.ops.read(marks_of));
            Some(done)
        } else {
            None
        };
        let update_in_flight = !fleet.release_if_final(&id, (self.clock)());
        drop(fleet);
        if !update_in_flight {
            tracing::info!(host_id = %id, "a retired host held no update: slot freed");
            enrollment.retire(&id);
        }
        Ok(proto::RetireResponse {
            entry: entry.as_ref().map(entry_proto),
            update_in_flight,
        })
    }
}
