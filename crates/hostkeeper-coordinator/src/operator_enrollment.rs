//! `Enrollment.List` and `Enrollment.Approve` of `hostkeeper.operator.v1`, as plain functions
//! over its messages, as the other operator calls are ([`crate::operator`]). A transport (the
//! gRPC services, or the JSON gateway) finds the caller from the bearer
//! ([`Coordinator::caller`]) before it reads the request, and hands the call here.
//!
//! Both take the operator token only: a valid CI token is `forbidden`, and no token or any
//! other `unauthorized`, before the request is looked at; nothing is audited for them, since
//! no operator made the call. The work is [`Enrollment::list`] and [`Enrollment::approve`],
//! which audits each approval and each refused one under the name of the operator's token. A
//! coordinator without `--enrollment-dir` has no pending request.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use crate::enrollment::Enrollment;
use crate::operator::{Caller, entry_proto};
use crate::service::Coordinator;

impl Coordinator {
    /// `Enrollment.List`: the requests waiting for an operator ([`Enrollment::list`]).
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token), `store_failed`.
    pub fn enrollment_list(
        &self,
        caller: &Caller,
        request: &proto::ListPendingRequest,
    ) -> Result<proto::ListPendingResponse, EnrollmentRefusal> {
        caller.operator()?;
        match self.enrollment() {
            Some(enrollment) => enrollment.list(request),
            None => Ok(proto::ListPendingResponse::default()),
        }
    }

    /// `Enrollment.Approve`: approves a pending request as the host id, role and services it
    /// names, which must be services this coordinator manages ([`Enrollment::approve`]). The
    /// host gets its certificate at its next `Poll`, never in this answer.
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
        let approved = enrollment.approve(by, request, &known)?;
        Ok(proto::ApproveResponse {
            host_id: approved.host_id.to_string(),
            certificate_issued_at_poll: true,
            entry: approved.entry.as_ref().map(entry_proto),
        })
    }
}
