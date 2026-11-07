//! The approval window of an inventory record, and `Enrollment.OpenRecord`: the part of
//! [`crate::inventory`] that makes a record the approval of the host it was entered for.
//!
//! A record that names a role is added with a window ([`RecordWindow::end`]): until the end the
//! caller named, for the seconds the caller named, or for the coordinator's
//! `--record-approve-for`, and never for longer than `--record-approve-max`. While the window is
//! open and no approval used the record, a pending request that matches the record exactly is
//! approved by the coordinator itself ([`crate::enrollment::Enrollment::approve_by_record`]);
//! that approval marks the record used ([`RecordUse`]), and a used record approves nothing
//! again. A record's state at a time is [`InventoryRecord::state_at`]: used, else open while the
//! time is before its end, else closed. `OpenRecord` ([`Coordinator::open_record`]) opens the
//! window of a closed record again, or moves the end of one still open, and changes nothing else
//! of the record; a used record is refused `record_used`.
//!
//! The window's limits live with the ops file that holds the records
//! ([`crate::ops::OpsStore::with_record_window`]), so they apply with or without enrollment.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;
use serde::{Deserialize, Serialize};

use super::{InventoryRecord, RecordIndex, record_proto, unreadable};
use crate::operator::{Caller, entry_proto, optional_reason};
use crate::ops::AuditEntry;
use crate::service::Coordinator;

/// `--record-approve-for` when it is not given: 24 hours.
pub const DEFAULT_APPROVE_FOR_SECS: u64 = 24 * 3600;

/// `--record-approve-max` when it is not given: 7 days.
pub const DEFAULT_APPROVE_MAX_SECS: u64 = 7 * 86_400;

/// The audit action of a record's window opened again.
pub const ACTION_RECORD_OPEN: &str = "record_open";

/// `--record-approve-for` is longer than `--record-approve-max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "--record-approve-for: {for_secs} seconds is longer than --record-approve-max, {max_secs} seconds: a record's default window may not outlast the longest one"
)]
pub struct WindowAboveMax {
    /// What `--record-approve-for` says, in seconds.
    pub for_secs: u64,
    /// What `--record-approve-max` says, in seconds.
    pub max_secs: u64,
}

/// How long a record's window is by default, and the longest it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordWindow {
    for_secs: u64,
    max_secs: u64,
}

impl Default for RecordWindow {
    /// 24 hours, and at most 7 days.
    fn default() -> Self {
        Self {
            for_secs: DEFAULT_APPROVE_FOR_SECS,
            max_secs: DEFAULT_APPROVE_MAX_SECS,
        }
    }
}

impl RecordWindow {
    /// The window of `for_secs` by default and at most `max_secs`.
    ///
    /// # Errors
    /// [`WindowAboveMax`]: the default is longer than the most.
    pub fn new(for_secs: u64, max_secs: u64) -> Result<Self, WindowAboveMax> {
        if for_secs > max_secs {
            return Err(WindowAboveMax { for_secs, max_secs });
        }
        Ok(Self { for_secs, max_secs })
    }

    /// The length of a window nobody named, in seconds.
    #[must_use]
    pub fn for_secs(&self) -> u64 {
        self.for_secs
    }

    /// The longest window, in seconds.
    #[must_use]
    pub fn max_secs(&self) -> u64 {
        self.max_secs
    }

    /// The end of the window of a record opened at `now`, in unix seconds: `until` when it is
    /// given, `now` and `for_secs` on when that is, else the default; 0 for a record that
    /// names no role (`has_role` false) and asks for none.
    ///
    /// # Errors
    /// `invalid_window`: both `until` and `for_secs` given, an `until` that is not after
    /// `now`, an `until` or a `for_secs` that makes the window longer than the most, or any
    /// window asked for a record that names no role.
    pub(super) fn end(
        &self,
        until: u64,
        for_secs: u64,
        has_role: bool,
        now: u64,
    ) -> Result<u64, proto::EnrollmentRefusal> {
        let invalid = proto::EnrollmentRefusal::InvalidWindow;
        match (until, for_secs, has_role) {
            (0, 0, false) => Ok(0),
            (_, _, false) => Err(invalid),
            (0, 0, true) => Ok(now.saturating_add(self.for_secs)),
            (until, 0, true) if until > now && until - now <= self.max_secs => Ok(until),
            (0, secs, true) if secs <= self.max_secs => Ok(now.saturating_add(secs)),
            _ => Err(invalid),
        }
    }
}

/// The approval that used a record, as the ops file keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordUse {
    /// The request the coordinator approved by the record: its id, 64 hex digits.
    pub request_id: String,
    /// When it approved it, in unix seconds.
    pub at_unix: u64,
}

impl InventoryRecord {
    /// Where the record's window stands at `now`: used, else open while `now` is before its
    /// end, else closed.
    pub(super) fn state_at(&self, now: u64) -> proto::RecordState {
        if self.used.is_some() {
            proto::RecordState::Used
        } else if now < self.approve_until_unix {
            proto::RecordState::Open
        } else {
            proto::RecordState::Closed
        }
    }
}

/// Marks the record of `host_id` used by the approval of `request_id` at `at_unix`; a record
/// that is gone or already used is left as it is.
pub(crate) fn mark_used(
    records: &mut [InventoryRecord],
    host_id: &str,
    request_id: &str,
    at_unix: u64,
) {
    let free = records
        .iter_mut()
        .find(|record| record.host_id == host_id && record.used.is_none());
    if let Some(record) = free {
        record.used = Some(RecordUse {
            request_id: request_id.to_owned(),
            at_unix,
        });
    }
}

impl Coordinator {
    /// The limits of the windows of new records (`--record-approve-for`,
    /// `--record-approve-max`).
    #[must_use]
    pub fn record_window(&self) -> RecordWindow {
        self.ops.record_window()
    }

    /// `Enrollment.OpenRecord`: opens the window of the record of a host id again, or moves the
    /// end of one still open, from now, as `AddRecord` opens it; changes nothing else of the
    /// record. Under the ops file's lock, audited `record_open` with the end and the reason.
    /// The answer's record is as kept, `enrolled` when a host already enrolled matches it, and
    /// says what the enrollment store holds for its host id (`host_held`). Then a pending
    /// request that matches the record is approved by it, as at `AddRecord`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_host_id`, `invalid_request` (the reason),
    /// `no_record`, `invalid_window` (see [`RecordWindow::end`]; also a record that names no
    /// role), `record_used`, `store_failed` (also when the enrolled hosts, or what the store
    /// holds for the host id, cannot be read: nothing is written then), `audit_unavailable`.
    pub fn open_record(
        &self,
        caller: &Caller,
        request: proto::OpenRecordRequest,
    ) -> Result<proto::OpenRecordResponse, proto::EnrollmentRefusal> {
        use proto::EnrollmentRefusal::{InvalidHostId, InvalidWindow, NoRecord, RecordUsed};
        let by = caller.operator()?;
        let host_id = request
            .host_id
            .parse::<HostId>()
            .map_err(|_| InvalidHostId)?
            .to_string();
        let why = optional_reason(&request.reason, proto::EnrollmentRefusal::InvalidRequest)?;
        let hosts = self
            .enrolled_hosts()
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let held = self
            .host_held(&host_id)
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let window = self.ops.record_window();
        let now = self.ops.unix_now();
        let mut kept = None;
        let done = self
            .ops
            .decide_change(crate::ops::Write::BeforeMemory, |state| {
                let records = &mut state.inventory;
                let record = records
                    .iter_mut()
                    .find(|r| r.host_id == host_id)
                    .ok_or(NoRecord)?;
                if record.used.is_some() {
                    return Err(RecordUsed);
                }
                if record.role.is_empty() {
                    return Err(InvalidWindow);
                }
                let end = window.end(
                    request.approve_until_unix,
                    request.approve_for_secs,
                    true,
                    now,
                )?;
                record.approve_until_unix = end;
                let record = record.clone();
                let enrolled = RecordIndex::new(records)
                    .enrolled(&hosts)
                    .contains(&host_id);
                kept = Some((record, enrolled));
                let mut audit = AuditEntry::new(by, ACTION_RECORD_OPEN);
                audit.node_id = Some(host_id.clone());
                audit.reason = Some(match &why {
                    Some(why) => format!("until {end}; {why}"),
                    None => format!("until {end}"),
                });
                Ok(audit)
            })?;
        let (record, enrolled) = kept.ok_or(proto::EnrollmentRefusal::StoreFailed)?;
        let shown = record_proto(&record, enrolled, held, now);
        self.approve_waiting(&record.host_id);
        Ok(proto::OpenRecordResponse {
            record: Some(shown),
            entry: Some(entry_proto(&done)),
        })
    }

    /// Tries the pending requests that match the record of `host_id` against the records as
    /// they are now: the daemon posts its request once and then only polls, so a record entered
    /// after the machine booted approves it here. Without enrollment there is none to try.
    pub(super) fn approve_waiting(&self, host_id: &str) {
        let Some(enrollment) = self.enrollment() else {
            return;
        };
        let records = self.ops.read(|state| state.inventory.clone());
        enrollment.approve_matching(host_id, &records);
    }
}
