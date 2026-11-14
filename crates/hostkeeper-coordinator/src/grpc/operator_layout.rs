//! `Layout` of `hostkeeper.operator.v1` over [`Coordinator`].
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::operator_layout`] with the caller the router admitted, and a refusal is the status
//! [`refusal::layout`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::layout_server::Layout as Api;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `Layout`.
pub(crate) struct LayoutService {
    coordinator: Arc<Coordinator>,
}

impl LayoutService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl Api for LayoutService {
    async fn get(
        &self,
        request: Request<proto::GetLayoutRequest>,
    ) -> Result<Response<proto::GetLayoutResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_get(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn put_name(
        &self,
        request: Request<proto::PutLayoutNameRequest>,
    ) -> Result<Response<proto::PutLayoutNameResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_put_name(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn add_row(
        &self,
        request: Request<proto::AddLayoutRowRequest>,
    ) -> Result<Response<proto::AddLayoutRowResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_add_row(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn remove_row(
        &self,
        request: Request<proto::RemoveLayoutRowRequest>,
    ) -> Result<Response<proto::RemoveLayoutRowResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_remove_row(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn put_rack(
        &self,
        request: Request<proto::PutLayoutRackRequest>,
    ) -> Result<Response<proto::PutLayoutRackResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_put_rack(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn remove_rack(
        &self,
        request: Request<proto::RemoveLayoutRackRequest>,
    ) -> Result<Response<proto::RemoveLayoutRackResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_remove_rack(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn place(
        &self,
        request: Request<proto::PlaceLayoutRequest>,
    ) -> Result<Response<proto::PlaceLayoutResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_place(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn unplace(
        &self,
        request: Request<proto::UnplaceLayoutRequest>,
    ) -> Result<Response<proto::UnplaceLayoutResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_unplace(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn put_device(
        &self,
        request: Request<proto::PutLayoutDeviceRequest>,
    ) -> Result<Response<proto::PutLayoutDeviceResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_put_device(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn remove_device(
        &self,
        request: Request<proto::RemoveLayoutDeviceRequest>,
    ) -> Result<Response<proto::RemoveLayoutDeviceResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_remove_device(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn put_sled(
        &self,
        request: Request<proto::PutLayoutSledRequest>,
    ) -> Result<Response<proto::PutLayoutSledResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_put_sled(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }

    async fn remove_sled(
        &self,
        request: Request<proto::RemoveLayoutSledRequest>,
    ) -> Result<Response<proto::RemoveLayoutSledResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .layout_remove_sled(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::layout)
    }
}
