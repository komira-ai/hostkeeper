//! `Enrollment.List`, `Enrollment.Approve` and `Enrollment.Reject` of `hostkeeper.operator.v1`,
//! as plain functions over its messages, as the other operator calls are ([`crate::operator`]). A transport (the
//! gRPC services, or the JSON gateway) finds the caller from the bearer
//! ([`Coordinator::caller`]) before it reads the request, and hands the call here.
//!
//! They take the operator token only: a valid CI token is `forbidden`, and no token or any
//! other `unauthorized`, before the request is looked at; nothing is audited for them, since
//! no operator made the call. The work is [`Enrollment::list`], [`Enrollment::approve`], which
//! audits each approval and each refused one under the name of the operator's token, and
//! [`Enrollment::reject`], which audits the first rejection of a request. A coordinator without
//! `--enrollment-dir` has no pending request.
//!
//! An approval of a request that matches one record of the host inventory is made as that
//! record's host id, read from the ops file when the call is made: an empty host id is it, and
//! another host id is `record_mismatch` unless the approval replaces (a record removed or added
//! while the call runs is seen at the next call). A request that matches a record another
//! pending request matches is contested: the list names the others on it, and its approval is
//! `record_contested` until all but one are rejected (a request posted while the call runs is
//! seen at the next call).
//!
//! A request that matches an open inventory record exactly is not left to an operator: the
//! coordinator approves it itself, as the record's host id and role, with no service, when it is
//! posted ([`Coordinator::enroll`]) and when the record is added or opened
//! ([`Coordinator::add_record`], [`Coordinator::open_record`]); the conditions are
//! [`Enrollment::approve_by_record`]'s. What waits for an operator is listed with the record's
//! state (`record_state`, `record_approve_until_unix`).
//!
//! An approval assigns the services it names to the host it approves
//! ([`Coordinator::assign_host`], the one path by which `Services.Assign` of a host assigns
//! too), after the approval is recorded: each service is one audit entry `assign`. An
//! assignment that cannot be written is logged and leaves the approval standing; the operator
//! assigns the services with `Services.Assign`. A Mac approved by its MDM nonce at its
//! `Enroll` ([`Coordinator::enroll`]) has the services of its allowlist entry assigned the same
//! way, by `mdm-nonce`. An approval by a record assigns none.

use std::collections::BTreeMap;

use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollRequest, EnrollResponse, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;
use hostkeeper_wire::{HostId, RoleName};

use crate::enrollment::{ByRecord, Enrollment, MAC_APPROVER, RpcError};
use crate::inventory::show_match;
use crate::operator::{Caller, entry_proto};
use crate::service::Coordinator;

impl Coordinator {
    /// `Enrollment.List`: the requests waiting for an operator ([`Enrollment::list`]), each
    /// with its match against the host inventory ([`crate::inventory::show_match`], with where
    /// the record's window stands at the time of the call) and, when it
    /// is contested, the other pending requests that match its record
    /// ([`Enrollment::contests`]).
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token), `store_failed`.
    pub fn enrollment_list(
        &self,
        caller: &Caller,
        request: &proto::ListPendingRequest,
    ) -> Result<proto::ListPendingResponse, EnrollmentRefusal> {
        caller.operator()?;
        let Some(enrollment) = self.enrollment() else {
            return Ok(proto::ListPendingResponse::default());
        };
        let mut listed = enrollment.list(request)?;
        let now = self.ops.unix_now();
        let records = self.ops.read(|state| state.inventory.clone());
        // An expired request is not pending, so no contest names it.
        let contested: BTreeMap<String, Vec<String>> = enrollment
            .contests(&records)?
            .into_iter()
            .map(|(id, others)| (id.to_hex(), others.iter().map(KeyHash::to_hex).collect()))
            .collect();
        for pending in &mut listed.requests {
            show_match(pending, &records, now);
            if let Some(others) = contested.get(&pending.request_id) {
                pending.contested_by.clone_from(others);
            }
        }
        Ok(listed)
    }

    /// `Enrollment.Approve`: approves a pending request as the role and services it names,
    /// which must be services this coordinator manages, and as the host id it names or, for a
    /// request that matches one inventory record, that record's ([`Enrollment::approve`]); and
    /// assigns those services to the host ([`Coordinator::assign_host`]). The answer names the
    /// host id approved. The host gets its certificate at its next `Poll`, never in this answer.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, and the refusals of [`Enrollment::approve`]; without
    /// enrollment, `unknown_request`.
    pub fn enrollment_approve(
        &self,
        caller: &Caller,
        request: proto::ApproveRequest,
    ) -> Result<proto::ApproveResponse, EnrollmentRefusal> {
        let by = caller.operator()?;
        let enrollment: &Enrollment = self.enrollment().ok_or(EnrollmentRefusal::UnknownRequest)?;
        let known = |name: &str| self.services.get(name).is_some();
        let (role, services) = (request.role.clone(), request.services.clone());
        let records = self.ops.read(|state| state.inventory.clone());
        let approved = enrollment.approve(by, request, &known, &records)?;
        self.assign_approved(by, approved.host_id.as_str(), &role, &services);
        Ok(proto::ApproveResponse {
            host_id: approved.host_id.to_string(),
            certificate_issued_at_poll: true,
            entry: approved.entry.as_ref().map(entry_proto),
        })
    }

    /// `Enroll` of `hostkeeper.agent.v1.Coordinator` over `enrollment`, this coordinator's
    /// ([`Enrollment::enrolled`]): when a Mac's request is approved by its nonce, the services
    /// of its allowlist entry are assigned to its host by `mdm-nonce`, as an operator's
    /// approval assigns the services it names. A request still pending (a new one or a repost)
    /// is tried against the host inventory ([`Enrollment::approve_by_record`]); one it approves
    /// is answered as one already approved (`UNSPECIFIED`: poll it), as a Mac's by its nonce.
    ///
    /// # Errors
    /// As [`Enrollment::enrolled`].
    pub fn enroll(
        &self,
        enrollment: &Enrollment,
        request: EnrollRequest,
    ) -> Result<EnrollResponse, RpcError> {
        let mut enrolled = enrollment.enrolled(request)?;
        if enrolled.response.state == EnrollState::Pending as i32 {
            self.approve_posted(enrollment, &enrolled.request_id, &mut enrolled.response);
        }
        if let Some(entry) = &enrolled.by_nonce {
            let services: Vec<String> = entry
                .services
                .iter()
                .map(|service| service.as_str().to_owned())
                .collect();
            self.assign_approved(
                MAC_APPROVER,
                entry.host_id.as_str(),
                entry.role.as_str(),
                &services,
            );
        }
        Ok(enrolled.response)
    }

    /// Tries the pending request `request_id` against the host inventory; when the record
    /// approves it the answer `response` becomes the one of a request already approved.
    fn approve_posted(
        &self,
        enrollment: &Enrollment,
        request_id: &KeyHash,
        response: &mut EnrollResponse,
    ) {
        let records = self.ops.read(|state| state.inventory.clone());
        if let ByRecord::Approved { .. } = enrollment.approve_by_record(request_id, &records) {
            response.state = EnrollState::Unspecified as i32;
        }
    }

    /// Assigns `services` to `host_id` of `role`, just approved by `by`
    /// ([`Coordinator::assign_host`]). The approval checked the host id, the role and the
    /// services by the same rules; an assignment refused is logged and leaves the approval
    /// standing.
    fn assign_approved(&self, by: &str, host_id: &str, role: &str, services: &[String]) {
        let assigned = host_id
            .parse::<HostId>()
            .ok()
            .zip(role.parse::<RoleName>().ok())
            .map(|(host, role)| self.assign_host(by, &host, Some(&role), services));
        if let Some(Err(refusal)) = assigned {
            tracing::error!(
                host_id,
                refusal = refusal.as_str_name(),
                "the services of an approval were not assigned: assign them with Services.Assign"
            );
        }
    }

    /// `Enrollment.Reject`: rejects a pending request ([`Enrollment::reject`]); the answer
    /// carries the `enroll_reject` entry of the first rejection, none for a repeat.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, and the refusals of [`Enrollment::reject`]; without
    /// enrollment, `unknown_request`.
    pub fn enrollment_reject(
        &self,
        caller: &Caller,
        request: &proto::RejectRequest,
    ) -> Result<proto::RejectResponse, EnrollmentRefusal> {
        let by = caller.operator()?;
        let enrollment = self.enrollment().ok_or(EnrollmentRefusal::UnknownRequest)?;
        let entry = enrollment.reject(by, request)?;
        Ok(proto::RejectResponse {
            entry: entry.as_ref().map(entry_proto),
        })
    }
}
