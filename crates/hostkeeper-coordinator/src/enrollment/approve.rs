//! `Enrollment.Approve` as a plain function over the store (design 5.7 step 3), and the audit
//! of every approval and refused approval.
//!
//! [`Enrollment::approve`] checks the request's fields, each against its own refusal, then
//! the key hash against the pending request's (in constant time), then the host id against the
//! host inventory: a request (not a renewal) that matches exactly one record is approved as that
//! record's host id, which an empty host id defaults to, and a host id other than the record's is
//! `record_mismatch` unless the approval replaces. Before that, a request that matches a record
//! another pending request matches is `record_contested` ([`Enrollment::contests`]), whatever
//! the approval names and even when it replaces: the operator rejects all but one first. A request that matches no record, or several,
//! is approved as the host id named, which it must name. Then it calls the store's
//! [`EnrollmentStore::approve_recorded`], which checks again under its lock: the request is
//! pending, not a `duplicate_claim` unless the approval replaces, the host id is not held by
//! another key nor denied unless it replaces. That one write records the approval with the
//! host id, role, services (the host's first assignment), host group, approver, time and reason;
//! issuance stays with `Poll`.
//!
//! From a coordinator that holds host groups ([`Enrollment::held_groups`]) an approval places
//! its host in a group (`docs/design/hostgroups.md` section 1.2): the one its `host_group` names,
//! or else its `role`, read as a group's name ([`group_in`]), which must be held
//! (`unknown_host_group`); one of the two is required (`invalid_role`), and the role the store
//! writes is the group's name. It names no services (`services_retired`): the group decides what
//! the host runs. A renewal, or a `replace` of a host in a group, keeps that group: naming
//! another is `host_group_differs`, checked by the store under its lock. A coordinator that
//! holds none approves by role and services, as before groups, and refuses a `host_group`
//! (`unknown_host_group`).
//!
//! The audit entry lives in the coordinator's ops store, a second file, so it cannot be in the
//! approval's write. It is made from the approval record right after that write
//! ([`enroll_approve`](ACTION_APPROVE) or [`enroll_replace`](ACTION_REPLACE)), and again at the
//! coordinator's start for any recorded approval the audit log does not hold
//! ([`Enrollment::audit_to`]). A refused approval is audited `enroll_approve_refused` with its
//! token, and changes nothing else.
//!
//! An approval the coordinator made itself by an inventory record ([`INVENTORY_APPROVER`], see
//! `by_record`) is audited `enrollment.auto_approved`, in one change of the ops file with the
//! mark that the record is used, and at the start that change is made again for one whose entry
//! the audit log lacks. `Approve` does not mark a record used.

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
use super::by_record::{ACTION_AUTO_APPROVED, INVENTORY_APPROVER, record_auto_approval};
use super::expiry::ExpiriesAudited;
use super::groups::group_in;
use crate::host_groups::HostGroups;
use crate::inventory::{InventoryMatch, InventoryRecord, match_claims};
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

/// The fields of an approval as the operator named them, checked: the host id is `None` when
/// none was named, for the inventory to fill in ([`host_id_for`]).
struct Asked {
    request_id: KeyHash,
    key_sha256: KeyHash,
    host_id: Option<HostId>,
    role: Role,
    services: Vec<ServiceName>,
    /// The host group, from a coordinator that holds one.
    host_group: Option<String>,
    replace: bool,
    reason: String,
}

impl Asked {
    /// The approval asked for, as `host_id`.
    fn decided(self, host_id: HostId) -> Checked {
        Checked {
            request_id: self.request_id,
            key_sha256: self.key_sha256,
            decision: Decision {
                host_id,
                role: self.role,
                services: self.services,
                replace: self.replace,
                host_group: self.host_group,
            },
            reason: self.reason,
        }
    }
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
pub(super) fn role_of(role: &str) -> Option<Role> {
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

/// `request`'s fields checked, each against its own refusal; an empty host id is none named. With
/// `held`, the host groups the coordinator holds, the host is placed by group, as the module
/// says; without, by its role and services.
fn check(
    request: proto::ApproveRequest,
    known: &dyn Fn(&str) -> bool,
    held: Option<&HostGroups>,
) -> Result<Asked, EnrollmentRefusal> {
    // A coordinator that holds no group refuses a group first, as it did before groups.
    if held.is_none() && !request.host_group.is_empty() {
        return Err(EnrollmentRefusal::UnknownHostGroup);
    }
    let invalid = EnrollmentRefusal::InvalidRequest;
    let request_id = KeyHash::from_hex(&request.request_id).ok_or(invalid)?;
    let key_sha256 = KeyHash::from_hex(&request.key_sha256).ok_or(invalid)?;
    let host_id = match request.host_id.as_str() {
        "" => None,
        text => Some(HostId::parse(text).map_err(|_| EnrollmentRefusal::InvalidHostId)?),
    };
    let (role, services, host_group) = match held {
        None => (
            role_of(&request.role).ok_or(EnrollmentRefusal::InvalidRole)?,
            services_of(&request.services, known).ok_or(EnrollmentRefusal::InvalidService)?,
            None,
        ),
        Some(_) if !request.services.is_empty() => {
            return Err(EnrollmentRefusal::ServicesRetired);
        }
        Some(held) => {
            let group = group_in(&request.host_group, &request.role, held)?
                .ok_or(EnrollmentRefusal::InvalidRole)?;
            // A group's name follows the operator API's rule of a role, which is the stricter.
            let role = role_of(group.as_str()).ok_or(EnrollmentRefusal::InvalidRole)?;
            (role, Vec::new(), Some(group.to_string()))
        }
    };
    let reason = valid_reason(&request.reason).ok_or(invalid)?;
    Ok(Asked {
        request_id,
        key_sha256,
        host_id,
        role,
        services,
        host_group,
        replace: request.replace,
        reason,
    })
}

/// The host id an approval `asked` of `pending` is made as, by the host inventory `inventory`.
/// A request that is not a renewal and matches exactly one record ([`match_claims`]) is approved
/// as that record's host id: an empty host id is it, and another is `record_mismatch` unless the
/// approval replaces. Any other request (a renewal, which the store holds to the host id it
/// renews, or one that matches no record or several) is approved as the host id named, and
/// naming none is `invalid_host_id`.
fn host_id_for(
    asked: &Asked,
    pending: &RequestView,
    inventory: &[InventoryRecord],
) -> Result<HostId, EnrollmentRefusal> {
    let record = match match_claims(&pending.claims, inventory) {
        InventoryMatch::Matched(record) if pending.renewal_of.is_none() => Some(record),
        _ => None,
    };
    match (&asked.host_id, record) {
        (None, Some(record)) => {
            // The ops file holds a record's host id by the same grammar; one edited by hand
            // to break it names no host.
            HostId::parse(&record.host_id).map_err(|_| EnrollmentRefusal::InvalidHostId)
        }
        (None, None) => Err(EnrollmentRefusal::InvalidHostId),
        (Some(named), Some(record)) if named.as_str() != record.host_id && !asked.replace => {
            Err(EnrollmentRefusal::RecordMismatch)
        }
        (Some(named), _) => Ok(named.clone()),
    }
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
        ApproveError::HostGroupDiffers { .. } => EnrollmentRefusal::HostGroupDiffers,
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
pub(super) fn claims_summary(claims: &Claims) -> String {
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
/// `request <id>;`, which is how a start finds the approvals already audited. An approval by an
/// inventory record is `enrollment.auto_approved` and names the record after the request. An
/// approval that put its host in a group names the group (`group web`), in its reason and as the
/// entry's `host_group`, in place of its role and services.
pub(super) fn approval_entry(approval: &RecordedApproval) -> AuditEntry {
    let (action, record) = if approval.approver == INVENTORY_APPROVER {
        (
            ACTION_AUTO_APPROVED,
            format!("record {}; ", approval.host_id),
        )
    } else if approval.replace {
        (ACTION_REPLACE, String::new())
    } else {
        (ACTION_APPROVE, String::new())
    };
    let services = if approval.services.is_empty() {
        "none".to_owned()
    } else {
        approval.services.join(",")
    };
    let placed = if approval.host_group.is_empty() {
        format!("role {}; services {services}", approval.role)
    } else {
        format!("group {}", approval.host_group)
    };
    let mut entry = AuditEntry::new(&approval.approver, action);
    entry.node_id = Some(approval.host_id.clone());
    entry.host_group = Some(approval.host_group.clone()).filter(|group| !group.is_empty());
    entry.reason = Some(format!(
        "request {}; {record}{placed}; claims {}; {}",
        approval.request_id,
        claims_summary(&approval.claims),
        approval.reason
    ));
    entry
}

/// The request id an approval's audit entry names, if `entry` is one.
fn audited_request(entry: &AuditEntry) -> Option<&str> {
    if ![ACTION_APPROVE, ACTION_REPLACE, ACTION_AUTO_APPROVED].contains(&entry.action.as_str()) {
        return None;
    }
    entry.reason.as_deref()?.strip_prefix("request ")?.get(..64)
}

impl Enrollment {
    /// From now on, audits into `ops`; and audits there now every approval the store recorded,
    /// and every expiry, whose entry the audit log does not hold (one whose audit did not land
    /// before a crash). Once the audit file's retention has deleted segments, an approval made
    /// (or a request expired) before the oldest line kept is not audited again: its entry went
    /// with its segment.
    /// Called when a coordinator takes this enrollment, at its start.
    pub fn audit_to(&self, ops: Arc<OpsStore>) {
        let mut audited = BTreeSet::new();
        let mut expiries = ExpiriesAudited::default();
        let read = ops
            .audit_each(&mut |entry| {
                if let Some(request) = audited_request(&entry) {
                    audited.insert(request.to_owned());
                }
                expiries.see(&entry);
            })
            .and_then(|()| ops.audit_pruned_before());
        match (self.store.recorded_approvals(), &read) {
            (Ok(approvals), Ok(pruned_before)) => {
                for approval in approvals
                    .iter()
                    .filter(|approval| !audited.contains(approval.request_id.to_hex().as_str()))
                    .filter(|approval| pruned_before.is_none_or(|t| approval.approved_unix >= t))
                {
                    tracing::info!(
                        request_id = %approval.request_id,
                        "auditing an approval whose entry did not land before the last stop"
                    );
                    if approval.approver == INVENTORY_APPROVER {
                        record_auto_approval(&ops, approval);
                    } else {
                        write_entry(&ops, approval_entry(approval));
                    }
                }
            }
            (Err(why), _) => {
                tracing::error!(%why, "cannot read the recorded approvals: none is audited again");
            }
            (_, Err(why)) => {
                tracing::error!(%why, "cannot read the audit log: no approval is audited again");
            }
        }
        if let Ok(pruned_before) = read {
            self.audit_missing_expiries(&ops, &expiries, pruned_before);
        }
        *self.audit.lock().unwrap_or_else(PoisonError::into_inner) = Some(ops);
    }

    /// The ops store enrollment audits into; `None` until a coordinator takes this enrollment.
    pub(super) fn ops(&self) -> Option<Arc<OpsStore>> {
        self.audit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Writes `entry` to the ops store enrollment audits into: the entry as numbered, or `None`
    /// when there is none yet or the write failed (logged).
    pub(super) fn audit(&self, entry: AuditEntry) -> Option<AuditEntry> {
        match self.ops() {
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
    /// pending request it names is approved as the role and services it names, which `known`
    /// says the coordinator manages, or, from a coordinator that holds host groups, in the group
    /// it names (the module says how), and as the host id [`host_id_for`] gives by the host
    /// inventory `inventory`: the one matching record's when the request matches one, which an
    /// empty host id defaults to. The certificate is made at the host's next `Poll`. The caller
    /// holds the lock `HostGroups.Remove` checks under, so the group named is not removed while
    /// the approval is written.
    ///
    /// # Errors
    /// `invalid_request` (the request id, the key hash or the reason), `invalid_host_id`,
    /// `invalid_role`, `invalid_service`, `services_retired`, `unknown_host_group`,
    /// `unknown_request`, `key_mismatch`, `record_contested`, `record_mismatch`, `not_pending`,
    /// `duplicate_claim`, `host_id_taken`, `denied`, `host_group_differs`, `store_failed`. Each is
    /// audited `enroll_approve_refused`, and nothing else changes.
    pub fn approve(
        &self,
        by: &str,
        request: proto::ApproveRequest,
        known: &dyn Fn(&str) -> bool,
        inventory: &[InventoryRecord],
    ) -> Result<Approved, EnrollmentRefusal> {
        let id = KeyHash::from_hex(&request.request_id);
        let asked = match check(request, known, self.held_groups().as_ref()) {
            Ok(asked) => asked,
            Err(refusal) => {
                self.audit_refused(by, refusal, id.as_ref(), None);
                return Err(refusal);
            }
        };
        let request_id = asked.request_id;
        let refused = |refusal: EnrollmentRefusal, host_id: Option<&HostId>| {
            self.audit_refused(by, refusal, Some(&request_id), host_id);
            refusal
        };
        let pending = self
            .pending_for(&asked.request_id, &asked.key_sha256)
            .and_then(|pending| self.uncontested(pending, inventory))
            .and_then(|pending| Ok((host_id_for(&asked, &pending, inventory)?, pending)));
        let (host_id, pending) =
            pending.map_err(|refusal| refused(refusal, asked.host_id.as_ref()))?;
        let checked = asked.decided(host_id);
        let record = ApprovalRecord {
            approver: by.to_owned(),
            approved_unix: self.clock.now_unix(),
            reason: checked.reason.clone(),
            mac_nonce: None,
            mac_enrolled_since_unix: None,
        };
        let approval = self
            .store
            .approve_recorded(
                &checked.request_id,
                &checked.key_sha256,
                &checked.decision,
                &record,
            )
            .map_err(|error| refused(refusal_of(&error), Some(&checked.decision.host_id)))?;
        Ok(self.audited(&checked, &record, pending.claims, &approval.host_group))
    }

    /// The request `request_id` names, when it exists and `key_sha256` is its key's hash
    /// (compared in constant time). The store checks both again under its lock.
    fn pending_for(
        &self,
        request_id: &KeyHash,
        key_sha256: &KeyHash,
    ) -> Result<RequestView, EnrollmentRefusal> {
        let pending = self
            .store
            .request(request_id)
            .map_err(|why| {
                tracing::error!(%why, "cannot read a pending request");
                EnrollmentRefusal::StoreFailed
            })?
            .ok_or(EnrollmentRefusal::UnknownRequest)?;
        if !key_sha256.equals_in_constant_time(request_id) {
            return Err(EnrollmentRefusal::KeyMismatch);
        }
        Ok(pending)
    }

    /// `pending`, unless another pending request matches an inventory record it matches
    /// (`record_contested`). A request that matches no record is never contested, and the
    /// pending requests are read only for one that matches.
    fn uncontested(
        &self,
        pending: RequestView,
        inventory: &[InventoryRecord],
    ) -> Result<RequestView, EnrollmentRefusal> {
        if match_claims(&pending.claims, inventory) == InventoryMatch::Unmatched {
            return Ok(pending);
        }
        if self.contests(inventory)?.contains_key(&pending.request_id) {
            return Err(EnrollmentRefusal::RecordContested);
        }
        Ok(pending)
    }

    /// The approval of `checked` with `record`, which the store just wrote for a request that
    /// claimed `claims`, putting its host in `host_group` (empty for none), audited.
    pub(super) fn audited(
        &self,
        checked: &Checked,
        record: &ApprovalRecord,
        claims: Claims,
        host_group: &str,
    ) -> Approved {
        let approval = self.recorded(checked, record, claims, host_group);
        Approved {
            host_id: checked.decision.host_id.clone(),
            entry: self.audit(approval_entry(&approval)),
        }
    }

    /// The approval the store just wrote for `checked` with `record`, for a request that claimed
    /// `claims`, putting its host in `host_group` (empty for none), as the store lists it
    /// ([`EnrollmentStore::recorded_approvals`]).
    ///
    /// [`EnrollmentStore::recorded_approvals`]: hostkeeper_enrollment::enroll::EnrollmentStore::recorded_approvals
    pub(super) fn recorded(
        &self,
        checked: &Checked,
        record: &ApprovalRecord,
        claims: Claims,
        host_group: &str,
    ) -> RecordedApproval {
        RecordedApproval {
            request_id: checked.request_id,
            host_id: checked.decision.host_id.to_string(),
            role: checked.decision.role.as_str().to_owned(),
            host_group: host_group.to_owned(),
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
        }
    }
}

/// Writes `entry` to `ops` as a change of nothing but the audit: the entry as numbered, or
/// `None` when the write failed (logged).
pub(super) fn write_entry(ops: &OpsStore, entry: AuditEntry) -> Option<AuditEntry> {
    match ops.change(Write::BeforeMemory, entry, |_| {}) {
        Ok(entry) => Some(entry),
        Err(why) => {
            tracing::error!(%why, "an enrollment audit entry could not be written");
            None
        }
    }
}
