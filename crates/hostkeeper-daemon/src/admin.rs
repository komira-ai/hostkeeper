//! The admin socket's server: `hostkeeper.local.v1.Admin`, for root on the host (design section
//! 4.1).
//!
//! [`serve`] takes its connections as a stream of connections, each with the [`Peer`] it came
//! from, so that the server never opens a socket itself: [`crate::socket`] gives it the
//! connections of the real unix socket with the credentials the kernel reports
//! (`SO_PEERCRED`), and a test gives it in-process pipes with a peer it chooses. A connection
//! from any peer but uid 0, or from one whose credentials are unknown, is closed before a byte
//! of it is read, so no request of it is ever decoded.
//!
//! `Status` is served. `Abort` and `ClearRecord` answer `UNIMPLEMENTED` with the token
//! `not_implemented`.

use std::io;
use std::sync::Arc;

use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{FailureClass, FailurePhase};
use hostkeeper_proto::hostkeeper::local::v1::admin_server::{Admin, AdminServer};
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortRequest, AbortResponse, ClearRecordRequest, ClearRecordResponse, RecordProblem,
    ServiceRecord, StatusRequest, StatusResponse, StepOutcome, StepRecord,
};
use hostkeeper_steps::record::{Record, State};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_stream::{Stream, StreamExt};
use tonic::transport::server::Connected;
use tonic::transport::{Error, Server};
use tonic::{Request, Response, Status};

use crate::executor::{Daemon, RecordState, ServiceStatus};
use crate::refusal::Refusal;

/// Who is at the other end of a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The peer's uid, `None` when its credentials could not be read.
    pub uid: Option<u32>,
}

impl Peer {
    /// Whether the peer may be served: only uid 0.
    pub fn is_root(self) -> bool {
        self.uid == Some(0)
    }
}

/// The connection, when its peer may be served; a connection of any other peer is dropped here,
/// closed before anything is read from it.
fn admit<IO>(accepted: io::Result<(IO, Peer)>) -> Option<io::Result<IO>> {
    match accepted {
        Ok((io, peer)) => peer.is_root().then_some(Ok(io)),
        Err(e) => Some(Err(e)),
    }
}

/// Serves `Admin` on every connection of `incoming` whose peer is root, until `incoming` ends
/// and its connections have closed.
pub async fn serve<S, IO>(daemon: Arc<Daemon>, incoming: S) -> Result<(), Error>
where
    S: Stream<Item = io::Result<(IO, Peer)>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
{
    let admitted = incoming.filter_map(admit);
    Server::builder()
        .serve_with_incoming(AdminServer::new(AdminService { daemon }), admitted)
        .await
}

/// The `Admin` service over the daemon.
pub struct AdminService {
    daemon: Arc<Daemon>,
}

fn not_implemented() -> Status {
    Status::unimplemented(Refusal::NotImplemented.token())
}

#[tonic::async_trait]
impl Admin for AdminService {
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let status = self
            .daemon
            .status()
            .map_err(|_| Status::internal(Refusal::StoreFailed.token()))?;
        let services = status.services.iter().map(service_record).collect();
        Ok(Response::new(StatusResponse { services }))
    }

    async fn abort(
        &self,
        _request: Request<AbortRequest>,
    ) -> Result<Response<AbortResponse>, Status> {
        Err(not_implemented())
    }

    async fn clear_record(
        &self,
        _request: Request<ClearRecordRequest>,
    ) -> Result<Response<ClearRecordResponse>, Status> {
        Err(not_implemented())
    }
}

/// One service as `Status` reports it. What the service manager says of it (`state`) is not
/// observed by this daemon yet and is left out. A service whose step could not be stopped is
/// under way, with the reason `unstoppable_unit`.
pub fn service_record(status: &ServiceStatus) -> ServiceRecord {
    let (mut current, last, problem) = match &status.record {
        RecordState::Absent => (None, None, RecordProblem::Unspecified),
        RecordState::Unfinished(rec) => (Some(step_record(rec)), None, RecordProblem::Unspecified),
        RecordState::Finished(rec) => (None, Some(step_record(rec)), RecordProblem::Unspecified),
        RecordState::Unreadable(_) => (None, None, RecordProblem::Unreadable),
        RecordState::Newer(_) => (None, None, RecordProblem::NewerSchema),
    };
    // The step could not be stopped: the update stays under way, and says why.
    if let (true, Some(current)) = (status.stuck, current.as_mut()) {
        current.reason = crate::boot::ALERT_UNSTOPPABLE.to_owned();
    }
    ServiceRecord {
        service: status.service.to_string(),
        current,
        last,
        state: None,
        record_problem: problem as i32,
    }
}

/// An order as its step record holds it. The record keeps no order id and no wall-clock times.
pub fn step_record(rec: &Record) -> StepRecord {
    let mut out = StepRecord {
        order_try: rec.order_try.unwrap_or(0),
        version: rec.target_version.clone(),
        package_digest: rec.tarball_digest.clone(),
        step: rec.step.name().to_owned(),
        attempt: rec.attempt,
        committed: rec.progress.committed,
        ..StepRecord::default()
    };
    if let State::Finished { verdict, reason } = &rec.state {
        out.reason = reason.clone().unwrap_or_default();
        verdict_into(verdict, &mut out);
    }
    out
}

/// A verdict in the fields of a step record.
pub fn verdict_into(verdict: &Verdict, out: &mut StepRecord) {
    match verdict {
        Verdict::Succeeded { promotion_pending } => {
            out.outcome = StepOutcome::Succeeded as i32;
            out.promotion_pending = *promotion_pending;
        }
        Verdict::Failed { phase, class } => {
            out.outcome = StepOutcome::Failed as i32;
            out.phase = failure_phase(*phase) as i32;
            out.class = failure_class(*class) as i32;
        }
        Verdict::Deferred { retry_after_secs } => {
            out.outcome = StepOutcome::Deferred as i32;
            out.retry_after_secs = retry_after_secs.unwrap_or(0);
        }
        Verdict::Halt => {
            out.outcome = StepOutcome::Failed as i32;
            out.phase = FailurePhase::Halt as i32;
        }
    }
}

fn failure_phase(phase: Phase) -> FailurePhase {
    match phase {
        Phase::BeforeCommit => FailurePhase::BeforeCommit,
        Phase::GateRelease => FailurePhase::GateRelease,
        Phase::GateEnvironment => FailurePhase::GateEnvironment,
        Phase::AfterCommit => FailurePhase::AfterCommit,
        Phase::Halt => FailurePhase::Halt,
    }
}

fn failure_class(class: Class) -> FailureClass {
    match class {
        Class::Release => FailureClass::Release,
        Class::Environment => FailureClass::Environment,
    }
}
