//! `Console` of `hostkeeper.operator.v1` over [`Coordinator`].
//!
//! The adapter has no rule of its own: each method calls its handler in [`crate::console`] with
//! the caller the router admitted, off the runtime's threads (a power call runs the BMC tool and
//! waits for it, and every call that acts appends an audit line), and a refusal is the status
//! [`refusal::console`] makes of it; a handler that did not finish is `store_failed`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::console_server::Console as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::operator::Caller;
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

    /// `call` of `request`, off the runtime's threads.
    async fn run<Q, A>(
        &self,
        request: Request<Q>,
        call: fn(&Coordinator, &Caller, &Q) -> Result<A, ConsoleRefusal>,
    ) -> Result<Response<A>, Status>
    where
        Q: Send + 'static,
        A: Send + 'static,
    {
        let caller = caller_of(&request);
        let coordinator = Arc::clone(&self.coordinator);
        let message = request.into_inner();
        tokio::task::spawn_blocking(move || call(&coordinator, &caller, &message))
            .await
            .unwrap_or(Err(ConsoleRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::console)
    }
}

#[tonic::async_trait]
impl Api for ConsoleService {
    async fn open(
        &self,
        request: Request<proto::OpenConsoleRequest>,
    ) -> Result<Response<proto::OpenConsoleResponse>, Status> {
        self.run(request, Coordinator::console_open).await
    }

    async fn power(
        &self,
        request: Request<proto::ConsolePowerRequest>,
    ) -> Result<Response<proto::ConsolePowerResponse>, Status> {
        self.run(request, Coordinator::console_power).await
    }

    async fn set_boot_device(
        &self,
        request: Request<proto::SetBootDeviceRequest>,
    ) -> Result<Response<proto::SetBootDeviceResponse>, Status> {
        self.run(request, Coordinator::console_set_boot_device)
            .await
    }

    async fn list(
        &self,
        request: Request<proto::ListConsoleSessionsRequest>,
    ) -> Result<Response<proto::ListConsoleSessionsResponse>, Status> {
        self.run(request, Coordinator::console_list).await
    }

    async fn close(
        &self,
        request: Request<proto::CloseConsoleRequest>,
    ) -> Result<Response<proto::CloseConsoleResponse>, Status> {
        self.run(request, Coordinator::console_close).await
    }
}
