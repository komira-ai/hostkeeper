//! `Mdm` of `hostkeeper.operator.v1` over [`Coordinator`]: the devices of the gate, the setup
//! (secrets and settings), the push certificate, the ABM token and key, and the enrollment
//! requests that wait for an operator.
//!
//! The adapter has no rule of its own: each method calls its handler on the coordinator with the
//! caller the router admitted (`crate::operator_mdm*`), and a refusal is the status
//! [`refusal::mdm`] makes of it.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::mdm_server::Mdm as MdmApi;
use tonic::{Request, Response, Status};

use super::operator::caller_of;
use super::refusal;
use crate::service::Coordinator;

/// The service of `Mdm`.
pub(crate) struct MdmService {
    coordinator: Arc<Coordinator>,
}

impl MdmService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl MdmApi for MdmService {
    async fn list_devices(
        &self,
        request: Request<proto::ListDevicesRequest>,
    ) -> Result<Response<proto::ListDevicesResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_list_devices(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn put_secret(
        &self,
        request: Request<proto::PutSecretRequest>,
    ) -> Result<Response<proto::PutSecretResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_put_secret(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn put_setting(
        &self,
        request: Request<proto::PutSettingRequest>,
    ) -> Result<Response<proto::PutSettingResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_put_setting(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn list_secrets(
        &self,
        request: Request<proto::ListSecretsRequest>,
    ) -> Result<Response<proto::ListSecretsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_list_secrets(&caller)
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn list_settings(
        &self,
        request: Request<proto::ListSettingsRequest>,
    ) -> Result<Response<proto::ListSettingsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_list_settings(&caller)
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn create_push_request(
        &self,
        request: Request<proto::CreatePushRequestRequest>,
    ) -> Result<Response<proto::CreatePushRequestResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_create_push_request(&caller, request.into_inner())
            .await
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn put_abm_token(
        &self,
        request: Request<proto::PutAbmTokenRequest>,
    ) -> Result<Response<proto::PutAbmTokenResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_put_abm_token(&caller, request.into_inner())
            .await
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn put_push_reply(
        &self,
        request: Request<proto::PutPushReplyRequest>,
    ) -> Result<Response<proto::PutPushReplyResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_put_push_reply(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn put_push_certificate(
        &self,
        request: Request<proto::PutPushCertificateRequest>,
    ) -> Result<Response<proto::PutPushCertificateResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_put_push_certificate(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn list_enrollment_requests(
        &self,
        request: Request<proto::ListEnrollmentRequestsRequest>,
    ) -> Result<Response<proto::ListEnrollmentRequestsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_list_enrollment_requests(&caller)
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn confirm_enrollment(
        &self,
        request: Request<proto::ConfirmEnrollmentRequest>,
    ) -> Result<Response<proto::ConfirmEnrollmentResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_confirm_enrollment(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::mdm)
    }

    async fn get_abm_public_key(
        &self,
        request: Request<proto::GetAbmPublicKeyRequest>,
    ) -> Result<Response<proto::GetAbmPublicKeyResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mdm_get_abm_public_key(&caller, request.get_ref())
            .await
            .map(Response::new)
            .map_err(refusal::mdm)
    }
}
