//! `Users` and `ApiTokens` of `hostkeeper.operator.v1` over [`Coordinator`].
//!
//! The adapter has no rule of its own: each method calls its handler in
//! [`crate::operator_accounts`] with the caller the router admitted, and a refusal is the status
//! [`refusal::users`] or [`refusal::api_tokens`] makes of it. A call that makes a password or a
//! token hashes or draws on a thread of its own, off the runtime's.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::api_tokens_server::ApiTokens as TokensApi;
use hostkeeper_proto::hostkeeper::operator::v1::users_server::Users as UsersApi;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::operator::Caller;
use crate::service::Coordinator;

/// The service of `Users`.
pub(crate) struct UsersService {
    coordinator: Arc<Coordinator>,
}

/// The service of `ApiTokens`.
pub(crate) struct ApiTokensService {
    coordinator: Arc<Coordinator>,
}

impl UsersService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// What `call` makes of `request` as `caller`, run off the runtime's threads: a password is
    /// hashed on the way. A task that did not finish is `INTERNAL`.
    async fn run<Q, A>(
        &self,
        request: Request<Q>,
        call: fn(&Coordinator, &Caller, &Q) -> Result<A, proto::UsersRefusal>,
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
            .unwrap_or(Err(proto::UsersRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::users)
    }
}

impl ApiTokensService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// What `call` makes of `request` as `caller`, run off the runtime's threads.
    async fn run<Q, A>(
        &self,
        request: Request<Q>,
        call: fn(&Coordinator, &Caller, &Q) -> Result<A, proto::ApiTokensRefusal>,
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
            .unwrap_or(Err(proto::ApiTokensRefusal::StoreFailed))
            .map(Response::new)
            .map_err(refusal::api_tokens)
    }
}

#[tonic::async_trait]
impl UsersApi for UsersService {
    async fn list(
        &self,
        request: Request<proto::ListUsersRequest>,
    ) -> Result<Response<proto::ListUsersResponse>, Status> {
        self.run(request, Coordinator::users_list).await
    }

    async fn create(
        &self,
        request: Request<proto::CreateUserRequest>,
    ) -> Result<Response<proto::CreateUserResponse>, Status> {
        self.run(request, Coordinator::users_create).await
    }

    async fn disable(
        &self,
        request: Request<proto::DisableUserRequest>,
    ) -> Result<Response<proto::DisableUserResponse>, Status> {
        self.run(request, Coordinator::users_disable).await
    }

    async fn enable(
        &self,
        request: Request<proto::EnableUserRequest>,
    ) -> Result<Response<proto::EnableUserResponse>, Status> {
        self.run(request, Coordinator::users_enable).await
    }

    async fn reset_password(
        &self,
        request: Request<proto::ResetPasswordRequest>,
    ) -> Result<Response<proto::ResetPasswordResponse>, Status> {
        self.run(request, Coordinator::users_reset_password).await
    }

    async fn delete(
        &self,
        request: Request<proto::DeleteUserRequest>,
    ) -> Result<Response<proto::DeleteUserResponse>, Status> {
        self.run(request, Coordinator::users_delete).await
    }
}

#[tonic::async_trait]
impl TokensApi for ApiTokensService {
    async fn list(
        &self,
        request: Request<proto::ListApiTokensRequest>,
    ) -> Result<Response<proto::ListApiTokensResponse>, Status> {
        self.run(request, Coordinator::api_tokens_list).await
    }

    async fn create(
        &self,
        request: Request<proto::CreateApiTokenRequest>,
    ) -> Result<Response<proto::CreateApiTokenResponse>, Status> {
        self.run(request, Coordinator::api_tokens_create).await
    }

    async fn revoke(
        &self,
        request: Request<proto::RevokeApiTokenRequest>,
    ) -> Result<Response<proto::RevokeApiTokenResponse>, Status> {
        self.run(request, Coordinator::api_tokens_revoke).await
    }
}
