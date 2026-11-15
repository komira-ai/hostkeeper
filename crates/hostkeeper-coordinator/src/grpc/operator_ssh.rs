//! `Ssh` of `hostkeeper.operator.v1`, until the coordinator holds an SSH user CA.
//!
//! The wire of the service is in the protos (`operator/v1/ssh.proto`); the CA, the signing and the
//! tunnel are later changes (docs/design/ssh-through-coordinator.md, items C2 and C4). Until they
//! land every method answers what `SshRefusal` says of a coordinator with no CA: `not_configured`,
//! `FAILED_PRECONDITION`, after the router has admitted the caller as an operator (a caller with
//! no credential, or CI's, is refused before it gets here). Nothing is signed, dialled or
//! recorded, so no method can change anything.

use futures::stream::Empty;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ssh_server::Ssh as Api;
use tonic::{Request, Response, Status, Streaming};

/// The service of `Ssh`.
pub(crate) struct SshService;

/// What every method answers: the coordinator holds no SSH user CA.
fn not_configured() -> Status {
    Status::failed_precondition("not_configured")
}

#[tonic::async_trait]
impl Api for SshService {
    async fn sign_user_cert(
        &self,
        _request: Request<proto::SignUserCertRequest>,
    ) -> Result<Response<proto::SignUserCertResponse>, Status> {
        Err(not_configured())
    }

    async fn host_keys(
        &self,
        _request: Request<proto::HostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        Err(not_configured())
    }

    async fn revoke(
        &self,
        _request: Request<proto::RevokeRequest>,
    ) -> Result<Response<proto::RevokeResponse>, Status> {
        Err(not_configured())
    }

    type TunnelStream = Empty<Result<proto::TunnelFrame, Status>>;

    async fn tunnel(
        &self,
        _request: Request<Streaming<proto::TunnelFrame>>,
    ) -> Result<Response<Self::TunnelStream>, Status> {
        Err(not_configured())
    }
}
