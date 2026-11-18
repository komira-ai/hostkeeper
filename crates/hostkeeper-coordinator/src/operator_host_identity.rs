//! `HostIdentity.Get` and `HostIdentity.Approve` of `hostkeeper.operator.v1`, as plain functions
//! over their messages, as the other operator calls are ([`crate::operator`]): whether the
//! coordinator signs service certificates for a host, and an operator's approval of a host the
//! coordinator approved itself (`docs/design/service-identity.md`, section 3).
//!
//! They take the operator's credential only: a valid CI token is `forbidden`, and no credential
//! `unauthorized`, before the request is looked at. The work is [`Enrollment::identity_get`] and
//! [`Enrollment::identity_approve`], which audits the approval `identity_approve` under the name of
//! the operator's token, about the host, before it is written. A coordinator without
//! `--enrollment-dir` has no host: `unknown_host`.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostIdentityRefusal;

use crate::operator::{Caller, Denied};
use crate::service::Coordinator;

impl From<Denied> for HostIdentityRefusal {
    fn from(denied: Denied) -> Self {
        match denied {
            Denied::Unauthorized => Self::Unauthorized,
            Denied::Forbidden => Self::Forbidden,
        }
    }
}

impl Coordinator {
    /// `HostIdentity.Get`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, and the refusals of [`Enrollment::identity_get`]; without
    /// enrollment, `unknown_host`.
    ///
    /// [`Enrollment::identity_get`]: crate::enrollment::Enrollment::identity_get
    pub fn host_identity_get(
        &self,
        caller: &Caller,
        request: &proto::GetHostIdentityRequest,
    ) -> Result<proto::GetHostIdentityResponse, HostIdentityRefusal> {
        caller.operator()?;
        self.enrollment()
            .ok_or(HostIdentityRefusal::UnknownHost)?
            .identity_get(request)
    }

    /// `HostIdentity.Approve`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, and the refusals of [`Enrollment::identity_approve`]; without
    /// enrollment, `unknown_host`.
    ///
    /// [`Enrollment::identity_approve`]: crate::enrollment::Enrollment::identity_approve
    pub fn host_identity_approve(
        &self,
        caller: &Caller,
        request: &proto::ApproveHostIdentityRequest,
    ) -> Result<proto::ApproveHostIdentityResponse, HostIdentityRefusal> {
        let by = caller.operator()?;
        self.enrollment()
            .ok_or(HostIdentityRefusal::UnknownHost)?
            .identity_approve(by, request)
    }
}
