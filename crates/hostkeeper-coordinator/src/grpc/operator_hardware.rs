//! `Hardware` of `hostkeeper.operator.v1` over [`Coordinator`]: `Log`.
//!
//! The adapter has no rule of its own: the method calls its handler,
//! [`Coordinator::hardware_log`], with the caller the router admitted, and a refusal is the status
//! [`refusal::hardware`] makes of it. A call reads the log's file or table, so it runs off the
//! runtime's threads; a handler that did not finish is `store_failed`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HardwareRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::hardware_server::Hardware as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `Hardware`.
pub(crate) struct HardwareService {
    coordinator: Arc<Coordinator>,
}

impl HardwareService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for HardwareService {
    async fn log(
        &self,
        request: Request<proto::HardwareLogRequest>,
    ) -> Result<Response<proto::HardwareLogResponse>, Status> {
        let caller = caller_of(&request);
        let coordinator = Arc::clone(&self.coordinator);
        let message = request.into_inner();
        tokio::task::spawn_blocking(move || coordinator.hardware_log(&caller, &message))
            .await
            .unwrap_or(Err(HardwareRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::hardware)
    }
}
