//! `Enrollment.Reject` as a plain function over the store, and the deny row of
//! `Hosts.Retire` (design 6.1): the two ways an operator turns a host away.
//!
//! [`Enrollment::reject`] marks a pending request denied ([`EnrollmentStore::reject`]): it leaves
//! the queue, which makes room under the pending cap, and its key is refused from then on (its
//! `Poll` answers denied, and a repost of it changes nothing). The first rejection is audited
//! `enroll_reject` under the name of the operator's token, about the host a renewal renews;
//! a repeat is answered as rejected and audited nothing. As for an approval, the entry is written
//! right after the store's write, and one that cannot be written is logged.
//!
//! [`Enrollment::deny_host`] writes the deny row of an enrolled host ([`EnrollmentStore::deny`]),
//! which refuses its heartbeat and its `Renew` at once; the coordinator does the rest of a
//! retirement ([`crate::service::Coordinator::retire_host`]).

use hostkeeper_enrollment::enroll::{EnrollmentStore, RejectError, RequestView};
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{EnrollmentRefusal, HostsRefusal};

use super::Enrollment;
use super::approve::claims_summary;
use crate::operator::optional_reason;
use crate::ops::AuditEntry;

/// The audit action of a rejection.
pub const ACTION_REJECT: &str = "enroll_reject";

/// The audit entry of the rejection by `by` of `request`, for `why` if the operator gave one:
/// about the host a renewal renews, its reason the request id and the claims.
fn reject_entry(by: &str, request: &RequestView, why: Option<String>) -> AuditEntry {
    let mut entry = AuditEntry::new(by, ACTION_REJECT);
    entry.node_id = request.renewal_of.as_ref().map(HostId::to_string);
    let said = format!(
        "request {}; claims {}",
        request.request_id,
        claims_summary(&request.claims)
    );
    entry.reason = Some(match why {
        Some(why) => format!("{said}; {why}"),
        None => said,
    });
    entry
}

/// The refusal an error of the store's rejection is; a failure of the store is logged.
fn refusal_of(error: &RejectError) -> EnrollmentRefusal {
    match error {
        RejectError::UnknownRequest => EnrollmentRefusal::UnknownRequest,
        RejectError::NotPending(_) => EnrollmentRefusal::NotPending,
        RejectError::Store(_) => {
            tracing::error!(%error, "a rejection could not be written: nothing changed");
            EnrollmentRefusal::StoreFailed
        }
    }
}

impl Enrollment {
    /// `Enrollment.Reject` by `by` (the name of the operator's token, already checked): the
    /// pending request it names is rejected. Answers the `enroll_reject` entry of the first
    /// rejection, and `None` for a repeat (or an entry that could not be written, logged).
    ///
    /// # Errors
    /// `invalid_request` (the request id or the reason), `unknown_request`, `not_pending` (the
    /// request was approved or issued, or its host was retired), `store_failed`. Nothing
    /// changes, and nothing is audited.
    pub fn reject(
        &self,
        by: &str,
        request: &proto::RejectRequest,
    ) -> Result<Option<AuditEntry>, EnrollmentRefusal> {
        let id = hostkeeper_enrollment::names::KeyHash::from_hex(&request.request_id)
            .ok_or(EnrollmentRefusal::InvalidRequest)?;
        let why = optional_reason(&request.reason, EnrollmentRefusal::InvalidRequest)?;
        let rejection = self.store.reject(&id).map_err(|error| refusal_of(&error))?;
        if !rejection.first {
            return Ok(None);
        }
        Ok(self.audit(reject_entry(by, &rejection.request, why)))
    }

    /// Writes the deny row of the enrolled host `host` ([`EnrollmentStore::deny`]): its
    /// heartbeats and its `Renew` are refused `denied` from now on. Returns whether the row is
    /// new: `false` for a host denied already.
    ///
    /// # Errors
    /// `unknown_host`: the store approved no host with the id (a farm-CA host, or none);
    /// `store_failed` (logged).
    pub fn deny_host(&self, host: &hostkeeper_wire::HostId) -> Result<bool, HostsRefusal> {
        let store: &EnrollmentStore = &self.store;
        let failed = |why: hostkeeper_enrollment::enroll::StoreError| {
            tracing::error!(%why, host_id = %host, "a deny row could not be written");
            HostsRefusal::StoreFailed
        };
        let enrolled = super::store_host(host)
            .map(|id| store.host_role(&id).map(|role| role.map(|_| id)))
            .transpose()
            .map_err(failed)?
            .flatten()
            .ok_or(HostsRefusal::UnknownHost)?;
        store.deny(&enrolled).map_err(failed)
    }
}
