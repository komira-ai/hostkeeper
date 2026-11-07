//! The enrollment as the MDM policy gate's enrollment side asks it
//! ([`hostkeeper_mdm_gate::Enrollments`], docs/design/hostkeeper-mdm.md section 5.1): the
//! allowlist, the requests waiting for an operator that claim a Mac serial, when one claiming
//! it was last approved, a nonce bound to a waiting request, and the audit of a push.
//!
//! The gate is linked into the coordinator, so these are library calls over the store; the
//! gate judges which request is a candidate, and this answers what the store holds.

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_mdm_gate::{Enrollments, EnrollmentsError, Nonce, Waiting};

use super::Enrollment;
use crate::ops::AuditEntry;

/// Who the gate's audit lines are by.
pub const GATE_AUDITOR: &str = "mdm-gate";

fn store_error(why: impl std::fmt::Display) -> EnrollmentsError {
    EnrollmentsError(why.to_string())
}

impl Enrollments for Enrollment {
    fn allowed(&self, serial: &str) -> bool {
        self.mac.allowlist.get(serial).is_some()
    }

    fn waiting(&self, serial: &str) -> Result<Vec<Waiting>, EnrollmentsError> {
        Ok(self
            .store
            .pending()
            .map_err(store_error)?
            .into_iter()
            .filter(|request| request.claims.mac_serial == serial)
            .map(|request| Waiting {
                request_id: request.request_id,
                platform_uuid: request.claims.mac_platform_uuid,
                posted_unix: request.posted_unix,
                duplicate_claim: request.state == RequestState::DuplicateClaim,
            })
            .collect())
    }

    fn last_approval_unix(&self, serial: &str) -> Result<Option<u64>, EnrollmentsError> {
        Ok(self
            .store
            .recorded_approvals()
            .map_err(store_error)?
            .iter()
            .filter(|approval| approval.claims.mac_serial == serial)
            .map(|approval| approval.approved_unix)
            .max())
    }

    fn issue_nonce(&self, serial: &str, request_id: &KeyHash) -> Result<Nonce, EnrollmentsError> {
        self.issue_mac_nonce(serial, request_id)
            .map(|nonce| Nonce::new(nonce.as_bytes().to_vec()))
            .map_err(store_error)
    }

    fn audit(&self, action: &str, reason: &str) {
        let mut entry = AuditEntry::new(GATE_AUDITOR, action);
        entry.reason = Some(reason.to_owned());
        Enrollment::audit(self, entry);
    }
}
