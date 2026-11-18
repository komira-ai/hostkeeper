//! Approval by the host inventory: entering a record that names a host group (or a role) is the
//! approval of the host it was entered for, so the coordinator approves a pending request by the
//! record itself, with no `Enrollment.Approve`.
//!
//! [`Enrollment::approve_by_record`] approves the pending request `request_id` when every
//! condition holds, and otherwise leaves it pending for an operator ([`Waits`] says which did
//! not):
//!
//! - it is not a renewal, and its state is pending (not a `duplicate_claim`);
//! - the records match it ([`match_claims`]) as exactly one record, open or not: an ambiguous
//!   match stays with the operator;
//! - the match is exact ([`InventoryRecord::matches_exactly`]): by a data MAC among the claimed
//!   addresses and, when the record holds a BMC MAC, that BMC MAC claimed; a Mac by its serial;
//! - the record is open (the time is before its `approve_until_unix`), unused, and names a host
//!   group or a role ([`InventoryRecord::group`]);
//! - from a coordinator that holds host groups, that name is a group it holds: a record added
//!   before the coordinator held groups names one only by its role, which no group may have;
//! - no other pending request contests the record ([`Enrollment::contests`]).
//!
//! It then approves through the store's [`EnrollmentStore::approve_recorded`] as an operator's
//! approval is made, as the record's host id and role, bound to the SHA-256 of the request's key,
//! with no service (a record names none), by [`INVENTORY_APPROVER`]; from a coordinator that
//! holds host groups, in the record's group, whose name is then the role
//! (`docs/design/hostgroups.md` section 1.2). The store checks again under
//! its lock what an operator's approval is held to: a host id another key holds, a denied host id,
//! a duplicate claim and a request no longer pending each leave the request pending (logged, not
//! audited, the record untouched). That one host per host id is what keeps two requests that
//! race for one record from both using it.
//!
//! The audit entry (`enrollment.auto_approved`) and the mark that the record is used are one
//! change of the ops file, made right after the store's write ([`record_auto_approval`]). When
//! that write fails the approval stands (the store holds the host id, so the record cannot
//! approve another key), the failure is logged, and the coordinator's start makes the change
//! again ([`Enrollment::audit_to`]).
//!
//! It runs when a request is posted ([`crate::service::Coordinator::enroll`]), and when a record
//! is added or opened ([`Enrollment::approve_matching`]), since a daemon posts its request once
//! and then only polls.
//!
//! [`EnrollmentStore::approve_recorded`]: hostkeeper_enrollment::enroll::EnrollmentStore::approve_recorded

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, Decision, RecordedApproval, RequestState, RequestView,
};
use hostkeeper_enrollment::names::{HostId, KeyHash};
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use super::Enrollment;
use super::approve::{Checked, approval_entry, refusal_of, role_of, token_of};
use super::groups::group_in;
use crate::inventory::{InventoryMatch, InventoryRecord, mark_used, match_claims};
use crate::ops::{AuditEntry, OpsStore, Write};

/// The approver of an approval made by an inventory record.
pub const INVENTORY_APPROVER: &str = "inventory";

/// The audit action of an approval made by an inventory record.
pub const ACTION_AUTO_APPROVED: &str = "enrollment.auto_approved";

/// Why a request was left pending for an operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waits {
    /// The request is unknown, or is no longer pending (approved, issued, denied or expired).
    NotPending,
    /// The request is a `duplicate_claim`: it needs `Approve` with `replace`.
    DuplicateClaim,
    /// The request renews a host's certificate.
    Renewal,
    /// No record matches the request.
    Unmatched,
    /// More than one record matches the request.
    Ambiguous,
    /// The one record matches, but not exactly: the request claims no BMC MAC, or another one,
    /// than the record holds.
    Inexact,
    /// The record is used.
    Used,
    /// The record names neither a host group nor a role.
    NoRole,
    /// The coordinator holds host groups, and none has the name the record gives: a record
    /// added before it held groups, whose role names none.
    UnknownHostGroup,
    /// The record's window is closed.
    Closed,
    /// Another pending request matches the record too.
    Contested,
    /// The record's host id or role breaks its rule (an ops file edited by hand).
    BadRecord,
    /// The store refused the approval, as it would an operator's.
    Refused(EnrollmentRefusal),
    /// The request, or the pending requests, could not be read.
    Unreadable,
}

/// What [`Enrollment::approve_by_record`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ByRecord {
    /// The request was approved as the record's host id and role, or group.
    Approved {
        /// The host id the certificate will name.
        host_id: HostId,
        /// The `enrollment.auto_approved` entry as the ops store numbered it; `None` when the
        /// change that makes it could not be written (logged; it is made at the next start).
        entry: Option<Box<AuditEntry>>,
    },
    /// The request stays pending.
    Waits(Waits),
}

/// Writes the audit entry of `approval`, an approval made by an inventory record, and marks the
/// record of its host id used by it, as one change of `ops`: the entry as numbered, or `None`
/// when the write failed (logged). A record that is gone or already used is left as it is.
pub(super) fn record_auto_approval(
    ops: &OpsStore,
    approval: &RecordedApproval,
) -> Option<AuditEntry> {
    let request = approval.request_id.to_hex();
    let changed = ops.change(Write::BeforeMemory, approval_entry(approval), |state| {
        mark_used(
            &mut state.inventory,
            &approval.host_id,
            &request,
            approval.approved_unix,
        );
    });
    match changed {
        Ok(entry) => Some(entry),
        Err(why) => {
            tracing::error!(
                %why,
                request_id = %request,
                "the audit entry of an inventory approval and the mark that its record is used \
                 could not be written: the approval stands, and they are written at the next start"
            );
            None
        }
    }
}

impl Enrollment {
    /// Approves the pending request `request_id` by the record of `inventory` it matches, when
    /// every condition in the module's header holds; otherwise the request stays as it was.
    pub fn approve_by_record(
        &self,
        request_id: &KeyHash,
        inventory: &[InventoryRecord],
    ) -> ByRecord {
        let now = self.clock.now_unix();
        let (view, record) = match self.chosen(request_id, inventory, now) {
            Ok(chosen) => chosen,
            Err(waits) => return ByRecord::Waits(waits),
        };
        let (Ok(host_id), Some(role)) = (HostId::parse(&record.host_id), role_of(record.group()))
        else {
            return ByRecord::Waits(Waits::BadRecord);
        };
        let host_group = match self.held_groups() {
            None => None,
            Some(held) => match group_in(record.group(), "", &held) {
                Ok(group) => group.map(|group| group.to_string()),
                Err(_) => return ByRecord::Waits(Waits::UnknownHostGroup),
            },
        };
        let checked = Checked {
            request_id: *request_id,
            key_sha256: *request_id,
            decision: Decision {
                host_id: host_id.clone(),
                role,
                services: Vec::new(),
                replace: false,
                host_group,
            },
            reason: format!("approved by its inventory record {}", record.host_id),
        };
        let approval = ApprovalRecord {
            approver: INVENTORY_APPROVER.to_owned(),
            approved_unix: now,
            reason: checked.reason.clone(),
            mac_nonce: None,
            mac_enrolled_since_unix: None,
        };
        let approved = match self.store.approve_recorded(
            &checked.request_id,
            &checked.key_sha256,
            &checked.decision,
            &approval,
        ) {
            Ok(approved) => approved,
            Err(error) => {
                let refusal = refusal_of(&error);
                tracing::info!(
                    request_id = %request_id,
                    host_id = %record.host_id,
                    refusal = %token_of(refusal),
                    "an inventory record did not approve its request: it stays pending"
                );
                return ByRecord::Waits(Waits::Refused(refusal));
            }
        };
        let recorded = self.recorded(&checked, &approval, view.claims, &approved.host_group);
        let entry = self
            .ops()
            .and_then(|ops| record_auto_approval(&ops, &recorded))
            .map(Box::new);
        tracing::info!(
            request_id = %request_id,
            host_id = %host_id,
            "a request was approved by its inventory record"
        );
        ByRecord::Approved { host_id, entry }
    }

    /// Tries [`Self::approve_by_record`] on every pending request that matches the record of
    /// `host_id` in `inventory` (the records as they are now): the requests it was tried on, in
    /// the order the store lists them, with what became of each.
    pub fn approve_matching(
        &self,
        host_id: &str,
        inventory: &[InventoryRecord],
    ) -> Vec<(KeyHash, ByRecord)> {
        let pending = match self.store.pending() {
            Ok(pending) => pending,
            Err(why) => {
                tracing::error!(%why, "cannot read the pending requests: none is approved by a record");
                return Vec::new();
            }
        };
        pending
            .iter()
            .filter(|view| {
                matches!(
                    match_claims(&view.claims, inventory),
                    InventoryMatch::Matched(record) if record.host_id == host_id
                )
            })
            .map(|view| {
                (
                    view.request_id,
                    self.approve_by_record(&view.request_id, inventory),
                )
            })
            .collect()
    }

    /// The request `request_id` and the record of `inventory` that may approve it at `now`, or
    /// what makes it wait.
    fn chosen<'a>(
        &self,
        request_id: &KeyHash,
        inventory: &'a [InventoryRecord],
        now: u64,
    ) -> Result<(RequestView, &'a InventoryRecord), Waits> {
        let view = self
            .store
            .request(request_id)
            .map_err(|why| {
                tracing::error!(%why, "cannot read a request: it is not approved by a record");
                Waits::Unreadable
            })?
            .ok_or(Waits::NotPending)?;
        if view.renewal_of.is_some() {
            return Err(Waits::Renewal);
        }
        match view.state {
            RequestState::Pending => {}
            RequestState::DuplicateClaim => return Err(Waits::DuplicateClaim),
            _ => return Err(Waits::NotPending),
        }
        let record = match match_claims(&view.claims, inventory) {
            InventoryMatch::Matched(record) => record,
            InventoryMatch::Unmatched => return Err(Waits::Unmatched),
            InventoryMatch::Ambiguous(_) => return Err(Waits::Ambiguous),
        };
        if !record.matches_exactly(&view.claims) {
            return Err(Waits::Inexact);
        }
        if record.used.is_some() {
            return Err(Waits::Used);
        }
        if record.group().is_empty() {
            return Err(Waits::NoRole);
        }
        if now >= record.approve_until_unix {
            return Err(Waits::Closed);
        }
        let contested = self.contests(inventory).map_err(|_| Waits::Unreadable)?;
        if contested.contains_key(request_id) {
            return Err(Waits::Contested);
        }
        Ok((view, record))
    }
}
