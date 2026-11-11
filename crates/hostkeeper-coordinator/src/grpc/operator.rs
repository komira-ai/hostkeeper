//! The eight services of `hostkeeper.operator.v1` over [`Coordinator`]: `Hosts`, `Releases`,
//! `Services`, `Enrollment`, `Audit`, `Alerts`, `Rollouts` and `Mdm`.
//!
//! An adapter has no rule of its own. A method that has a handler in the coordinator
//! ([`crate::operator`], [`crate::hosts`], [`crate::force`], [`crate::assign`],
//! [`crate::alerts`], [`crate::rollouts`], [`crate::operator_mdm`], [`crate::wait`]) calls it with
//! the caller the router admitted. A method the frozen JSON routes have (`Hosts.Include`,
//! `Hosts.SetLabels`, `Releases.ClearHalt`, `Releases.SetDesired`, `Releases.Converge`,
//! `Releases.Status`) builds the request that route reads and calls what that route calls. Every
//! method has a handler. The caller was settled before the request was read ([`super::admit`])
//! and arrives in the request's extensions; a request without one is an anonymous caller, and is
//! refused as such. `HostGroups` is in [`super::operator_host_groups`].
//!
//! Every call that takes a `host_group` reads it in its handler: with no host group held a
//! request that names one is refused (`not_in_host_group` of `Releases` and `Rollouts`,
//! `unknown_host_group` of `Enrollment.Approve` and `Enrollment.AddRecord`), never read as naming
//! none, and `Audit.List` with a `host_group` filter lists no entry ([`crate::operator`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::alerts_server::Alerts as AlertsApi;
use hostkeeper_proto::hostkeeper::operator::v1::audit_server::Audit as AuditApi;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_server::Enrollment as EnrollmentApi;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::Hosts as HostsApi;
use hostkeeper_proto::hostkeeper::operator::v1::mdm_server::Mdm as MdmApi;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as ReleasesApi;
use hostkeeper_proto::hostkeeper::operator::v1::rollouts_server::Rollouts as RolloutsApi;
use hostkeeper_proto::hostkeeper::operator::v1::services_server::Services as ServicesApi;
use serde_json::{Value, json};
use tonic::{Request, Response, Status};

use super::{Admitted, refusal, report};
use crate::desired::{Desired, DesiredError};
use crate::operator::{Caller, entry_proto};
use crate::ops::OperateError;
use crate::service::ci_calls::{ReleaseError, Setter};
use crate::service::{CiRefused, ConvergenceError, Coordinator, SetError, Unauthorized};
use crate::wait::{CallLimit, ConnectionWaits, MAX_WAIT_SECS, WaitRefused};
use crate::write_record::{WriteAsk, WriteId, WriteRecord};

/// What `operate` says of a service the coordinator does not manage.
const UNKNOWN_SERVICE: &str = "service is not one this coordinator manages";

/// What `operate` says of a clear of a service that is not halted.
const NOT_HALTED: &str = "the service is not halted";

/// The caller the router admitted for `request`; anonymous when it admitted none.
pub(crate) fn caller_of<T>(request: &Request<T>) -> Caller {
    match request.extensions().get::<Admitted>() {
        Some(Admitted::Operator { caller, .. }) => caller.clone(),
        _ => Caller::Anonymous,
    }
}

/// Does the JSON operator action `action` as `caller`, as `POST /v1/operator` does, and answers
/// the audit entry it made.
fn operate(
    coordinator: &Coordinator,
    caller: &Caller,
    action: &Value,
) -> Result<proto::AuditEntry, OperateError> {
    let by = caller.operator().map_err(|_| Unauthorized)?;
    let body = serde_json::to_vec(action).unwrap_or_default();
    coordinator
        .operate_as(by, &body)
        .map(|entry| entry_proto(&entry))
}

/// The refusal of `Hosts` for a failed operator action.
pub(crate) fn hosts_refusal(why: OperateError) -> proto::HostsRefusal {
    match why {
        OperateError::Unauthorized(_) => proto::HostsRefusal::Unauthorized,
        OperateError::Invalid(UNKNOWN_SERVICE) => proto::HostsRefusal::UnknownService,
        OperateError::Invalid(_) => proto::HostsRefusal::InvalidRequest,
        OperateError::Capped { .. } => proto::HostsRefusal::Capacity,
        OperateError::Store(why) => why.into(),
    }
}

/// The refusal of `Releases` for a failed operator action. `Releases` has no capacity refusal:
/// a full ops file is an invalid request there.
pub(crate) fn releases_action_refusal(why: OperateError) -> proto::ReleasesRefusal {
    match why {
        OperateError::Unauthorized(_) => proto::ReleasesRefusal::Unauthorized,
        OperateError::Invalid(UNKNOWN_SERVICE) => proto::ReleasesRefusal::UnknownService,
        OperateError::Invalid(NOT_HALTED) => proto::ReleasesRefusal::NotHalted,
        OperateError::Invalid(_) | OperateError::Capped { .. } => {
            proto::ReleasesRefusal::InvalidRequest
        }
        OperateError::Store(why) => why.into(),
    }
}

/// The refusal of `Releases` for a desired state CI's set refused.
pub(crate) fn set_refusal(why: SetError) -> proto::ReleasesRefusal {
    match why {
        SetError::Unauthorized(_) => proto::ReleasesRefusal::Unauthorized,
        SetError::Unavailable => proto::ReleasesRefusal::KeysUnavailable,
        SetError::Forbidden => proto::ReleasesRefusal::Forbidden,
        SetError::Invalid(DesiredError::UnknownService) => proto::ReleasesRefusal::UnknownService,
        SetError::Invalid(DesiredError::IndexInvalid(_)) => proto::ReleasesRefusal::IndexInvalid,
        SetError::Invalid(DesiredError::IndexDigestMismatch) => {
            proto::ReleasesRefusal::IndexDigestMismatch
        }
        SetError::Invalid(DesiredError::GenerationConflict { .. }) => {
            proto::ReleasesRefusal::GenerationConflict
        }
        SetError::Invalid(_) | SetError::UnparseableVersion | SetError::DigestConflict => {
            proto::ReleasesRefusal::InvalidRequest
        }
        SetError::TargetInvalid(_) => proto::ReleasesRefusal::TargetInvalid,
        SetError::ReleaseExists => proto::ReleasesRefusal::ReleaseExists,
        SetError::RepositoryUnknown => proto::ReleasesRefusal::RepositoryUnknown,
        SetError::BelowMinVersion => proto::ReleasesRefusal::BelowMinVersion,
        SetError::BadVersion => proto::ReleasesRefusal::HeldBad,
        SetError::NotInHostGroup => proto::ReleasesRefusal::NotInHostGroup,
        SetError::AmbiguousHostGroup => proto::ReleasesRefusal::AmbiguousHostGroup,
        SetError::LimitsInHostGroup => proto::ReleasesRefusal::LimitsInHostGroup,
        SetError::Ops(why) => why.into(),
        SetError::Store(_) => {
            tracing::error!(%why, "the desired state could not be stored");
            proto::ReleasesRefusal::StoreFailed
        }
    }
}

/// The refusal of `Releases` for a wait that was not held.
pub(crate) fn wait_refusal(why: WaitRefused) -> proto::ReleasesRefusal {
    match why {
        WaitRefused::UnknownService => proto::ReleasesRefusal::UnknownService,
        WaitRefused::TooMany => proto::ReleasesRefusal::TooManyWaits,
        WaitRefused::NotInHostGroup => proto::ReleasesRefusal::NotInHostGroup,
    }
}

/// The refusal of `Releases` for a convergence that was not read.
pub(crate) fn converge_refusal(why: ConvergenceError) -> proto::ReleasesRefusal {
    match why {
        ConvergenceError::Refused(CiRefused::Unauthorized(_)) => {
            proto::ReleasesRefusal::Unauthorized
        }
        ConvergenceError::Refused(CiRefused::Unavailable) => {
            proto::ReleasesRefusal::KeysUnavailable
        }
        ConvergenceError::UnknownService => proto::ReleasesRefusal::UnknownService,
        ConvergenceError::NotInHostGroup => proto::ReleasesRefusal::NotInHostGroup,
    }
}

/// The JSON body `POST /v1/desired` reads for `request`, and what the request asks of the store
/// beside it: its `if_generation` and its write id, which the JSON body does not carry
/// ([`WriteAsk`]; a store that keeps no write records refuses a request that names either).
///
/// # Errors
/// `invalid_request`: no desired state; a write id that is not 1 to 128 visible ASCII
/// characters; or two package digests that differ. An empty `service` names none, which the JSON
/// route reads as the legacy service. A release index and its digest are passed as the keys
/// `release_index` and `release_index_digest`, which the route checks (an index beside a package
/// digest, or one of the pair alone, is its `index_invalid`).
pub(crate) fn desired_body(
    request: proto::SetDesiredRequest,
) -> Result<(Vec<u8>, WriteAsk), proto::ReleasesRefusal> {
    let invalid = proto::ReleasesRefusal::InvalidRequest;
    let ask = WriteAsk {
        if_generation: request.if_generation,
        write_id: request
            .write_id
            .as_deref()
            .map(WriteId::parse)
            .transpose()
            .map_err(|_| invalid)?,
    };
    let desired = request.desired.ok_or(invalid)?;
    let digest = match (desired.package_digest, request.package_digest) {
        (Some(inside), Some(beside)) if inside != beside => return Err(invalid),
        (inside, beside) => inside.or(beside),
    };
    let mut body = json!({
        "version": desired.version,
        "max_updating": desired.max_updating,
        "min_up": desired.min_up,
    });
    if !request.service.is_empty() {
        body["service"] = request.service.into();
    }
    if let Some(digest) = digest {
        body["release_digest"] = digest.into();
    }
    if let Some(index) = request.release_index {
        body["release_index"] = index.into();
    }
    if let Some(digest) = request.release_index_digest {
        body["release_index_digest"] = digest.into();
    }
    Ok((serde_json::to_vec(&body).unwrap_or_default(), ask))
}

/// `record` as `hostkeeper.operator.v1` says it, the replaced minimums as the desired state's
/// are written (the format's `storage` and `frontend` named).
pub(crate) fn record_proto(record: &WriteRecord) -> proto::WriteRecord {
    proto::WriteRecord {
        generation: record.generation,
        replaced: record.replaced.as_ref().map(|replaced| proto::Replaced {
            version: replaced.version.as_str().to_owned(),
            min_up: replaced
                .min_up
                .min_up_written()
                .iter()
                .map(|(role, n)| (role.to_owned(), n))
                .collect(),
        }),
        write_id: record.write_id.as_ref().map(|id| id.as_str().to_owned()),
    }
}

/// `desired` as `hostkeeper.operator.v1` says it, with the limits as the JSON route writes them.
pub(crate) fn desired_proto(desired: &Desired) -> proto::DesiredState {
    let written = serde_json::to_value(desired).unwrap_or_default();
    let limits = |key: &str| -> HashMap<String, u32> {
        written
            .get(key)
            .cloned()
            .and_then(|limits| serde_json::from_value(limits).ok())
            .unwrap_or_default()
    };
    proto::DesiredState {
        version: desired.version.to_string(),
        max_updating: limits("max_updating"),
        min_up: limits("min_up"),
        package_digest: desired.release_digest.as_ref().map(ToString::to_string),
        release_index_digest: desired
            .release_index
            .as_ref()
            .map(|record| record.digest.to_string()),
    }
}

/// The service of `Hosts`.
pub(crate) struct HostsService {
    coordinator: Arc<Coordinator>,
}

impl HostsService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl HostsApi for HostsService {
    async fn list(
        &self,
        request: Request<proto::ListHostsRequest>,
    ) -> Result<Response<proto::ListHostsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .list_hosts(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::hosts)
    }

    async fn get(
        &self,
        request: Request<proto::GetHostRequest>,
    ) -> Result<Response<proto::GetHostResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .get_host(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::hosts)
    }

    async fn exclude(
        &self,
        request: Request<proto::ExcludeRequest>,
    ) -> Result<Response<proto::ExcludeResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .exclude(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::hosts)
    }

    async fn include(
        &self,
        request: Request<proto::IncludeRequest>,
    ) -> Result<Response<proto::IncludeResponse>, Status> {
        let caller = caller_of(&request);
        let message = request.into_inner();
        let action = json!({
            "action": "include",
            "service": message.service,
            "node_id": message.node_id,
        });
        operate(&self.coordinator, &caller, &action)
            .map(|entry| Response::new(proto::IncludeResponse { entry: Some(entry) }))
            .map_err(|why| refusal::hosts(hosts_refusal(why)))
    }

    async fn set_labels(
        &self,
        request: Request<proto::SetLabelsRequest>,
    ) -> Result<Response<proto::SetLabelsResponse>, Status> {
        let caller = caller_of(&request);
        let message = request.into_inner();
        let action = json!({
            "action": "set_labels",
            "node_id": message.node_id,
            "labels": message.labels,
        });
        operate(&self.coordinator, &caller, &action)
            .map(|entry| Response::new(proto::SetLabelsResponse { entry: Some(entry) }))
            .map_err(|why| refusal::hosts(hosts_refusal(why)))
    }

    async fn abort(
        &self,
        request: Request<proto::AbortRequest>,
    ) -> Result<Response<proto::AbortResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .abort(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::hosts)
    }

    async fn retire(
        &self,
        request: Request<proto::RetireRequest>,
    ) -> Result<Response<proto::RetireResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .retire_host(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::hosts)
    }

    async fn clear_record(
        &self,
        request: Request<proto::ClearRecordRequest>,
    ) -> Result<Response<proto::ClearRecordResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .clear_record(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::hosts)
    }
}

/// The service of `Releases`.
pub(crate) struct ReleasesService {
    coordinator: Arc<Coordinator>,
}

impl ReleasesService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }

    /// `not_in_host_group` for a request that names a host group while the coordinator holds none.
    fn no_group_held(&self, named: &str) -> Result<(), Status> {
        if named.is_empty() || self.coordinator.desired.in_group_mode() {
            return Ok(());
        }
        Err(refusal::releases(proto::ReleasesRefusal::NotInHostGroup))
    }
}

#[tonic::async_trait]
impl ReleasesApi for ReleasesService {
    async fn set_desired(
        &self,
        request: Request<proto::SetDesiredRequest>,
    ) -> Result<Response<proto::SetDesiredResponse>, Status> {
        let ci = match request.extensions().get::<Admitted>() {
            Some(Admitted::Ci(verified)) => Some(verified.clone()),
            _ => None,
        };
        let (ci, operator) = match (ci, caller_of(&request).operator()) {
            (Some(verified), _) => (Some(verified), String::new()),
            (None, Ok(name)) => (None, name.to_owned()),
            (None, Err(_)) => {
                return Err(refusal::releases(proto::ReleasesRefusal::Unauthorized));
            }
        };
        let mut message = request.into_inner();
        self.no_group_held(&message.host_group)?;
        // A map left empty is a set without targets: proto3 sends no empty map.
        let targets = std::mem::take(&mut message.targets);
        let targets = (!targets.is_empty()).then_some(targets);
        let host_group = std::mem::take(&mut message.host_group);
        let (body, ask) = desired_body(message).map_err(refusal::releases)?;
        let setter = match &ci {
            Some(verified) => Setter::Ci(verified),
            None => Setter::Operator(&operator),
        };
        let (set, record) = self
            .coordinator
            .set_desired_in(&setter, &body, &ask, targets, &host_group)
            .await
            .map_err(|why| refusal::releases(set_refusal(why)))?;
        Ok(Response::new(proto::SetDesiredResponse {
            desired: Some(desired_proto(&set.desired)),
            record: record.as_ref().map(record_proto),
            dropped_roles: set.dropped_roles,
        }))
    }

    async fn get_release(
        &self,
        request: Request<proto::GetReleaseRequest>,
    ) -> Result<Response<proto::GetReleaseResponse>, Status> {
        // CI's token or the operator's, as for `Converge`: a read, which changes nothing.
        let ci = matches!(
            request.extensions().get::<Admitted>(),
            Some(Admitted::Ci(_))
        );
        if !ci && caller_of(&request).operator().is_err() {
            return Err(refusal::releases(proto::ReleasesRefusal::Unauthorized));
        }
        let message = request.into_inner();
        self.no_group_held(&message.host_group)?;
        let service = match message.service.as_str() {
            "" => self.coordinator.legacy_service(),
            name => Some(name),
        };
        let service =
            service.ok_or_else(|| refusal::releases(proto::ReleasesRefusal::InvalidRequest))?;
        let release = self
            .coordinator
            .release_verified_in(&message.host_group, service, &message.version)
            .map_err(|why| {
                refusal::releases(match why {
                    ReleaseError::UnknownService => proto::ReleasesRefusal::UnknownService,
                    ReleaseError::Unknown => proto::ReleasesRefusal::ReleaseUnknown,
                    ReleaseError::NotInHostGroup => proto::ReleasesRefusal::NotInHostGroup,
                })
            })?;
        Ok(Response::new(proto::GetReleaseResponse {
            targets: release.targets().clone(),
            release_index_digest: release.index_digest().map(ToString::to_string),
        }))
    }

    async fn converge(
        &self,
        request: Request<proto::ConvergeRequest>,
    ) -> Result<Response<proto::ConvergeResponse>, Status> {
        // CI's token or the operator's: a convergence is a read, and changes nothing.
        let ci = matches!(
            request.extensions().get::<Admitted>(),
            Some(Admitted::Ci(_))
        );
        if !ci && caller_of(&request).operator().is_err() {
            return Err(refusal::releases(proto::ReleasesRefusal::Unauthorized));
        }
        // No service names the legacy service, as the JSON route's query does.
        let message = request.into_inner();
        self.no_group_held(&message.host_group)?;
        let service = match message.service.as_str() {
            "" => self.coordinator.legacy_service(),
            name => Some(name),
        };
        let service =
            service.ok_or_else(|| refusal::releases(proto::ReleasesRefusal::InvalidRequest))?;
        self.coordinator
            .convergence_in(&message.host_group, service)
            .map(|convergence| {
                let mut convergence = report::convergence(&convergence);
                convergence.record = self
                    .coordinator
                    .write_record_in(&message.host_group, service)
                    .as_ref()
                    .map(record_proto);
                Response::new(proto::ConvergeResponse {
                    convergence: Some(convergence),
                })
            })
            .map_err(|why| refusal::releases(converge_refusal(why)))
    }

    async fn wait_converged(
        &self,
        request: Request<proto::WaitConvergedRequest>,
    ) -> Result<Response<proto::WaitConvergedResponse>, Status> {
        // CI's token or the operator's, as for `Converge`: checked once, when the call was
        // admitted, and never again while it is held.
        let ci = matches!(
            request.extensions().get::<Admitted>(),
            Some(Admitted::Ci(_))
        );
        if !ci && caller_of(&request).operator().is_err() {
            return Err(refusal::releases(proto::ReleasesRefusal::Unauthorized));
        }
        let limit = request.extensions().get::<CallLimit>().cloned();
        let connection = request.extensions().get::<ConnectionWaits>().cloned();
        let message = request.into_inner();
        self.no_group_held(&message.host_group)?;
        if message.wait_secs == 0 || message.wait_secs > MAX_WAIT_SECS {
            return Err(refusal::releases(proto::ReleasesRefusal::InvalidWait));
        }
        let service = match message.service.as_str() {
            "" => self.coordinator.legacy_service(),
            name => Some(name),
        };
        let service = service
            .filter(|_| !message.version.is_empty())
            .ok_or_else(|| refusal::releases(proto::ReleasesRefusal::InvalidRequest))?;
        let held = self
            .coordinator
            .hold_wait(service, &message.host_group)
            .map_err(|why| refusal::releases(wait_refusal(why)))?;
        let wait = Duration::from_secs(message.wait_secs.into());
        if let Some(limit) = limit {
            limit.hold_for(wait);
        }
        let _connection = connection.as_ref().map(ConnectionWaits::hold);
        let (outcome, convergence) = self
            .coordinator
            .wait_converged(&held, &message.version, wait)
            .await;
        let mut convergence = report::convergence(&convergence);
        convergence.record = self
            .coordinator
            .write_record_in(&message.host_group, service)
            .as_ref()
            .map(record_proto);
        Ok(Response::new(proto::WaitConvergedResponse {
            outcome: outcome.into(),
            convergence: Some(convergence),
        }))
    }

    async fn force(
        &self,
        request: Request<proto::ForceRequest>,
    ) -> Result<Response<proto::ForceResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .force(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::releases)
    }

    async fn unmark_bad(
        &self,
        request: Request<proto::UnmarkBadRequest>,
    ) -> Result<Response<proto::UnmarkBadResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .unmark_bad(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::releases)
    }

    async fn clear_halt(
        &self,
        request: Request<proto::ClearHaltRequest>,
    ) -> Result<Response<proto::ClearHaltResponse>, Status> {
        let caller = caller_of(&request);
        let message = request.into_inner();
        let action = json!({
            "action": "clear_halt",
            "service": message.service,
            "reason": message.reason,
        });
        operate(&self.coordinator, &caller, &action)
            .map(|entry| Response::new(proto::ClearHaltResponse { entry: Some(entry) }))
            .map_err(|why| refusal::releases(releases_action_refusal(why)))
    }

    async fn status(
        &self,
        request: Request<proto::StatusRequest>,
    ) -> Result<Response<proto::StatusResponse>, Status> {
        let caller = caller_of(&request);
        let only = request.into_inner().service;
        caller
            .operator()
            .map(|_| Response::new(report::status(&self.coordinator.status_now(), &only)))
            .map_err(|_| refusal::releases(proto::ReleasesRefusal::Unauthorized))
    }

    async fn mark_bad(
        &self,
        request: Request<proto::MarkBadRequest>,
    ) -> Result<Response<proto::MarkBadResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .mark_bad(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::releases)
    }

    async fn halt(
        &self,
        request: Request<proto::HaltRequest>,
    ) -> Result<Response<proto::HaltResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .halt(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::releases)
    }
}

/// The service of `Services`.
pub(crate) struct ServicesService {
    coordinator: Arc<Coordinator>,
}

impl ServicesService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl ServicesApi for ServicesService {
    async fn list(
        &self,
        request: Request<proto::ListServicesRequest>,
    ) -> Result<Response<proto::ListServicesResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .list_services(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn assign(
        &self,
        request: Request<proto::AssignRequest>,
    ) -> Result<Response<proto::AssignResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .assign(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn unassign(
        &self,
        request: Request<proto::UnassignRequest>,
    ) -> Result<Response<proto::UnassignResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .unassign(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn set_min_version(
        &self,
        request: Request<proto::SetMinVersionRequest>,
    ) -> Result<Response<proto::SetMinVersionResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .set_min_version(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn set_repository(
        &self,
        request: Request<proto::SetRepositoryRequest>,
    ) -> Result<Response<proto::SetRepositoryResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .set_repository(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn hold(
        &self,
        request: Request<proto::HoldRequest>,
    ) -> Result<Response<proto::HoldResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .hold(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }

    async fn unhold(
        &self,
        request: Request<proto::UnholdRequest>,
    ) -> Result<Response<proto::UnholdResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .unhold(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::services)
    }
}

/// The service of `Enrollment`.
pub(crate) struct EnrollmentService {
    coordinator: Arc<Coordinator>,
}

impl EnrollmentService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl EnrollmentApi for EnrollmentService {
    async fn list(
        &self,
        request: Request<proto::ListPendingRequest>,
    ) -> Result<Response<proto::ListPendingResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .enrollment_list(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn approve(
        &self,
        request: Request<proto::ApproveRequest>,
    ) -> Result<Response<proto::ApproveResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .enrollment_approve(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn reject(
        &self,
        request: Request<proto::RejectRequest>,
    ) -> Result<Response<proto::RejectResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .enrollment_reject(&caller, request.get_ref())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn add_record(
        &self,
        request: Request<proto::AddRecordRequest>,
    ) -> Result<Response<proto::AddRecordResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .add_record(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn list_records(
        &self,
        request: Request<proto::ListRecordsRequest>,
    ) -> Result<Response<proto::ListRecordsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .list_records(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn remove_record(
        &self,
        request: Request<proto::RemoveRecordRequest>,
    ) -> Result<Response<proto::RemoveRecordResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .remove_record(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }

    async fn open_record(
        &self,
        request: Request<proto::OpenRecordRequest>,
    ) -> Result<Response<proto::OpenRecordResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .open_record(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::enrollment)
    }
}

/// The service of `Audit`.
pub(crate) struct AuditService {
    coordinator: Arc<Coordinator>,
}

impl AuditService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl AuditApi for AuditService {
    async fn list(
        &self,
        request: Request<proto::ListAuditRequest>,
    ) -> Result<Response<proto::ListAuditResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .audit_list(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::audit)
    }
}

/// The service of `Alerts`.
pub(crate) struct AlertsService {
    coordinator: Arc<Coordinator>,
}

impl AlertsService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl AlertsApi for AlertsService {
    async fn list(
        &self,
        request: Request<proto::ListAlertsRequest>,
    ) -> Result<Response<proto::ListAlertsResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .list_alerts(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::alerts)
    }
}

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

/// `Rollouts` over the coordinator.
pub(crate) struct RolloutsService {
    coordinator: Arc<Coordinator>,
}

impl RolloutsService {
    /// The service over `coordinator`.
    pub(crate) fn new(coordinator: Arc<Coordinator>) -> Self {
        Self { coordinator }
    }
}

#[tonic::async_trait]
impl RolloutsApi for RolloutsService {
    async fn advance(
        &self,
        request: Request<proto::AdvanceRequest>,
    ) -> Result<Response<proto::AdvanceResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .rollout_advance(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::rollouts)
    }

    async fn pause(
        &self,
        request: Request<proto::PauseRequest>,
    ) -> Result<Response<proto::PauseResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .rollout_pause(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::rollouts)
    }

    async fn resume(
        &self,
        request: Request<proto::ResumeRequest>,
    ) -> Result<Response<proto::ResumeResponse>, Status> {
        let caller = caller_of(&request);
        self.coordinator
            .rollout_resume(&caller, request.into_inner())
            .map(Response::new)
            .map_err(refusal::rollouts)
    }
}
