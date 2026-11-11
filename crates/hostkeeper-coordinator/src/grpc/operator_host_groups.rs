//! `HostGroups` of `hostkeeper.operator.v1` over [`Coordinator`]: `List`, `Get`, `Put`, `Remove`
//! and `Move`.
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::operator_host_groups`] or [`crate::host_move`] with the caller the router admitted,
//! and a refusal is the status [`refusal::host_groups`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::host_groups_server::HostGroups as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `HostGroups`.
pub(crate) struct HostGroupsService {
    coordinator: Arc<Coordinator>,
}

impl HostGroupsService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for HostGroupsService {
    async fn list(
        &self,
        request: Request<proto::ListHostGroupsRequest>,
    ) -> Result<Response<proto::ListHostGroupsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_groups_list(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::host_groups)
    }

    async fn get(
        &self,
        request: Request<proto::GetHostGroupRequest>,
    ) -> Result<Response<proto::GetHostGroupResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_groups_get(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::host_groups)
    }

    async fn put(
        &self,
        request: Request<proto::PutHostGroupRequest>,
    ) -> Result<Response<proto::PutHostGroupResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_groups_put(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::host_groups)
    }

    async fn remove(
        &self,
        request: Request<proto::RemoveHostGroupRequest>,
    ) -> Result<Response<proto::RemoveHostGroupResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_groups_remove(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::host_groups)
    }

    async fn r#move(
        &self,
        request: Request<proto::MoveHostRequest>,
    ) -> Result<Response<proto::MoveHostResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .host_groups_move(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::host_groups)
    }
}
