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
//! `Status` reports, besides the services, the host's enrollment and the heartbeat loop's link
//! as the loops last wrote them into the daemon's [`crate::node::NodeState`]; each is absent
//! until its loop wrote it.
//!
//! The service has exactly the three calls of `hostkeeper.local.v1.Admin`: `Status`, `Abort`
//! ([`Daemon::abort`]) and `ClearRecord` ([`Daemon::clear_record`]). Nothing on the socket
//! reaches `clear_halt` or a verb that installs, updates or switches a version. A refused call
//! answers a gRPC status whose message is one token ([`admin_status`]) and changes nothing.
//! The admin protobuf has no token for a service whose lock is held: that is answered
//! `FAILED_PRECONDITION` with the daemon's own token `busy`.

use std::io;
use std::sync::Arc;

use hostkeeper_enroll::Enrollment;
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_observe::{
    HealthView, Observation, ServiceState as Observed, UnhealthyReason, UnobservedReason,
    VersionView, beat_health,
};
use hostkeeper_proto::hostkeeper::agent::v1::{FailureClass, FailurePhase, Health};
use hostkeeper_proto::hostkeeper::local::v1::admin_server::{Admin, AdminServer};
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortRequest, AbortResponse, ClearRecordRequest, ClearRecordResponse,
    Enrollment as WireEnrollment, EnrollmentState, Lifecycle as Life, LinkHealth as WireLink,
    Liveness, RecordProblem, ServiceRecord, ServiceState, StatusRequest, StatusResponse,
    StepOutcome, StepRecord, Unhealthy,
};
use hostkeeper_steps::record::{Record, State};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_stream::{Stream, StreamExt};
use tonic::transport::server::Connected;
use tonic::transport::{Error, Server};
use tonic::{Request, Response, Status};

use crate::checks;
use crate::executor::{Daemon, LastUninstall, RecordState, Refused, ServiceStatus};
use crate::installed::Lifecycle;
use crate::node::LinkHealth;
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
    serve_until(daemon, incoming, std::future::pending()).await
}

/// [`serve`], until `stop` completes: then no connection is taken any more, the requests in
/// flight are answered, and it returns.
pub async fn serve_until<S, IO, F>(daemon: Arc<Daemon>, incoming: S, stop: F) -> Result<(), Error>
where
    S: Stream<Item = io::Result<(IO, Peer)>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    F: std::future::Future<Output = ()>,
{
    let admitted = incoming.filter_map(admit);
    Server::builder()
        .serve_with_incoming_shutdown(server(daemon), admitted, stop)
        .await
}

/// The `Admin` service over the daemon.
pub struct AdminService {
    daemon: Arc<Daemon>,
}

/// The `Admin` server over `daemon`: its three calls, and `UNIMPLEMENTED` for any other path.
/// [`serve_until`] puts it behind the peer check.
pub fn server(daemon: Arc<Daemon>) -> AdminServer<AdminService> {
    AdminServer::new(AdminService { daemon })
}

/// The `AdminRefusal` token of a request whose service name breaks its rule.
pub const INVALID_REQUEST: &str = "invalid_request";

/// The gRPC status of a refused admin call, with the code its `AdminRefusal` token names.
pub fn admin_status(refused: &Refused) -> Status {
    let token = refused.refusal.token();
    match refused.refusal {
        Refusal::InvalidOrder => Status::invalid_argument(INVALID_REQUEST),
        Refusal::NothingRunning | Refusal::RecordReads | Refusal::Busy => {
            Status::failed_precondition(token)
        }
        _ => Status::internal(Refusal::StoreFailed.token()),
    }
}

/// The service a request names, or `invalid_request`.
fn service(text: &str) -> Result<hostkeeper_config::names::ServiceName, Status> {
    checks::service_name(text).map_err(|refusal| admin_status(&Refused::new(refusal, text)))
}

/// Runs `call` on a thread meant for blocking: an abort waits for the order it stops to end. A
/// call whose thread failed is `store_failed`.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, Refused> + Send + 'static,
) -> Result<T, Status> {
    let failed = Err(Refused::new(
        Refusal::StoreFailed,
        "the call's thread failed",
    ));
    let done = tokio::task::spawn_blocking(call).await.unwrap_or(failed);
    done.map_err(|refused| admin_status(&refused))
}

#[tonic::async_trait]
impl Admin for AdminService {
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        // The status probes each service's health and asks the service manager, so it runs on a
        // thread meant for blocking; a status that cannot be had, or whose thread failed, is one
        // answer.
        let daemon = Arc::clone(&self.daemon);
        let status = tokio::task::spawn_blocking(move || daemon.status())
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| Status::internal(Refusal::StoreFailed.token()))?;
        let services = status.services.iter().map(service_record).collect();
        let node = &self.daemon.ctx().node;
        Ok(Response::new(StatusResponse {
            services,
            enrollment: node.enrollment().as_ref().map(enrollment),
            link: node.link().map(link_health),
        }))
    }

    async fn abort(
        &self,
        request: Request<AbortRequest>,
    ) -> Result<Response<AbortResponse>, Status> {
        let service = service(&request.get_ref().service)?;
        let daemon = Arc::clone(&self.daemon);
        let aborted = blocking(move || daemon.abort(&service)).await?;
        let record = Some(step_record(&aborted.record));
        Ok(Response::new(AbortResponse { record }))
    }

    async fn clear_record(
        &self,
        request: Request<ClearRecordRequest>,
    ) -> Result<Response<ClearRecordResponse>, Status> {
        let service = service(&request.get_ref().service)?;
        let daemon = Arc::clone(&self.daemon);
        blocking(move || daemon.clear_record(&service)).await?;
        let problem = RecordProblem::Unreadable as i32;
        Ok(Response::new(ClearRecordResponse { problem }))
    }
}

/// One service as `Status` reports it. How it runs (`state`) is its observation
/// ([`service_state`]), left out for a service that was not observed. A service whose step could
/// not be stopped is under way, with the reason `unstoppable_unit`. A service being removed says
/// so in its lifecycle, and its `last` is the verdict of its uninstall's last run when the daemon
/// has one.
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
    let last = match (status.lifecycle, &status.uninstall) {
        (Lifecycle::Removing, Some(uninstall)) => Some(uninstall_record(uninstall)),
        _ => last,
    };
    ServiceRecord {
        service: status.service.to_string(),
        current,
        last,
        state: status.observation.as_ref().map(service_state),
        record_problem: problem as i32,
        lifecycle: lifecycle(status.lifecycle) as i32,
    }
}

/// The host's enrollment in the fields of the wire's `Enrollment`: `key_sha256` only for a
/// pending host, `host_id` and `not_after_unix` only for an enrolled one. Why a pki directory
/// is unreadable is not on the wire; the enrollment loop logged it.
pub fn enrollment(enrollment: &Enrollment) -> WireEnrollment {
    let state = match enrollment {
        Enrollment::Unenrolled => EnrollmentState::Unenrolled,
        Enrollment::Pending { .. } => EnrollmentState::Pending,
        Enrollment::Enrolled { .. } => EnrollmentState::Enrolled,
        Enrollment::Unreadable(_) => EnrollmentState::Unreadable,
    };
    let mut out = WireEnrollment {
        state: state as i32,
        ..WireEnrollment::default()
    };
    match enrollment {
        Enrollment::Pending { key_sha256 } => out.key_sha256 = key_sha256.to_string(),
        Enrollment::Enrolled { host_id, not_after } => {
            out.host_id = host_id.as_str().to_owned();
            out.not_after_unix = Some(*not_after);
        }
        Enrollment::Unenrolled | Enrollment::Unreadable(_) => {}
    }
    out
}

/// The heartbeat loop's link in the fields of the wire's `LinkHealth`, one to one.
pub fn link_health(link: LinkHealth) -> WireLink {
    WireLink {
        last_success_unix: link.last_success_unix,
        consecutive_failures: link.consecutive_failures,
    }
}

/// An observation in the fields of the wire's `ServiceState`.
///
/// - `liveness` is the observed state, one to one.
/// - `version` is the recorded version, only when the running executable's digest confirmed it.
/// - `healthy` is a live service whose heartbeat health is healthy ([`beat_health`]) and that has
///   no `unhealthy` reason.
/// - `unhealthy` is `OBSERVING` for a round discarded because the units changed during it,
///   `VERSION_MISMATCH` for an executable that is not the sealed one, `HEALTH`,
///   `HEALTH_REDIRECT` or `HEALTH_UNMAPPED` for the health check's reason (`status_non_2xx` and
///   `unreachable`, `redirect`, `url_invalid`), and `NO_MAIN_PROCESS` for a live service whose
///   recorded version could not be told apart; else `UNSPECIFIED`.
///
/// The wire has no field for the observation's reason token or for the unit that decided it, so
/// `unit` is empty; the reason is in the library's [`Observation::reason_token`].
pub fn service_state(observation: &Observation) -> ServiceState {
    let liveness = match &observation.state {
        Observed::Absent => Liveness::Absent,
        Observed::Stopped => Liveness::Stopped,
        Observed::Transitioning => Liveness::Transitioning,
        Observed::Unobserved { .. } => Liveness::Unobserved,
        Observed::Unmonitored => Liveness::Unmonitored,
        Observed::Live => Liveness::Live,
    };
    let version = match &observation.version {
        VersionView::Recorded {
            version,
            confirmed: true,
        } => Some(version.clone()),
        _ => None,
    };
    let live = observation.state == Observed::Live;
    let unhealthy = match (
        &observation.state,
        &observation.version,
        &observation.health,
    ) {
        (
            Observed::Unobserved {
                reason: UnobservedReason::ChangedDuringRound,
                ..
            },
            _,
            _,
        ) => Unhealthy::Observing,
        (_, VersionView::Mismatch { .. }, _) => Unhealthy::VersionMismatch,
        (
            _,
            _,
            HealthView::Unhealthy {
                reason: UnhealthyReason::StatusNon2xx | UnhealthyReason::Unreachable,
            },
        ) => Unhealthy::Health,
        (
            _,
            _,
            HealthView::Unhealthy {
                reason: UnhealthyReason::Redirect,
            },
        ) => Unhealthy::HealthRedirect,
        (
            _,
            _,
            HealthView::Unhealthy {
                reason: UnhealthyReason::UrlInvalid,
            },
        ) => Unhealthy::HealthUnmapped,
        (Observed::Live, VersionView::Unknown, _) if observation.recorded.is_some() => {
            Unhealthy::NoMainProcess
        }
        _ => Unhealthy::Unspecified,
    };
    ServiceState {
        liveness: liveness as i32,
        version,
        healthy: live
            & (beat_health(observation).0 == Health::Healthy)
            & (unhealthy == Unhealthy::Unspecified),
        unhealthy: unhealthy as i32,
        unit: String::new(),
    }
}

/// An uninstall's last run, in the fields of a step record: its try, the step `uninstall`, and
/// its verdict.
pub fn uninstall_record(uninstall: &LastUninstall) -> StepRecord {
    let mut out = StepRecord {
        order_try: uninstall.order_try,
        step: hostkeeper_hook::Step::Uninstall.name().to_owned(),
        reason: uninstall.finished.reason.clone().unwrap_or_default(),
        ..StepRecord::default()
    };
    verdict_into(&uninstall.finished.verdict, &mut out);
    out
}

fn lifecycle(lifecycle: Lifecycle) -> Life {
    match lifecycle {
        Lifecycle::Absent => Life::Absent,
        Lifecycle::Installing => Life::Installing,
        Lifecycle::Running => Life::Running,
        Lifecycle::Removing => Life::Removing,
    }
}

/// An order as its step record holds it. The record keeps no wall-clock times, and an order id
/// only for a force. A force, or a hooked update a force stopped, is `forced`; a force runs no
/// hook step, so its `step` is empty.
pub fn step_record(rec: &Record) -> StepRecord {
    let force = rec.force.as_ref();
    let mut out = StepRecord {
        order_id: force
            .and_then(|force| force.order_id.clone())
            .unwrap_or_default(),
        order_try: rec.order_try.unwrap_or(0),
        version: rec.target_version.clone(),
        package_digest: rec.tarball_digest.clone(),
        forced: rec.forced | force.is_some(),
        step: match force {
            Some(_) => String::new(),
            None => rec.step.name().to_owned(),
        },
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
