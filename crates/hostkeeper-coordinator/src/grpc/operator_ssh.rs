//! `Ssh` of `hostkeeper.operator.v1` over [`Coordinator`]: `SignUserCert`, `Revoke`, `HostKeys`,
//! `PinHostKeys` and the stream `Tunnel`, which the coordinator answers once it holds an SSH user
//! CA (`--ssh-ca-secret-file`) and, for the last three, tunnels
//! (docs/design/ssh-through-coordinator.md); a coordinator without them answers what
//! `SshRefusal` says of one that cannot: `not_configured`, `FAILED_PRECONDITION`.
//!
//! The router admits the caller as an operator first (a caller with no credential, or CI's, is
//! refused before it gets here). The adapter has no rule of its own: each method calls its
//! handler in [`crate::ssh_sign`] or [`crate::ssh_access`] with the caller the router admitted,
//! and a refusal is the status [`refusal::ssh`] makes of it. The handlers that write to the book
//! run off the runtime's threads. A refused `Tunnel` is a status before any frame is answered;
//! the stream of an opened one is the relay's.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ssh_server::Ssh as Api;
use tonic::{Request, Response, Status, Streaming};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;
use crate::ssh_tunnel::Outgoing;
use crate::wait::ConnectionWaits;

/// The service of `Ssh`.
pub(crate) struct SshService {
    coordinator: Arc<Coordinator>,
}

impl SshService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

/// What a call that needs a handler off the runtime's threads answers when the handler did not
/// finish: the change could not be made.
fn not_finished(why: &tokio::task::JoinError) -> Status {
    tracing::error!(%why, "a call of Ssh did not finish");
    refusal::ssh(proto::SshRefusal::StoreFailed)
}

#[tonic::async_trait]
impl Api for SshService {
    async fn sign_user_cert(
        &self,
        request: Request<proto::SignUserCertRequest>,
    ) -> Result<Response<proto::SignUserCertResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .ssh_sign_user_cert(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::ssh)
    }

    async fn host_keys(
        &self,
        request: Request<proto::HostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        let caller = caller_of(&request);
        let coordinator = Arc::clone(&self.coordinator);
        tokio::task::spawn_blocking(move || coordinator.ssh_host_keys(&caller, request.get_ref()))
            .await
            .map_err(|why| not_finished(&why))?
            .map(Response::new)
            .map_err(refusal::ssh)
    }

    async fn pin_host_keys(
        &self,
        request: Request<proto::PinHostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        let caller = caller_of(&request);
        let coordinator = Arc::clone(&self.coordinator);
        tokio::task::spawn_blocking(move || {
            coordinator.ssh_pin_host_keys(&caller, request.get_ref())
        })
        .await
        .map_err(|why| not_finished(&why))?
        .map(Response::new)
        .map_err(refusal::ssh)
    }

    async fn revoke(
        &self,
        request: Request<proto::RevokeRequest>,
    ) -> Result<Response<proto::RevokeResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .ssh_revoke(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::ssh)
    }

    type TunnelStream = Outgoing;

    async fn tunnel(
        &self,
        request: Request<Streaming<proto::TunnelFrame>>,
    ) -> Result<Response<Self::TunnelStream>, Status> {
        let caller = caller_of(&request);
        // The connection is kept for as long as the relay runs, as for a held wait.
        let hold = request
            .extensions()
            .get::<ConnectionWaits>()
            .map(ConnectionWaits::hold);
        self.coordinator
            .ssh_tunnel_open(&caller, request.into_inner(), hold)
            .await
            .map(Response::new)
            .map_err(refusal::ssh)
    }
}
