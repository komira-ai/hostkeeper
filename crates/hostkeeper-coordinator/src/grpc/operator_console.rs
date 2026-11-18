//! `Console` of `hostkeeper.operator.v1` over [`Coordinator`].
//!
//! The adapter has no rule of its own: each method calls its handler in [`crate::console`] with
//! the caller the router admitted, and a refusal is the status [`refusal::console`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::console_server::Console as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `Console`.
pub(crate) struct ConsoleService {
    coordinator: Arc<Coordinator>,
}

impl ConsoleService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for ConsoleService {
    async fn open(
        &self,
        request: Request<proto::OpenConsoleRequest>,
    ) -> Result<Response<proto::OpenConsoleResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .console_open(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::console)
    }

    async fn power(
        &self,
        request: Request<proto::ConsolePowerRequest>,
    ) -> Result<Response<proto::ConsolePowerResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .console_power(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::console)
    }

    async fn set_boot_device(
        &self,
        request: Request<proto::SetBootDeviceRequest>,
    ) -> Result<Response<proto::SetBootDeviceResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .console_set_boot_device(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::console)
    }

    async fn list(
        &self,
        request: Request<proto::ListConsoleSessionsRequest>,
    ) -> Result<Response<proto::ListConsoleSessionsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .console_list(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::console)
    }

    async fn close(
        &self,
        request: Request<proto::CloseConsoleRequest>,
    ) -> Result<Response<proto::CloseConsoleResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .console_close(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::console)
    }
}
