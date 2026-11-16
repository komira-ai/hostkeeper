//! `Ssh` of `hostkeeper.operator.v1` over [`Coordinator`]: `SignUserCert` and `Revoke`, which
//! the coordinator answers once it holds an SSH user CA (`--ssh-ca-secret-file`), and `HostKeys`
//! and `Tunnel`, which are later changes (docs/design/ssh-through-coordinator.md, item C4) and
//! answer what `SshRefusal` says of a coordinator that cannot: `not_configured`,
//! `FAILED_PRECONDITION`.
//!
//! The router admits the caller as an operator first (a caller with no credential, or CI's, is
//! refused before it gets here). The adapter has no rule of its own: each method calls its
//! handler in [`crate::ssh_sign`] with the caller the router admitted, and a refusal is the
//! status [`refusal::ssh`] makes of it.

use std::sync::Arc;

use futures::stream::Empty;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ssh_server::Ssh as Api;
use tonic::{Request, Response, Status, Streaming};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

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

/// What a method that waits for the tunnel's change answers: the coordinator cannot do it yet.
fn not_configured() -> Status {
    Status::failed_precondition("not_configured")
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
        _request: Request<proto::HostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        Err(not_configured())
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

    type TunnelStream = Empty<Result<proto::TunnelFrame, Status>>;

    async fn tunnel(
        &self,
        _request: Request<Streaming<proto::TunnelFrame>>,
    ) -> Result<Response<Self::TunnelStream>, Status> {
        Err(not_configured())
    }
}
