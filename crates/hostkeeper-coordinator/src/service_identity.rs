//! Service identity at the coordinator (`docs/design/service-identity.md`): which services of a
//! host get a certificate, and the daemon's report of what it holds.
//!
//! [`Coordinator::issue_service_certificate`] is `Coordinator.IssueServiceCertificate`: the caller
//! and the request are checked first (a current host certificate, not denied, a service name, a
//! P-256 CSR: `crate::enrollment::identity`), then
//! [`Coordinator::identity_assigned`] says whether the service is the host's and its entry says
//! `identity`, then the enrollment checks the host's assurance and signs. The checks are made in
//! that order, so the refusal a caller is told is the first that fails.
//!
//! A service is **assigned** to a host when the host's [`crate::group_scope::Basis`] lists it for
//! the host and no uninstall of it is ordered on the host: in group mode the host's group lists it;
//! in role mode (a coordinator that holds no host group) the rollout file, and the operator's
//! assignments over it, assign it to the host's role. A host in no group is assigned nothing, as in
//! the decision. The entry is the one [`crate::picks::pick`] would order the host for the service's
//! desired version (the release the coordinator keeps for it): its manifest says `identity`. A host
//! whose daemon has not been heard since the coordinator started is picked nothing and is refused
//! `service_not_assigned` until its next heartbeat, which the daemon's retry waits for.
//!
//! The daemon announces no feature (the list of features is full) and reports in every heartbeat
//! over gRPC the certificate it holds of each service with an identity ([`crate::service_identity_report`]).

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1::{
    CoordinatorRefusal, IssueServiceCertificateRequest, IssueServiceCertificateResponse,
};

use crate::assignment::{Placement, assigned};
use crate::enrollment::{Enrollment, RpcError};
use crate::identity::Identity;
use crate::picks::{Pick, pick};
use crate::service::Coordinator;

impl Coordinator {
    /// `Coordinator.IssueServiceCertificate` for the holder of the client certificate `peer`.
    ///
    /// # Errors
    /// `forbidden`, `denied`, `invalid_service`, `bad_csr`, `key_not_p256`, `service_not_assigned`,
    /// `identity_not_approved`, `store_failed` (nothing was issued), or [`RpcError::Internal`].
    pub(crate) fn issue_service_certificate(
        &self,
        enrollment: &Arc<Enrollment>,
        peer: Option<&[u8]>,
        request: &IssueServiceCertificateRequest,
    ) -> Result<IssueServiceCertificateResponse, RpcError> {
        let ask = enrollment.service_ask(peer, request)?;
        if !self.identity_assigned(&ask.identity, ask.service.as_str()) {
            tracing::info!(
                host_id = %ask.host,
                service = ask.service.as_str(),
                "refused a service certificate: the service is not assigned with an identity"
            );
            return Err(RpcError::Refused(CoordinatorRefusal::ServiceNotAssigned));
        }
        enrollment.issue_service(&ask)
    }

    /// Whether `service` is assigned to the host `who` and the entry the coordinator would pick for
    /// the host at the service's desired version says `identity` (the module says how each is
    /// decided).
    pub(crate) fn identity_assigned(&self, who: &Identity, service: &str) -> bool {
        let basis = self.basis_of(&who.host_id);
        let Some(def) = basis.services.get(service) else {
            return false;
        };
        let role = basis.role_of(who);
        let host = who.host_id.as_str();
        let placed = self.ops.read(|state| {
            let entries = state.assignments.get(service);
            let placement = Placement {
                node_id: host,
                role: &role,
                labels: state.hosts.get(host).map(|held| &held.labels),
            };
            assigned(def, entries, &placement)
                && !entries.is_some_and(|entries| entries.uninstall.contains_key(host))
        });
        if !placed {
            return false;
        }
        let Some(desired) = self.desired_in(&basis, service) else {
            return false;
        };
        let fleet = self.lock_fleet();
        matches!(
            pick(&fleet, service, &desired.version, &who.host_id),
            Pick::Entry(entry) if entry.manifest.as_ref().and_then(|m| m.identity) == Some(true)
        )
    }
}
