//! `Enrollment.UpdateRecord`: the part of [`crate::inventory`] that changes the data MACs, the BMC
//! MAC and the notes of a record and nothing else of it, so a record's role, host group, serial,
//! platform, window and use survive a change of what the operator typed. Removing a record and
//! adding it again would reset them (and give a used record a new window).

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use super::{RecordIndex, macs_of, record_proto, unreadable, valid_notes};
use crate::operator::{Caller, entry_proto};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The audit action of a record's MACs or notes changed.
pub const ACTION_RECORD_UPDATE: &str = "record_update";

impl Coordinator {
    /// `Enrollment.UpdateRecord`: sets the data MACs, the BMC MAC and the notes of the record of
    /// a host id to the ones given, checked as `AddRecord` checks them, and changes nothing else
    /// of the record. Under the ops file's lock, audited `record_update` with what the record
    /// then holds. The answer's record is as kept, `enrolled` when a host already enrolled
    /// matches it, and says what the enrollment store holds for its host id (`host_held`). Then
    /// a pending request that matches the record is approved by it, as at `AddRecord`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_host_id`, `invalid_request` (a MAC, the notes, more
    /// than `MAX_DATA_MACS` data MACs, a data MAC that is also the BMC MAC, or no data MAC on a
    /// record that holds no serial), `no_record`, `record_conflict` (a MAC another record holds),
    /// `store_failed` (also when the enrolled hosts, or what the store holds for the host id,
    /// cannot be read: nothing is written then), `audit_unavailable`.
    pub fn update_record(
        &self,
        caller: &Caller,
        request: proto::UpdateRecordRequest,
    ) -> Result<proto::UpdateRecordResponse, proto::EnrollmentRefusal> {
        use proto::EnrollmentRefusal::{InvalidHostId, InvalidRequest, NoRecord, RecordConflict};
        let by = caller.operator()?;
        let host_id = request
            .host_id
            .parse::<HostId>()
            .map_err(|_| InvalidHostId)?
            .to_string();
        let (data_macs, bmc_mac) = macs_of(&request.data_macs, &request.bmc_mac)?;
        let notes = match request.notes.trim() {
            "" => String::new(),
            text => valid_notes(text).ok_or(InvalidRequest)?,
        };
        let hosts = self
            .enrolled_hosts()
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let held = self
            .host_held(&host_id)
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let now = self.ops.unix_now();
        let mut kept = None;
        let done = self.ops.decide_change(Write::BeforeMemory, |state| {
            let records = &mut state.inventory;
            let at = records
                .iter()
                .position(|r| r.host_id == host_id)
                .ok_or(NoRecord)?;
            let mut record = records[at].clone();
            record.data_macs = data_macs.clone();
            record.bmc_mac = bmc_mac.clone();
            record.notes = notes.clone();
            if record.data_macs.is_empty() && record.serial.is_empty() {
                return Err(InvalidRequest);
            }
            let taken = records
                .iter()
                .filter(|r| r.host_id != host_id)
                .any(|r| r.macs().any(|held| record.macs().any(|new| new == held)));
            if taken {
                return Err(RecordConflict);
            }
            let mut audit = AuditEntry::new(by, ACTION_RECORD_UPDATE);
            audit.node_id = Some(host_id.clone());
            audit.reason = Some(record.describe());
            records[at] = record.clone();
            let enrolled = RecordIndex::new(records)
                .enrolled(&hosts)
                .contains(&host_id);
            kept = Some((record, enrolled));
            Ok(audit)
        })?;
        let (record, enrolled) = kept.ok_or(proto::EnrollmentRefusal::StoreFailed)?;
        let shown = record_proto(&record, enrolled, held, now, self.desired.in_group_mode());
        self.approve_waiting(&record.host_id);
        Ok(proto::UpdateRecordResponse {
            record: Some(shown),
            entry: Some(entry_proto(&done)),
        })
    }
}
