//! Service identity at the coordinator (`docs/design/service-identity.md`): the certificate of a
//! service a host runs, the deny list a relying party reads, and the operator's approval of a host
//! for identity. The coordinator-wide checks (is the service assigned to the host, does its entry
//! say `identity`) are [`crate::service_identity`]'s; this module is the part that needs the store,
//! the CA and the audit.
//!
//! - [`Enrollment::service_ask`] reads the first four checks of a `IssueServiceCertificate`, in the
//!   spec's order: the caller's client certificate is a current host certificate (`forbidden`),
//!   the host is not denied (`denied`), the service is a service name (`invalid_service`), and the
//!   CSR is a P-256 request signed by its own key (`bad_csr`, `key_not_p256`; its subject is not
//!   looked at). [`Enrollment::issue_service`] makes the last: the host's assurance allows it
//!   (`identity_not_approved`), then takes a serial of the counter the host certificates use,
//!   signs ([`hostkeeper_enrollment::ca::Ca::issue_service`]) and audits `service_identity_issued`
//!   about the host and the service, with the serial: never the CSR. A serial or an audit entry
//!   that cannot be written is `store_failed`, and the certificate is not answered. A refusal
//!   changes nothing.
//! - [`Enrollment::list_deny`] answers the hosts with a deny row that is retiring or retired, to a
//!   caller whose client certificate is a current host certificate or a service certificate of a
//!   host that is not denied, and whose time is now.
//! - [`Enrollment::identity_get`] and [`Enrollment::identity_approve`] are `HostIdentity.Get` and
//!   `HostIdentity.Approve`: the approval is audited `identity_approve` under the name of the
//!   operator's token, about the host, before it is written, so a call that cannot write its audit
//!   entry changes nothing; a key approved already, and a host an operator approved by hand,
//!   change nothing and audit nothing.

use hostkeeper_enrollment::ca::{P256Key, check_key_request};
use hostkeeper_enrollment::enroll::{
    Assurance, HostIdentityState, IdentityApproval, IdentityApproved, IdentityError,
};
use hostkeeper_enrollment::names::{HostId, KeyHash, ServiceName};
use hostkeeper_proto::hostkeeper::agent::v1::{
    CoordinatorRefusal, IssueServiceCertificateRequest, IssueServiceCertificateResponse,
    ListDenyResponse,
};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::HostIdentityRefusal;

use super::Enrollment;
use super::refusal::{RpcError, rpc_error};
use super::rpc::audit_entry;
use crate::identity::Identity;
use crate::operator::optional_reason;
use crate::ops::{AuditEntry, Write};

/// The audit action of a service certificate issued.
pub const ACTION_SERVICE_IDENTITY_ISSUED: &str = "service_identity_issued";

/// The audit action of an operator's approval of a host's key for service identity.
pub const ACTION_IDENTITY_APPROVE: &str = "identity_approve";

/// A call of `IssueServiceCertificate` that passed the checks of the caller and the request.
#[derive(Debug)]
pub(crate) struct ServiceAsk {
    /// The host the caller's certificate names, as the store spells it.
    pub(crate) host: HostId,
    /// The host and its role, as the heartbeat path reads a certificate.
    pub(crate) identity: Identity,
    /// The service the request names.
    pub(crate) service: ServiceName,
    /// The key of the CSR: the only thing the certificate takes from it.
    key: P256Key,
}

/// The refusal of a call that could not read or write what it records: logged, and told as
/// `store_failed` (the coordinator issued nothing, and asking again is safe).
fn store_failed(error: impl std::fmt::Display) -> RpcError {
    tracing::error!(%error, "a service certificate could not be recorded: nothing was issued");
    RpcError::Refused(CoordinatorRefusal::StoreFailed)
}

/// The refusal `refusal`, as an error.
fn refused(refusal: CoordinatorRefusal) -> RpcError {
    RpcError::Refused(refusal)
}

/// How a `HostIdentity` call that read or wrote the store and failed is told: logged, and
/// `store_failed`.
fn host_store_failed(error: impl std::fmt::Display) -> HostIdentityRefusal {
    tracing::error!(%error, "a host's identity could not be read or written: nothing changed");
    HostIdentityRefusal::StoreFailed
}

/// The refusal an error of the store's approval for identity is.
pub(super) fn identity_refusal(error: IdentityError) -> HostIdentityRefusal {
    match error {
        IdentityError::UnknownHost(_) => HostIdentityRefusal::UnknownHost,
        IdentityError::KeyMismatch => HostIdentityRefusal::KeyMismatch,
        IdentityError::Denied => HostIdentityRefusal::Denied,
        IdentityError::Store(why) => host_store_failed(why),
    }
}

/// The approval in force that an approval the store made answers, none for a host that needs none.
pub(super) fn approval_of(approved: IdentityApproved) -> Option<IdentityApproval> {
    match approved {
        IdentityApproved::Approved(approval) | IdentityApproved::AlreadyApproved(approval) => {
            Some(approval)
        }
        IdentityApproved::OperatorAssured => None,
    }
}

/// `approval` as the operator API shows it.
fn approval_info(approval: &IdentityApproval) -> proto::IdentityApprovalInfo {
    proto::IdentityApprovalInfo {
        approver: approval.approver.clone(),
        approved_unix: approval.approved_unix,
    }
}

impl Enrollment {
    /// The first four checks of `IssueServiceCertificate` by the holder of the client certificate
    /// `peer`, in order.
    ///
    /// # Errors
    /// `forbidden` (no certificate, or one that is no current certificate of an enrolled host),
    /// `denied`, `invalid_service`, `bad_csr`, `key_not_p256`, or `store_failed` (the deny row
    /// cannot be read).
    pub(crate) fn service_ask(
        &self,
        peer: Option<&[u8]>,
        request: &IssueServiceCertificateRequest,
    ) -> Result<ServiceAsk, RpcError> {
        let (host, identity) = self.current_host(peer)?;
        if self.store.is_denied(&host).map_err(store_failed)? {
            return Err(refused(CoordinatorRefusal::Denied));
        }
        let service = ServiceName::parse(&request.service)
            .map_err(|_| refused(CoordinatorRefusal::InvalidService))?;
        let key = check_key_request(&request.csr_der).map_err(rpc_error)?;
        Ok(ServiceAsk {
            host,
            identity,
            service,
            key,
        })
    }

    /// The host `peer`'s certificate names, when it is a current host certificate of an enrolled
    /// host ([`Enrollment::identity_of`]), as the store spells it and as the heartbeat path does.
    fn current_host(&self, peer: Option<&[u8]>) -> Result<(HostId, Identity), RpcError> {
        let der = peer.ok_or(refused(CoordinatorRefusal::Forbidden))?;
        let certificate = self
            .ca_bundle()
            .host_certificate(der)
            .ok_or(refused(CoordinatorRefusal::Forbidden))?;
        let identity = self
            .identity_of(der)
            .ok_or(refused(CoordinatorRefusal::Forbidden))?;
        Ok((certificate.host_id, identity))
    }

    /// The last check of `IssueServiceCertificate`, then the certificate: the host's assurance
    /// allows service identity, a serial is taken, the CA signs, and the issue is audited.
    ///
    /// # Errors
    /// `identity_not_approved`, `store_failed` (the host's record, the serial or the audit entry
    /// cannot be read or written: nothing is answered), or [`RpcError::Internal`] (signing).
    pub(crate) fn issue_service(
        &self,
        ask: &ServiceAsk,
    ) -> Result<IssueServiceCertificateResponse, RpcError> {
        if !self
            .store
            .identity_allowed(&ask.host)
            .map_err(store_failed)?
        {
            return Err(refused(CoordinatorRefusal::IdentityNotApproved));
        }
        let serial = self.store.allocate_serial().map_err(store_failed)?;
        let certificate = self
            .ca
            .issue_service(&ask.service, &ask.host, &ask.key, serial)
            .map_err(rpc_error)?;
        let mut entry = audit_entry(
            ACTION_SERVICE_IDENTITY_ISSUED,
            Some(&ask.host),
            format!("serial {serial}"),
        );
        entry.service = Some(ask.service.as_str().to_owned());
        self.audit(entry)
            .ok_or_else(|| store_failed("the audit entry was not written"))?;
        tracing::info!(
            host_id = %ask.host,
            service = ask.service.as_str(),
            serial,
            "issued a service certificate"
        );
        Ok(IssueServiceCertificateResponse {
            certificate_der: certificate.der,
            ca_bundle_pem: self.ca_pem(),
        })
    }

    /// `ListDeny` by the holder of the client certificate `peer`: the hosts with a deny row that is
    /// retiring or retired, sorted.
    ///
    /// # Errors
    /// `forbidden` (no certificate, or one that is neither a current host certificate nor a service
    /// certificate of this CA that is valid now), `denied` (the caller's host is denied), or
    /// [`RpcError::Internal`] (a deny row cannot be read).
    pub fn list_deny(&self, peer: Option<&[u8]>) -> Result<ListDenyResponse, RpcError> {
        let der = peer.ok_or(refused(CoordinatorRefusal::Forbidden))?;
        let caller = match self.ca_bundle().service_certificate(der) {
            Ok(certificate) => {
                let now = self.clock.now_unix();
                if !(certificate.not_before_unix..certificate.not_after_unix).contains(&now) {
                    return Err(refused(CoordinatorRefusal::Forbidden));
                }
                certificate.host_id
            }
            Err(_) => self.current_host(peer)?.0,
        };
        if self.store.is_denied(&caller).map_err(rpc_error)? {
            return Err(refused(CoordinatorRefusal::Denied));
        }
        let host_ids = self
            .store
            .denied_hosts()
            .map_err(rpc_error)?
            .iter()
            .map(HostId::to_string)
            .collect();
        Ok(ListDenyResponse { host_ids })
    }

    /// `HostIdentity.Get`: the assurance of the host, its current key, and whether the
    /// coordinator signs service certificates for it.
    ///
    /// # Errors
    /// `invalid_request` (the node id), `unknown_host`, `store_failed`.
    pub fn identity_get(
        &self,
        request: &proto::GetHostIdentityRequest,
    ) -> Result<proto::GetHostIdentityResponse, HostIdentityRefusal> {
        let host =
            HostId::parse(&request.node_id).map_err(|_| HostIdentityRefusal::InvalidRequest)?;
        let state = self.held_identity(&host)?;
        Ok(proto::GetHostIdentityResponse {
            node_id: host.to_string(),
            assurance: assurance_of(&state) as i32,
            key_sha256: state.key_hash.to_hex(),
            allowed: !state.denied
                && (state.assurance == Assurance::Operator || state.approval.is_some()),
            approval: state.approval.as_ref().map(approval_info),
            denied: state.denied,
        })
    }

    /// `HostIdentity.Approve` by `by` (the name of the operator's token, already checked): the
    /// host's current key is approved for service identity, audited `identity_approve` before it is
    /// written. The answer is the approval in force; none for a host an operator approved by hand,
    /// which needs none, and then nothing is written or audited, as for a key approved already.
    ///
    /// # Errors
    /// `invalid_request` (the node id, the key hash or the reason), `unknown_host`,
    /// `key_mismatch`, `denied`, `store_failed`, `audit_unavailable`.
    pub fn identity_approve(
        &self,
        by: &str,
        request: &proto::ApproveHostIdentityRequest,
    ) -> Result<proto::ApproveHostIdentityResponse, HostIdentityRefusal> {
        let host =
            HostId::parse(&request.node_id).map_err(|_| HostIdentityRefusal::InvalidRequest)?;
        let key =
            KeyHash::from_hex(&request.key_sha256).ok_or(HostIdentityRefusal::InvalidRequest)?;
        let why = optional_reason(&request.reason, HostIdentityRefusal::InvalidRequest)?;
        let state = self.held_identity(&host)?;
        if !key.equals_in_constant_time(&state.key_hash) {
            return Err(HostIdentityRefusal::KeyMismatch);
        }
        if state.denied {
            return Err(HostIdentityRefusal::Denied);
        }
        let held = if state.assurance == Assurance::Operator {
            None
        } else if state.approval.is_some() {
            state.approval
        } else {
            let mut entry = AuditEntry::new(by, ACTION_IDENTITY_APPROVE);
            entry.node_id = Some(host.to_string());
            entry.reason = Some(match why {
                Some(why) => format!("key {}; {why}", key.to_hex()),
                None => format!("key {}", key.to_hex()),
            });
            self.write_identity_entry(entry)?;
            let approved = self
                .store
                .approve_identity(&host, &key, by, self.clock.now_unix())
                .map_err(identity_refusal)?;
            approval_of(approved)
        };
        Ok(proto::ApproveHostIdentityResponse {
            node_id: host.to_string(),
            approval: held.as_ref().map(approval_info),
        })
    }

    /// What the store holds of the identity of `host`.
    fn held_identity(&self, host: &HostId) -> Result<HostIdentityState, HostIdentityRefusal> {
        self.store
            .identity_state(host)
            .map_err(host_store_failed)?
            .ok_or(HostIdentityRefusal::UnknownHost)
    }

    /// Writes `entry` to the ops store enrollment audits into, before the change it records.
    fn write_identity_entry(&self, entry: AuditEntry) -> Result<(), HostIdentityRefusal> {
        let ops = self.ops().ok_or_else(|| {
            tracing::error!("no ops store to audit an identity approval in: nothing changed");
            HostIdentityRefusal::AuditUnavailable
        })?;
        ops.change(Write::BeforeMemory, entry, |_| {})
            .map(|_| ())
            .map_err(|why| {
                tracing::error!(%why, "an identity approval's audit entry could not be written: nothing changed");
                HostIdentityRefusal::AuditUnavailable
            })
    }
}

/// The assurance of `state` as the operator API names it.
fn assurance_of(state: &HostIdentityState) -> proto::HostAssurance {
    match state.assurance {
        Assurance::Operator => proto::HostAssurance::Operator,
        Assurance::Auto => proto::HostAssurance::Auto,
    }
}
