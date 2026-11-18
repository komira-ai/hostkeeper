//! `HostIdentity` of `hostkeeper.operator.v1` over [`Coordinator`].
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::operator_host_identity`] with the caller the router admitted, and a refusal is the
//! status [`refusal::host_identity`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::host_identity_server::HostIdentity as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `HostIdentity`.
pub(crate) struct HostIdentityService {
    coordinator: Arc<Coordinator>,
}

impl HostIdentityService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for HostIdentityService {
    async fn get(
        &self,
        request: Request<proto::GetHostIdentityRequest>,
    ) -> Result<Response<proto::GetHostIdentityResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_identity_get(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::host_identity)
    }

    async fn approve(
        &self,
        request: Request<proto::ApproveHostIdentityRequest>,
    ) -> Result<Response<proto::ApproveHostIdentityResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_identity_approve(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::host_identity)
    }
}
