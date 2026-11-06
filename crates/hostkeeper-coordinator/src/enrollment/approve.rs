//! `Enrollment.Approve` as a plain function over the store (design 5.7 step 3), and the audit
//! of every approval and refused approval.
//!
//! [`Enrollment::approve`] checks the request's fields, each against its own refusal, then
//! the key hash against the pending request's (in constant time), and calls the store's
//! [`EnrollmentStore::approve_recorded`], which checks again under its lock: the request is
//! pending, not a `duplicate_claim` unless the approval replaces, the host id is not held by
//! another key nor denied unless it replaces. That one write records the approval with the
//! host id, role, services (the host's first assignment), approver, time and reason; issuance
//! stays with `Poll`.
//!
//! The audit entry lives in the coordinator's ops store, a second file, so it cannot be in the
//! approval's write. It is made from the approval record right after that write
//! ([`enroll_approve`](ACTION_APPROVE) or [`enroll_replace`](ACTION_REPLACE)), and again at the
//! coordinator's start for any recorded approval the audit log does not hold
//! ([`Enrollment::audit_to`]). A refused approval is audited `enroll_approve_refused` with its
//! token, and changes nothing else.

use std::collections::BTreeSet;
use std::sync::{Arc, PoisonError};

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, Claims, Decision, RecordedApproval, RequestView,
};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role, ServiceName};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;
use hostkeeper_wire::RoleName;

use super::Enrollment;
use crate::operator::valid_reason;
use crate::ops::{AuditEntry, OpsStore, Write};

/// The audit action of an approval.
pub const ACTION_APPROVE: &str = "enroll_approve";

/// The audit action of an approval that takes a host id over (`replace`).
pub const ACTION_REPLACE: &str = "enroll_replace";

/// The audit action of a refused approval.
pub const ACTION_REFUSED: &str = "enroll_approve_refused";

/// The most bytes of one claim an audit entry quotes.
const CLAIM_BYTES: usize = 64;

/// An approval made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    /// The host id the certificate will name.
    pub host_id: HostId,
    /// The audit entry, as the ops store numbered it; `None` when it could not be written
    /// (logged; it is written at the coordinator's next start).
    pub entry: Option<AuditEntry>,
}

/// The fields of an approval, checked.
pub(super) struct Checked {
    pub request_id: KeyHash,
    pub key_sha256: KeyHash,
    pub decision: Decision,
    pub reason: String,
}

/// The token of `refusal`: its name without the prefix, in lower case.
#[must_use]
pub fn token_of(refusal: EnrollmentRefusal) -> String {
    refusal
        .as_str_name()
        .trim_start_matches("ENROLLMENT_REFUSAL_")
        .to_ascii_lowercase()
}

/// `role` if it is one both by the operator API's rule (a letter first) and the store's.
fn role_of(role: &str) -> Option<Role> {
    role.parse::<RoleName>().ok()?;
    Role::parse(role).ok()
}

/// The services named, each by its rule and one `known` says the coordinator manages, without
/// repeats, in name order.
fn services_of(names: &[String], known: &dyn Fn(&str) -> bool) -> Option<Vec<ServiceName>> {
    let unique: BTreeSet<&String> = names.iter().collect();
    unique
        .into_iter()
        .map(|name| ServiceName::parse(name).ok().filter(|_| known(name)))
        .collect()
}

/// `request`'s fields checked, each against its own refusal.
fn check(
    request: proto::ApproveRequest,
    known: &dyn Fn(&str) -> bool,
) -> Result<Checked, EnrollmentRefusal> {
    let invalid = EnrollmentRefusal::InvalidRequest;
    let request_id = KeyHash::from_hex(&request.request_id).ok_or(invalid)?;
    let key_sha256 = KeyHash::from_hex(&request.key_sha256).ok_or(invalid)?;
    let host_id = HostId::parse(&request.host_id).map_err(|_| EnrollmentRefusal::InvalidHostId)?;
    let role = role_of(&request.role).ok_or(EnrollmentRefusal::InvalidRole)?;
    let services =
        services_of(&request.services, known).ok_or(EnrollmentRefusal::InvalidService)?;
    let reason = valid_reason(&request.reason).ok_or(invalid)?;
    Ok(Checked {
        request_id,
        key_sha256,
        decision: Decision {
            host_id,
            role,
            services,
            replace: request.replace,
        },
        reason,
    })
}

/// The refusal an error of the store's approval is; a failure of the store is logged.
pub(super) fn refusal_of(error: &ApproveError) -> EnrollmentRefusal {
    match error {
        ApproveError::UnknownRequest => EnrollmentRefusal::UnknownRequest,
        ApproveError::KeyMismatch => EnrollmentRefusal::KeyMismatch,
        ApproveError::NotPending(_) => EnrollmentRefusal::NotPending,
        ApproveError::HostIdTaken => EnrollmentRefusal::HostIdTaken,
        ApproveError::Denied => EnrollmentRefusal::Denied,
        // A renewal is approved under the host id it renews, and no other.
        ApproveError::RenewalOfOtherHost => EnrollmentRefusal::InvalidHostId,
        ApproveError::DuplicateClaim => EnrollmentRefusal::DuplicateClaim,
        // Only the Mac path names a nonce, and it declines on a nonce refusal before it asks
        // this; what remains is a failure of the store.
        ApproveError::MacNonce(_) | ApproveError::Store(_) => {
            tracing::error!(%error, "an approval could not be written: nothing changed");
            EnrollmentRefusal::StoreFailed
        }
    }
}

/// `text`, a host's claim, as an audit entry quotes it: at most [`CLAIM_BYTES`] bytes, every
/// character that is not printable ASCII written `?`.
fn quoted(text: &str) -> String {
    text.chars()
        .take(CLAIM_BYTES)
        .map(|c| if c.is_ascii_graphic() { c } else { '?' })
        .collect()
}

/// The claims of a request, as an audit entry summarises them: the ones it made.
fn claims_summary(claims: &Claims) -> String {
    let addresses = claims
        .addresses
        .iter()
        .map(|address| quoted(address))
        .collect::<Vec<_>>()
        .join(",");
    [
        ("machine_id", quoted(&claims.machine_id)),
        ("dmi_serial", quoted(&claims.dmi_serial)),
        ("product_uuid", quoted(&claims.dmi_product_uuid)),
        ("mac_serial", quoted(&claims.mac_serial)),
        ("platform_uuid", quoted(&claims.mac_platform_uuid)),
        ("addresses", addresses),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| format!("{name}={value}"))
    .collect::<Vec<_>>()
    .join(" ")
}

/// The audit entry of `approval`: by its approver, about its host id; its reason starts with
/// `request <id>;`, which is how a start finds the approvals already audited.
pub(super) fn approval_entry(approval: &RecordedApproval) -> AuditEntry {
    let action = if approval.replace {
        ACTION_REPLACE
    } else {
        ACTION_APPROVE
    };
    let services = if approval.services.is_empty() {
        "none".to_owned()
    } else {
        approval.services.join(",")
    };
    let mut entry = AuditEntry::new(&approval.approver, action);
    entry.node_id = Some(approval.host_id.clone());
    entry.reason = Some(format!(
        "request {}; role {}; services {services}; claims {}; {}",
        approval.request_id,
        approval.role,
        claims_summary(&approval.claims),
        approval.reason
    ));
    entry
}

/// The request id an approval's audit entry names, if `entry` is one.
fn audited_request(entry: &AuditEntry) -> Option<&str> {
    if entry.action != ACTION_APPROVE && entry.action != ACTION_REPLACE {
        return None;
    }
    entry.reason.as_deref()?.strip_prefix("request ")?.get(..64)
}

impl Enrollment {
    /// From now on, audits into `ops`; and audits there now every approval the store recorded
    /// whose entry the audit log does not hold (one whose audit did not land before a crash).
    /// Once the audit file's retention has deleted segments, an approval made before the oldest
    /// line kept is not audited again: its entry went with its segment.
    /// Called when a coordinator takes this enrollment, at its start.
    pub fn audit_to(&self, ops: Arc<OpsStore>) {
        let kept = ops
            .audit_log()
            .and_then(|log| Ok((log, ops.audit_pruned_before()?)));
        match (self.store.recorded_approvals(), kept) {
            (Ok(approvals), Ok((log, pruned_before))) => {
                let audited: BTreeSet<&str> = log.iter().filter_map(audited_request).collect();
                for approval in approvals
                    .iter()
                    .filter(|approval| !audited.contains(approval.request_id.to_hex().as_str()))
                    .filter(|approval| pruned_before.is_none_or(|t| approval.approved_unix >= t))
                {
                    tracing::info!(
                        request_id = %approval.request_id,
                        "auditing an approval whose entry did not land before the last stop"
                    );
                    write_entry(&ops, approval_entry(approval));
                }
            }
            (Err(why), _) => {
                tracing::error!(%why, "cannot read the recorded approvals: none is audited again");
            }
            (_, Err(why)) => {
                tracing::error!(%why, "cannot read the audit log: no approval is audited again");
            }
        }
        *self.audit.lock().unwrap_or_else(PoisonError::into_inner) = Some(ops);
    }

    /// Writes `entry` to the ops store enrollment audits into: the entry as numbered, or `None`
    /// when there is none yet or the write failed (logged).
    pub(super) fn audit(&self, entry: AuditEntry) -> Option<AuditEntry> {
        let ops = self
            .audit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match ops {
            Some(ops) => write_entry(&ops, entry),
            None => {
                tracing::info!(action = %entry.action, "no ops store to audit an enrollment change in");
                None
            }
        }
    }

    /// Audits that `by` was refused `refusal` approving `request_id` (when it was one).
    pub(super) fn audit_refused(
        &self,
        by: &str,
        refusal: EnrollmentRefusal,
        request_id: Option<&KeyHash>,
        host_id: Option<&HostId>,
    ) {
        let mut entry = AuditEntry::new(by, ACTION_REFUSED);
        entry.node_id = host_id.map(HostId::to_string);
        entry.reason = Some(match request_id {
            Some(id) => format!("{}; request {id}", token_of(refusal)),
            None => token_of(refusal),
        });
        self.audit(entry);
    }

    /// `Enrollment.Approve` by `by` (the name of the operator's token, already checked): the
    /// pending request it names is approved as the host id, role and services it names, which
    /// `known` says the coordinator manages. The certificate is made at the host's next `Poll`.
    ///
    /// # Errors
    /// `invalid_request` (the request id, the key hash or the reason), `invalid_host_id`,
    /// `invalid_role`, `invalid_service`, `unknown_request`, `key_mismatch`, `not_pending`,
    /// `duplicate_claim`, `host_id_taken`, `denied`, `store_failed`. Each is audited
    /// `enroll_approve_refused`, and nothing else changes.
    pub fn approve(
        &self,
        by: &str,
        request: proto::ApproveRequest,
        known: &dyn Fn(&str) -> bool,
    ) -> Result<Approved, EnrollmentRefusal> {
        let id = KeyHash::from_hex(&request.request_id);
        let checked = match check(request, known) {
            Ok(checked) => checked,
            Err(refusal) => {
                self.audit_refused(by, refusal, id.as_ref(), None);
                return Err(refusal);
            }
        };
        let record = ApprovalRecord {
            approver: by.to_owned(),
            approved_unix: self.clock.now_unix(),
            reason: checked.reason.clone(),
            mac_nonce: None,
        };
        self.pending_for(&checked)
            .and_then(|pending| {
                self.store
                    .approve_recorded(
                        &checked.request_id,
                        &checked.key_sha256,
                        &checked.decision,
                        &record,
                    )
                    .map_err(|error| refusal_of(&error))?;
                Ok(self.audited(&checked, &record, pending.claims))
            })
            .inspect_err(|refusal| {
                self.audit_refused(
                    by,
                    *refusal,
                    Some(&checked.request_id),
                    Some(&checked.decision.host_id),
                );
            })
    }

    /// The request `checked` names, when it exists and the key hash `checked` names is its
    /// key's (compared in constant time). The store checks both again under its lock.
    pub(super) fn pending_for(&self, checked: &Checked) -> Result<RequestView, EnrollmentRefusal> {
        let pending = self
            .store
            .request(&checked.request_id)
            .map_err(|why| {
                tracing::error!(%why, "cannot read a pending request");
                EnrollmentRefusal::StoreFailed
            })?
            .ok_or(EnrollmentRefusal::UnknownRequest)?;
        if !checked
            .key_sha256
            .equals_in_constant_time(&checked.request_id)
        {
            return Err(EnrollmentRefusal::KeyMismatch);
        }
        Ok(pending)
    }

    /// The approval of `checked` with `record`, which the store just wrote for a request that
    /// claimed `claims`, audited.
    pub(super) fn audited(
        &self,
        checked: &Checked,
        record: &ApprovalRecord,
        claims: Claims,
    ) -> Approved {
        let approval = RecordedApproval {
            request_id: checked.request_id,
            host_id: checked.decision.host_id.to_string(),
            role: checked.decision.role.as_str().to_owned(),
            services: checked
                .decision
                .services
                .iter()
                .map(|service| service.as_str().to_owned())
                .collect(),
            replace: checked.decision.replace,
            approver: record.approver.clone(),
            approved_unix: record.approved_unix,
            reason: record.reason.clone(),
            claims,
            mac_nonce: record.mac_nonce.is_some(),
        };
        Approved {
            host_id: checked.decision.host_id.clone(),
            entry: self.audit(approval_entry(&approval)),
        }
    }
}

/// Writes `entry` to `ops` as a change of nothing but the audit: the entry as numbered, or
/// `None` when the write failed (logged).
fn write_entry(ops: &OpsStore, entry: AuditEntry) -> Option<AuditEntry> {
    match ops.change(Write::BeforeMemory, entry, |_| {}) {
        Ok(entry) => Some(entry),
        Err(why) => {
            tracing::error!(%why, "an enrollment audit entry could not be written");
            None
        }
    }
}
