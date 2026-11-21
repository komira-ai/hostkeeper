//! The handlers of `Maintenance.RetryHost` and `Maintenance.PutBack` of `hostkeeper.operator.v1`
//! (docs/design/maintenance-updates.md, sections 5.4 and 5.7), each with the operator's
//! credential only: what an operator asks of a host whose turn halted.
//!
//! Neither call reaches the host. Each stores the act on the host's turn
//! ([`crate::maintenance_runs::RunHost::queue`]: the act and a new try of the order that took the
//! turn) and audits it, `maintenance_retry_host` or `maintenance_put_back`, with the host as the
//! entry's `node_id`, the turn's group as its `host_group` when it has one, and the caller's
//! reason followed by `run <id>`, in one change.
//! The host's next heartbeat is then sent the order ([`crate::decide_maintenance`]), at each
//! heartbeat until its daemon reports the turn at another step; the daemon acts at most once, since
//! it refuses an older try and a turn that is not halted. The answer is the host's turn as stored,
//! still halted.
//!
//! A call refuses, and changes nothing, `invalid_request` for a run id, a host id or a reason that
//! breaks its rule, `unknown_run`, `run_ended` for a `RetryHost` of a run that has ended
//! (cancelled, done or past its window: a `PutBack` of a host still halted in such a run is
//! taken), `not_in_run` for a host the run does not hold, and `not_halted` for a host whose
//! stored step is not halted (a host not back included). A call repeated while the host is still
//! halted queues the act again under a newer try, in place of the one queued.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::audit_actions::{MAINTENANCE_PUT_BACK, MAINTENANCE_RETRY_HOST};
use crate::maintenance_runs::{Act, Step};
use crate::maintenance_service::{host_proto, run_id};
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

impl Coordinator {
    /// `Maintenance.RetryHost` of `request`, by `caller`: the halted host's daemon is ordered the
    /// turn's failed step again, under a new try, at its next heartbeats.
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request`: not a run id, not a host id, or a reason
    /// that breaks its rule; `unknown_run`; `run_ended`; `not_in_run`; `not_halted`;
    /// `store_failed`; `audit_unavailable`.
    pub(crate) fn maintenance_retry_host(
        &self,
        caller: &Caller,
        request: proto::RetryMaintenanceHostRequest,
    ) -> Result<proto::RetryMaintenanceHostResponse, Refusal> {
        let (host, entry) = self.queue_act(
            caller,
            [
                request.run_id.as_str(),
                request.host_id.as_str(),
                request.reason.as_str(),
            ],
            Act::Retry,
        )?;
        Ok(proto::RetryMaintenanceHostResponse {
            host,
            entry: Some(entry),
        })
    }

    /// `Maintenance.PutBack` of `request`, by `caller`: the halted host's daemon is ordered to run
    /// the drained services' `resume` and leave the update as far as it got, under a new try, at
    /// its next heartbeats. Taken in a run that has ended too.
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request`: not a run id, not a host id, or a reason
    /// that breaks its rule; `unknown_run`; `not_in_run`; `not_halted`; `store_failed`;
    /// `audit_unavailable`.
    pub(crate) fn maintenance_put_back(
        &self,
        caller: &Caller,
        request: proto::PutBackMaintenanceHostRequest,
    ) -> Result<proto::PutBackMaintenanceHostResponse, Refusal> {
        let (host, entry) = self.queue_act(
            caller,
            [
                request.run_id.as_str(),
                request.host_id.as_str(),
                request.reason.as_str(),
            ],
            Act::PutBack,
        )?;
        Ok(proto::PutBackMaintenanceHostResponse {
            host,
            entry: Some(entry),
        })
    }

    /// Queues `act` on the halted host of the run, by `caller`, and audits it in the same change:
    /// `fields` are the run id, the host id and the reason of the request. Answers the host's
    /// turn as [`crate::maintenance_runs::Run::shown`] leaves it, and the entry.
    ///
    /// # Errors
    /// As [`Self::maintenance_retry_host`], but `run_ended` only for a `Retry`.
    fn queue_act(
        &self,
        caller: &Caller,
        fields: [&str; 3],
        act: Act,
    ) -> Result<(Option<proto::MaintenanceHost>, proto::AuditEntry), Refusal> {
        let by = caller.operator()?;
        let [run, host, reason] = fields;
        let id = run_id(run)?;
        let host: HostId = host.parse().map_err(|_| Refusal::InvalidRequest)?;
        let reason = valid_reason(reason).ok_or(Refusal::InvalidRequest)?;
        let action = match act {
            Act::Retry => MAINTENANCE_RETRY_HOST,
            Act::PutBack => MAINTENANCE_PUT_BACK,
        };
        let now = self.ops.unix_now();
        let mut entry = AuditEntry::new(by, action);
        entry.node_id = Some(host.as_str().to_owned());
        entry.reason = Some(format!("{reason} run {id}"));
        let mut answered = None;
        let done = self.ops.decide_change(Write::BeforeMemory, |state| {
            let run = state
                .maintenance_runs
                .iter_mut()
                .find(|run| run.id == id)
                .ok_or(Refusal::UnknownRun)?;
            if act == Act::Retry && run.ended_at(now) {
                return Err(Refusal::RunEnded);
            }
            let at = run
                .hosts
                .iter()
                .position(|held| held.host_id == host.as_str())
                .ok_or(Refusal::NotInRun)?;
            if run.hosts[at].step != Step::Halted {
                return Err(Refusal::NotHalted);
            }
            run.hosts[at].queue(act);
            let held = &run.hosts[at];
            entry.host_group = Some(held.host_group.clone()).filter(|group| !group.is_empty());
            answered = Some(host_proto(&run.shown(held, now)));
            Ok(entry)
        })?;
        Ok((answered, entry_proto(&done)))
    }
}

#[cfg(test)]
#[path = "maintenance_operator_tests.rs"]
mod tests;
