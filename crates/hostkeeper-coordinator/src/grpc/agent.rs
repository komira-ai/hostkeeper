//! `hostkeeper.agent.v1.Coordinator` over [`Coordinator`]: `Heartbeat`, `Enroll`, `Poll` and
//! `Renew`.
//!
//! The adapter has no rule of its own. `Heartbeat` is turned into the JSON the frozen route
//! reads ([`super::beat_map`]) and answered by the same code ([`crate::http::beat`]), with what
//! it says of the host's trusted root, which the JSON has no key for, beside it
//! ([`crate::trusted_root`]); `Enroll`,
//! `Poll` and `Renew` are the handlers of [`crate::enrollment`]. Who calls was settled by the
//! router before the request was read ([`super::admit`]) and arrives in the request's
//! extensions; a refusal is a status whose message is exactly one token of `CoordinatorRefusal`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::Coordinator as Api;
use hostkeeper_wire::wire::Answer;
use tonic::{Code, Request, Response, Status};

use super::Admitted;
use super::beat_map;
use super::refusal::token_status;
use crate::enrollment::Enrollment;
use crate::http::{BeatCaller, BeatRefused, beat};
use crate::orders::Order;
use crate::service::Coordinator;
use crate::trusted_root::TrustedRootSaid;

/// The gRPC status of a refused heartbeat: the code and the token `CoordinatorRefusal` names.
pub(crate) fn beat_status(refused: BeatRefused) -> Status {
    match refused {
        BeatRefused::Forbidden => token_status(Code::PermissionDenied, "forbidden"),
        BeatRefused::Denied => token_status(Code::PermissionDenied, "denied"),
        BeatRefused::BadRequest => token_status(Code::InvalidArgument, "invalid_heartbeat"),
    }
}

/// The `HeartbeatResponse` for `answer`, and the typed order beside it when there is one.
fn response_of(answer: Answer, order: Option<Order>) -> proto::HeartbeatResponse {
    proto::HeartbeatResponse {
        order: order.as_ref().map(Order::to_proto),
        features: answer.features.unwrap_or_default(),
        halt_generations: answer.halts.into_iter().collect(),
        acks: crate::verdict_file::to_proto(&answer.acks),
    }
}

/// The service of `hostkeeper.agent.v1.Coordinator`.
pub(crate) struct AgentService {
    coordinator: Arc<Coordinator>,
}

impl AgentService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// The coordinator's enrollment, or the status of a coordinator that has none:
    /// `UNAVAILABLE`, with no token.
    fn enrollment(&self) -> Result<&Arc<Enrollment>, Status> {
        self.coordinator
            .enrollment()
            .ok_or_else(|| Status::unavailable(""))
    }
}

#[tonic::async_trait]
impl Api for AgentService {
    async fn heartbeat(
        &self,
        request: Request<proto::HeartbeatRequest>,
    ) -> Result<Response<proto::HeartbeatResponse>, Status> {
        let Some(Admitted::Host { identity, retiring }) =
            request.extensions().get::<Admitted>().cloned()
        else {
            return Err(beat_status(BeatRefused::Forbidden));
        };
        let bytes = beat_map::json(request.get_ref(), self.coordinator.legacy_service());
        let root = Some(TrustedRootSaid::of(request.get_ref()));
        let caller = BeatCaller { identity, retiring };
        let (answer, order) = beat(&self.coordinator, caller, &bytes, root).map_err(beat_status)?;
        Ok(Response::new(response_of(answer, order)))
    }

    async fn enroll(
        &self,
        request: Request<proto::EnrollRequest>,
    ) -> Result<Response<proto::EnrollResponse>, Status> {
        let enrollment = self.enrollment()?;
        self.coordinator
            .enroll(enrollment, request.into_inner())
            .map(Response::new)
            .map_err(|why| why.status())
    }

    async fn poll(
        &self,
        request: Request<proto::PollRequest>,
    ) -> Result<Response<proto::PollResponse>, Status> {
        self.enrollment()?
            .poll(request.into_inner())
            .map(Response::new)
            .map_err(|why| why.status())
    }

    async fn renew(
        &self,
        request: Request<proto::RenewRequest>,
    ) -> Result<Response<proto::RenewResponse>, Status> {
        let peer = match request.extensions().get::<Admitted>() {
            Some(Admitted::Peer(peer)) => peer.clone(),
            _ => None,
        };
        self.enrollment()?
            .renew(peer.as_deref(), request.into_inner())
            .map(Response::new)
            .map_err(|why| why.status())
    }
}
