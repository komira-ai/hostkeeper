//! `Hosts.Retry`: an operator lets a host be ordered again a version it failed.
//!
//! A host whose last report of a service is a failure of the desired version is not ordered that
//! version again (the gate's `failed_here`, [`crate::decide`]): not at its next heartbeat, not
//! after an operator clears the halt the failure raised (which acknowledges the failure: the host
//! leaves the update plan, `docs/design/rollout-update-plans.md` section 1.5), and not after the
//! host is excluded and included again. Ordering it again is the operator's decision, so it is
//! this call: it sets the failed report aside ([`crate::fleet::retry`]), and the host is ordered
//! the service's desired version at its next heartbeat, as the update plan and the gate's other
//! rules let it, at the try after the failed one ([`crate::fleet::Fleet::next_try`]).
//!
//! The call is audited `retry` (the operator, the host, the service) before the report is set
//! aside, so a call refused `store_failed` or `audit_unavailable` sets nothing aside. Asking again
//! while the report is set aside answers it again and writes nothing. Like the orders, what is
//! set aside is kept in memory only: after a restart of the coordinator the operator asks again.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::grpc::report::update_result;
use crate::operator::{Caller, entry_proto};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The audit action of an accepted `Hosts.Retry`.
pub const ACTION: &str = "retry";

impl Coordinator {
    /// `Hosts.Retry`: sets aside the host's failed report of the service, after its `retry` audit
    /// line is written, and answers the report. A second call while the report is set aside
    /// answers it again and writes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the node id), `unknown_service` (the
    /// rollout file defines no such service and no host group lists it),
    /// `unknown_host` (not heard from since the coordinator started), `nothing_to_retry` (the
    /// host's last report of the service is not a failure, or it has none, or it is a failure of
    /// the version the host runs), `store_failed` and
    /// `audit_unavailable` (the audit line was not written). A refused call sets nothing aside
    /// and writes nothing.
    pub fn retry(
        &self,
        caller: &Caller,
        request: &proto::RetryRequest,
    ) -> Result<proto::RetryResponse, proto::HostsRefusal> {
        let by = caller.operator()?;
        let id = request
            .node_id
            .parse::<HostId>()
            .map_err(|_| proto::HostsRefusal::InvalidRequest)?;
        let def = self
            .package_def(&request.service)
            .ok_or(proto::HostsRefusal::UnknownService)?;
        // Under the fleet's lock: no heartbeat replaces the report between its check and its
        // setting aside.
        let mut fleet = self.lock_fleet();
        if fleet.node(&id).is_none() {
            return Err(proto::HostsRefusal::UnknownHost);
        }
        let (report, already) = fleet
            .retryable(&def.name, &id)
            .ok_or(proto::HostsRefusal::NothingToRetry)?;
        let set_aside = Some(update_result(&report));
        if already {
            return Ok(proto::RetryResponse {
                set_aside,
                entry: None,
            });
        }
        let mut audit = AuditEntry::new(by, ACTION);
        audit.service = Some(def.name.clone());
        audit.node_id = Some(id.to_string());
        let done = self.ops.try_change(Write::BeforeMemory, audit, |_| {
            Ok::<_, proto::HostsRefusal>(())
        })?;
        fleet.set_aside(&def.name, &id);
        tracing::info!(
            service = %def.name,
            node_id = %id,
            target = %report.target.as_str(),
            order_try = ?report.order_try,
            "a failed report was set aside: the host is ordered again"
        );
        Ok(proto::RetryResponse {
            set_aside,
            entry: Some(entry_proto(&done)),
        })
    }
}
