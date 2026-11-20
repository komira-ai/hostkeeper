//! `Xcode` of `hostkeeper.operator.v1` over [`Coordinator`]: `List` and `Remove`.
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::xcode_service`] with the caller the router admitted, and a refusal is the status
//! [`refusal::xcode`] makes of it. A call reads or removes files, so it runs off the runtime's
//! threads; a handler that did not finish is `store_failed`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::XcodeRefusal;
use hostkeeper_proto::hostkeeper::operator::v1::xcode_server::Xcode as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::operator::Caller;
use crate::service::Coordinator;

/// `Xcode.List` of `message`, which its handler takes by reference.
fn list_of(
    coordinator: &Coordinator,
    caller: &Caller,
    message: proto::ListXcodeRequest,
) -> Result<proto::ListXcodeResponse, XcodeRefusal> {
    coordinator.xcode_list(caller, &message)
}

/// The service of `Xcode`.
pub(crate) struct XcodeService {
    coordinator: Arc<Coordinator>,
}

impl XcodeService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// `call` of `request`, off the runtime's threads.
    async fn run<Q, A>(
        &self,
        request: Request<Q>,
        call: fn(&Coordinator, &Caller, Q) -> Result<A, XcodeRefusal>,
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
            .unwrap_or(Err(XcodeRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::xcode)
    }
}

#[tonic::async_trait]
impl Api for XcodeService {
    async fn list(
        &self,
        request: Request<proto::ListXcodeRequest>,
    ) -> Result<Response<proto::ListXcodeResponse>, Status> {
        self.run(request, list_of).await
    }

    async fn remove(
        &self,
        request: Request<proto::RemoveXcodeRequest>,
    ) -> Result<Response<proto::RemoveXcodeResponse>, Status> {
        self.run(request, Coordinator::xcode_remove).await
    }
}
