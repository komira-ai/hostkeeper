//! `Repositories` of `hostkeeper.operator.v1` over [`Coordinator`]: `List`, `Get`, `Put` and
//! `Remove`.
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::operator_repositories`] with the caller the router admitted, and a refusal is the
//! status [`refusal::repositories`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::repositories_server::Repositories as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `Repositories`.
pub(crate) struct RepositoriesService {
    coordinator: Arc<Coordinator>,
}

impl RepositoriesService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for RepositoriesService {
    async fn list(
        &self,
        request: Request<proto::ListRepositoriesRequest>,
    ) -> Result<Response<proto::ListRepositoriesResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .repositories_list(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::repositories)
    }

    async fn get(
        &self,
        request: Request<proto::GetRepositoryRequest>,
    ) -> Result<Response<proto::GetRepositoryResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .repositories_get(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::repositories)
    }

    async fn put(
        &self,
        request: Request<proto::PutRepositoryRequest>,
    ) -> Result<Response<proto::PutRepositoryResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .repositories_put(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::repositories)
    }

    async fn remove(
        &self,
        request: Request<proto::RemoveRepositoryRequest>,
    ) -> Result<Response<proto::RemoveRepositoryResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .repositories_remove(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::repositories)
    }
}
