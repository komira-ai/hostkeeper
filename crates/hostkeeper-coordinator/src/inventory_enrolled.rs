//! The record the coordinator writes for a host enrolled without one: the part of
//! [`crate::inventory`] that keeps every enrolled host in the inventory.
//!
//! A host approved by an operator's click with no record that matches it (a Mac that came through
//! the bootstrap package, whose serial no record holds; a server approved before its record was
//! entered) is given one, from what it claimed when it enrolled ([`claimed_record`]): its host id;
//! a Mac's serial; the data MACs among its claimed addresses that are universally administered
//! unicast MACs (a bridge's or a virtual machine's made-up MAC may repeat on another host), at most
//! [`MAX_DATA_MACS`]; and its claimed BMC MAC, so made. The record is added by
//! [`COORDINATOR`], the name no operator token may take, which is how it reads as created at
//! enrollment (`added_by`), and its notes say so and, for a Mac (a record that holds a serial),
//! its platform. It names no host
//! group nor role, so it has no approval window and approves nothing: the host was approved
//! already, and a second key that claims the same serial or MAC waits for an operator, as a
//! request that matches a closed record does. Its platform, which a record states only for a
//! Linux server's network install, is left unstated.
//!
//! It is written right after the approval ([`Coordinator::record_approved`]) and, for the hosts
//! enrolled before this, at the coordinator's start ([`Coordinator::record_unrecorded`]). Both
//! are one change of the ops file per host, audited `record_add` by [`COORDINATOR`], whose edit
//! reads the records as the change applies them to: a host some record already matches (one, or
//! several), a host id a record holds, a MAC or a serial another record holds, or a full
//! inventory writes nothing and is logged. So the start is idempotent, and several coordinators
//! on one database write each record once. A host whose claims hold neither a Mac serial nor such
//! a MAC (one enrolled by a release before claims), or a Mac whose claimed serial is not one a
//! record may hold, gets none; the Inventory page lists it, without a record.
//!
//! A host the layout places as itself (`host:<id>`) whose host id a record holds has its place
//! moved to the record (`inventory:<id>`), audited `layout_place`: the lab view joins a host with
//! a record by the record's key ([`Coordinator::place_with_record`]). A host left without a record
//! keeps its place.

use hostkeeper_enrollment::enroll::{Claims, EnrolledHost};
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{InventoryRecord, MAX_DATA_MACS, RecordIndex, is_serial, refused_beside};
use crate::audit_actions::about;
use crate::enrollment::token_of;
use crate::layout::{Kind, Subject};
use crate::operator_layout::LAYOUT_PLACE;
use crate::ops::{AuditEntry, OpsError, Write};
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;

/// The notes of a record written from a host's claims.
const NOTES: &str = "Recorded at enrollment from the host's claims.";

/// The notes of a Mac's record written from its claims: its platform with them.
const MAC_NOTES: &str = "Recorded at enrollment from the host's claims: a Mac (darwin-arm64).";

/// When a record is written for an enrolled host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum When {
    /// Right after an operator approved it.
    Approved,
    /// At the coordinator's start, for a host enrolled before.
    Start,
}

impl When {
    /// The start of the audit reason.
    fn reason(self) -> &'static str {
        match self {
            When::Approved => "at enrollment, from the host's claims",
            When::Start => "at the coordinator's start, for a host enrolled without one",
        }
    }
}

/// Why no record was written for an enrolled host.
#[derive(Debug)]
enum Unwritten {
    /// A record matches the host already (one, or several).
    Matched,
    /// The record the claims give is refused beside the records: one holds the host id, a MAC or
    /// the serial (`record_exists`, `record_conflict`), or the inventory is full (`capacity`).
    Refused(proto::EnrollmentRefusal),
    /// The ops store did not take the change.
    Store(OpsError),
}

impl From<OpsError> for Unwritten {
    fn from(why: OpsError) -> Self {
        Unwritten::Store(why)
    }
}

/// The record of the host `host_id` from what it claimed, `claims`, at `at_unix`, as the module
/// says; `None` when the claims hold neither a Mac serial nor a data MAC that may be recorded, or
/// a Mac serial a record may not hold (1 to 64 of `A-Za-z0-9._-`).
fn claimed_record(host_id: &str, claims: &Claims, at_unix: u64) -> Option<InventoryRecord> {
    let bmc = claims.bmc().filter(MacAddress::is_universal_unicast);
    let data_macs: Vec<String> = claims
        .data_macs()
        .into_iter()
        .filter(|mac| mac.is_universal_unicast() && Some(mac) != bmc.as_ref())
        .take(MAX_DATA_MACS)
        .map(|mac| mac.to_string())
        .collect();
    // A Mac is matched by its serial alone, as claimed: one no record may hold would give a
    // record the Mac never matches.
    let serial = claims.mac_serial.as_str();
    if (!serial.is_empty() && !is_serial(serial)) || (data_macs.is_empty() && serial.is_empty()) {
        return None;
    }
    Some(InventoryRecord {
        host_id: host_id.to_owned(),
        role: String::new(),
        host_group: String::new(),
        data_macs,
        bmc_mac: bmc.map(|mac| mac.to_string()).unwrap_or_default(),
        serial: serial.to_owned(),
        notes: if serial.is_empty() { NOTES } else { MAC_NOTES }.to_owned(),
        by: COORDINATOR.to_owned(),
        at_unix,
        approve_until_unix: 0,
        used: None,
        platform: None,
    })
}

impl Coordinator {
    /// Writes the record of `host_id`, which an operator just approved, from its claims, when no
    /// record matches it (the module says how): whether one was written. Without enrollment, or
    /// for a host the store does not hold as enrolled, nothing is written.
    pub(crate) fn record_approved(&self, host_id: &str) -> bool {
        match self.enrolled_host(host_id) {
            // None: denied since, or not enrolled.
            Ok(host) => host.is_some_and(|host| self.record_enrolled(&host, When::Approved)),
            Err(why) => {
                tracing::error!(%why, %host_id, "cannot read an approved host: no record is written for it");
                false
            }
        }
    }

    /// At the coordinator's start: writes a record for every enrolled host no record matches,
    /// from its claims, and moves the place of each it wrote to the record
    /// ([`Self::place_with_record`]). The number written; none when the enrolled hosts cannot
    /// be read (logged).
    pub fn record_unrecorded(&self) -> usize {
        let hosts = match self.enrolled_hosts() {
            Ok(hosts) => hosts,
            Err(why) => {
                tracing::error!(%why, "cannot read the enrolled hosts: none is recorded at this start");
                return 0;
            }
        };
        let unmatched: Vec<&EnrolledHost> = self.ops.read(|state| {
            let index = RecordIndex::new(&state.inventory);
            hosts
                .iter()
                .filter(|host| index.matching(&host.claims).is_empty())
                .collect()
        });
        let mut written = 0;
        for host in unmatched {
            if self.record_enrolled(host, When::Start) {
                self.place_with_record(host.host_id.as_str());
                written += 1;
            }
        }
        if written > 0 {
            tracing::info!(
                written,
                "recorded the enrolled hosts no inventory record matched"
            );
        }
        written
    }

    /// Writes the record of the enrolled `host` from its claims, `when` says, as one audited
    /// change of the ops file whose edit checks the records it is applied to: whether it was.
    fn record_enrolled(&self, host: &EnrolledHost, when: When) -> bool {
        let host_id = host.host_id.as_str();
        let Some(record) = claimed_record(host_id, &host.claims, self.ops.unix_now()) else {
            tracing::warn!(%host_id, "an enrolled host claimed nothing a record holds: it has none");
            return false;
        };
        let mut audit = AuditEntry::new(COORDINATOR, "record_add");
        audit.node_id = Some(host_id.to_owned());
        audit.reason = Some(format!("{}; {}", when.reason(), record.describe()));
        let written = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let records = &mut state.inventory;
            if !RecordIndex::new(records).matching(&host.claims).is_empty() {
                return Err(Unwritten::Matched);
            }
            if let Some(refusal) = refused_beside(records, &record) {
                return Err(Unwritten::Refused(refusal));
            }
            let at = records.partition_point(|r| r.host_id < record.host_id);
            records.insert(at, record.clone());
            Ok(())
        });
        match written {
            Ok(_) => {
                tracing::info!(%host_id, "recorded an enrolled host from its claims");
                true
            }
            Err(Unwritten::Matched) => false,
            Err(Unwritten::Refused(refusal)) => {
                let refusal = token_of(refusal);
                tracing::warn!(%refusal, %host_id, "no record was written for an enrolled host: the records refuse it");
                false
            }
            Err(Unwritten::Store(why)) => {
                tracing::warn!(%why, %host_id, "no record was written for an enrolled host: the ops store did not take it");
                false
            }
        }
    }

    /// Moves the place of the host `host_id` (`host:<id>`) to its record (`inventory:<id>`), when
    /// a record holds the host id (the lab view takes it as the host's) and the layout places the
    /// host and not the record, audited `layout_place` by [`COORDINATOR`]; otherwise changes
    /// nothing. A move the layout or the audit cannot take is logged: place the server again from
    /// the lab view.
    pub(crate) fn place_with_record(&self, host_id: &str) {
        let recorded = self
            .ops
            .read(|state| state.inventory.iter().any(|r| r.host_id == host_id));
        if !recorded {
            return;
        }
        let from = Subject {
            kind: Kind::Host,
            id: host_id.to_owned(),
        };
        let to = Subject {
            kind: Kind::Inventory,
            id: host_id.to_owned(),
        };
        let moved = self.layout().change(|held| {
            held.with_place_moved(&from, &to).map(|moved| match moved {
                Some(next) => (next, true),
                None => (held.clone(), false),
            })
        });
        match moved {
            Ok(false) => {}
            Ok(true) => {
                let mut entry = about(COORDINATOR, LAYOUT_PLACE, &to.key());
                entry.reason = Some(format!("moved from {} to its record", from.key()));
                if let Err(why) = self.ops.change(Write::BeforeMemory, entry, |_| {}) {
                    tracing::error!(%why, "the audit line of a place moved to its record was not written");
                }
            }
            Err(why) => {
                tracing::error!(%why, %host_id, "a host's place was not moved to its record: place it from the lab view");
            }
        }
    }
}
