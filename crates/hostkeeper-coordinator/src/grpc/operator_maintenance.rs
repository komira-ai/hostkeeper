//! `Maintenance` of `hostkeeper.operator.v1` over [`Coordinator`]: `Schedule`, `List`, `Get`,
//! `Cancel`, `RetryHost` and `PutBack`.
//!
//! The adapter has no rule of its own: each of those methods calls its handler in
//! `crate::maintenance_service` (`crate::maintenance_operator` for `RetryHost` and `PutBack`) with
//! the caller the router admitted, and a refusal is the status [`refusal::maintenance`] makes of
//! it. A call reads or writes the store, so it runs off the runtime's threads; a handler that did
//! not finish is `store_failed`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::maintenance_server::Maintenance as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::operator::Caller;
use crate::service::Coordinator;

/// `Maintenance.List` of `message`, which its handler takes by reference.
fn list_of(
    coordinator: &Coordinator,
    caller: &Caller,
    message: proto::ListMaintenanceRunsRequest,
) -> Result<proto::ListMaintenanceRunsResponse, MaintenanceRefusal> {
    coordinator.maintenance_list(caller, &message)
}

/// `Maintenance.Get` of `message`, which its handler takes by reference.
fn get_of(
    coordinator: &Coordinator,
    caller: &Caller,
    message: proto::GetMaintenanceRunRequest,
) -> Result<proto::GetMaintenanceRunResponse, MaintenanceRefusal> {
    coordinator.maintenance_get(caller, &message)
}

/// The service of `Maintenance`.
pub(crate) struct MaintenanceService {
    coordinator: Arc<Coordinator>,
}

impl MaintenanceService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// `call` of `request`, off the runtime's threads.
    async fn run<Q, A>(
        &self,
        request: Request<Q>,
        call: fn(&Coordinator, &Caller, Q) -> Result<A, MaintenanceRefusal>,
    ) -> Result<Response<A>, Status>
    where
        Q: Send + 'static,
        A: Send + 'static,
    {
        let caller = caller_of(&request);
        let coordinator = Arc::clone(&self.coordinator);
        let message = request.into_inner();
        tokio::task::spawn_blocking(move || call(&coordinator, &caller, message))
            .await
            .unwrap_or(Err(MaintenanceRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::maintenance)
    }
}

#[tonic::async_trait]
impl Api for MaintenanceService {
    async fn schedule(
        &self,
        request: Request<proto::ScheduleMaintenanceRequest>,
    ) -> Result<Response<proto::ScheduleMaintenanceResponse>, Status> {
        self.run(request, Coordinator::maintenance_schedule).await
    }

    async fn list(
        &self,
        request: Request<proto::ListMaintenanceRunsRequest>,
    ) -> Result<Response<proto::ListMaintenanceRunsResponse>, Status> {
        self.run(request, list_of).await
    }

    async fn get(
        &self,
        request: Request<proto::GetMaintenanceRunRequest>,
    ) -> Result<Response<proto::GetMaintenanceRunResponse>, Status> {
        self.run(request, get_of).await
    }

    async fn cancel(
        &self,
        request: Request<proto::CancelMaintenanceRunRequest>,
    ) -> Result<Response<proto::CancelMaintenanceRunResponse>, Status> {
        self.run(request, Coordinator::maintenance_cancel).await
    }

    async fn retry_host(
        &self,
        request: Request<proto::RetryMaintenanceHostRequest>,
    ) -> Result<Response<proto::RetryMaintenanceHostResponse>, Status> {
        self.run(request, Coordinator::maintenance_retry_host).await
    }

    async fn put_back(
        &self,
        request: Request<proto::PutBackMaintenanceHostRequest>,
    ) -> Result<Response<proto::PutBackMaintenanceHostResponse>, Status> {
        self.run(request, Coordinator::maintenance_put_back).await
    }
}
