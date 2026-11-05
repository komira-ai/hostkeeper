//! The host inventory (design 5.7, **Host inventory**): the record an operator adds for each
//! server when building it, and the match of a pending enrollment request against the records.
//!
//! A record names a host id, an optional role, the server's data MACs, its BMC MAC, an optional
//! serial and notes. It lives in the ops file ([`crate::ops::OpsState::inventory`]) with the
//! other operator state, and every change to it is made under the ops file's lock with its
//! audit entry: `Enrollment.AddRecord` ([`Coordinator::add_record`]),
//! `Enrollment.ListRecords` ([`Coordinator::list_records`]) and `Enrollment.RemoveRecord`
//! ([`Coordinator::remove_record`]). No two records hold one host id, one MAC or one serial.
//!
//! `Enrollment.List` ([`Coordinator::list_pending`]) shows each pending request with its match
//! ([`match_claims`]): the one record it matches, none, or several (every candidate named, none
//! chosen). A match fills in a host id and a role for the operator to read; it approves nothing,
//! and an approval still names the request's key hash (design 5.7, step 3).

use std::cmp::Ordering;

use hostkeeper_enrollment::enroll::{Claims, RequestState, RequestView};
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::{HostId, RoleName};
use serde::{Deserialize, Serialize};

use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The most records the ops file keeps. Raise it here if a farm needs more.
pub const MAX_RECORDS: usize = 10_000;

/// The most data MACs one record holds.
pub const MAX_DATA_MACS: usize = 16;

/// The most records or requests one page answers: a `limit` of 0, or one above this, is this.
pub const MAX_PAGE: usize = 256;

/// The longest serial a record holds.
const MAX_SERIAL: usize = 64;

/// One server the operator recorded, as the ops file keeps it. The MACs are in their one
/// spelling ([`MacAddress`]'s), the data MACs sorted; an empty text is "none".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryRecord {
    /// The host id the server is meant to enroll as.
    pub host_id: String,
    /// The role it is meant for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// The MACs of its data interfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_macs: Vec<String>,
    /// The MAC of its BMC.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bmc_mac: String,
    /// Its serial number: a Mac's request is matched by it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub serial: String,
    /// Free text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The name of the operator token that added it.
    pub by: String,
    /// When it was added, in unix seconds.
    pub at_unix: u64,
}

impl InventoryRecord {
    /// Every MAC the record holds: its data MACs and its BMC MAC.
    fn macs(&self) -> impl Iterator<Item = &str> {
        self.data_macs
            .iter()
            .map(String::as_str)
            .chain(Some(self.bmc_mac.as_str()).filter(|mac| !mac.is_empty()))
    }

    /// Whether the record matches a request that claims `claims`: by serial for a Mac (the
    /// claimed Mac serial, letters compared without case); otherwise by one of its data MACs
    /// among the claimed addresses, with no claimed BMC MAC other than its own.
    fn matches(&self, claims: &Claims) -> bool {
        if !claims.mac_serial.is_empty() {
            return !self.serial.is_empty() && self.serial.eq_ignore_ascii_case(&claims.mac_serial);
        }
        let claimed = claims.data_macs();
        let shares_data_mac = self
            .data_macs
            .iter()
            .any(|mac| claimed.iter().any(|claim| claim.to_string() == *mac));
        let bmc_agrees = claims
            .bmc()
            .is_none_or(|bmc| bmc.to_string() == self.bmc_mac);
        shares_data_mac && bmc_agrees
    }

    /// A line for the audit: what the record holds, the notes left out.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.role.is_empty() {
            parts.push(format!("role {}", self.role));
        }
        if !self.data_macs.is_empty() {
            parts.push(format!("data {}", self.data_macs.join(" ")));
        }
        if !self.bmc_mac.is_empty() {
            parts.push(format!("bmc {}", self.bmc_mac));
        }
        if !self.serial.is_empty() {
            parts.push(format!("serial {}", self.serial));
        }
        parts.join("; ")
    }
}

/// How a pending request matches the records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InventoryMatch<'a> {
    /// Exactly one record matches.
    Matched(&'a InventoryRecord),
    /// No record matches.
    Unmatched,
    /// More than one record matches: their host ids, sorted. None is chosen.
    Ambiguous(Vec<String>),
}

/// The match of a request claiming `claims` against `records` (design 5.7, **The match**).
#[must_use]
pub fn match_claims<'a>(claims: &Claims, records: &'a [InventoryRecord]) -> InventoryMatch<'a> {
    let found: Vec<&InventoryRecord> = records.iter().filter(|r| r.matches(claims)).collect();
    match found.as_slice() {
        [] => InventoryMatch::Unmatched,
        [one] => InventoryMatch::Matched(one),
        many => {
            let mut ids: Vec<String> = many.iter().map(|r| r.host_id.clone()).collect();
            ids.sort();
            InventoryMatch::Ambiguous(ids)
        }
    }
}

/// `record` as `hostkeeper.operator.v1` says it.
fn record_proto(record: &InventoryRecord) -> proto::InventoryRecord {
    proto::InventoryRecord {
        host_id: record.host_id.clone(),
        role: record.role.clone(),
        data_macs: record.data_macs.clone(),
        bmc_mac: record.bmc_mac.clone(),
        serial: record.serial.clone(),
        notes: record.notes.clone(),
        added_by: record.by.clone(),
        added_unix: record.at_unix,
    }
}

/// `text` as a MAC in its one spelling.
fn mac(text: &str) -> Result<String, proto::EnrollmentRefusal> {
    MacAddress::parse(text)
        .map(|mac| mac.to_string())
        .map_err(|_| proto::EnrollmentRefusal::InvalidRequest)
}

/// Whether `text` is a serial a record may hold: 1 to 64 of `A-Za-z0-9._-`.
fn is_serial(text: &str) -> bool {
    (1..=MAX_SERIAL).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The record `request` asks for, by `by` at `at_unix`, checked field by field.
fn record_of(
    request: proto::AddRecordRequest,
    by: &str,
    at_unix: u64,
) -> Result<InventoryRecord, proto::EnrollmentRefusal> {
    use proto::EnrollmentRefusal::{InvalidHostId, InvalidRequest, InvalidRole};
    let host_id = request
        .host_id
        .parse::<HostId>()
        .map_err(|_| InvalidHostId)?
        .to_string();
    let role = match request.role.as_str() {
        "" => String::new(),
        text => text
            .parse::<RoleName>()
            .map_err(|_| InvalidRole)?
            .to_string(),
    };
    let mut data_macs = request
        .data_macs
        .iter()
        .map(|text| mac(text))
        .collect::<Result<Vec<String>, _>>()?;
    data_macs.sort();
    data_macs.dedup();
    let bmc_mac = match request.bmc_mac.as_str() {
        "" => String::new(),
        text => mac(text)?,
    };
    let serial = request.serial.trim().to_owned();
    let notes = match request.notes.trim() {
        "" => String::new(),
        text => valid_reason(text).ok_or(InvalidRequest)?,
    };
    let identified = !data_macs.is_empty() || !serial.is_empty();
    let bounded = data_macs.len() <= MAX_DATA_MACS && data_macs.iter().all(|m| *m != bmc_mac);
    if !identified || !bounded || !(serial.is_empty() || is_serial(&serial)) {
        return Err(InvalidRequest);
    }
    Ok(InventoryRecord {
        host_id,
        role,
        data_macs,
        bmc_mac,
        serial,
        notes,
        by: by.to_owned(),
        at_unix,
    })
}

/// The page `limit` asks for: [`MAX_PAGE`] for 0 or for a limit above it.
fn page_size(limit: u32) -> usize {
    match usize::try_from(limit) {
        Ok(0) | Err(_) => MAX_PAGE,
        Ok(limit) => limit.min(MAX_PAGE),
    }
}

/// The page token of the request `view`: its first-seen time and its id, so the next page goes
/// on after it whatever was approved in between.
fn pending_token(view: &RequestView) -> String {
    format!("{}.{}", view.posted_unix, view.request_id)
}

/// The order of the pending view: oldest first, then by id.
fn pending_order(a: &RequestView, b: &RequestView) -> Ordering {
    (a.posted_unix, a.request_id).cmp(&(b.posted_unix, b.request_id))
}

/// The page token `token` as the place it names, checked: `<unix seconds>.<64 hex digits>`.
fn read_pending_token(token: &str) -> Option<(u64, String)> {
    let (unix, id) = token.split_once('.')?;
    let unix = unix.parse::<u64>().ok()?;
    let id_ok = id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    id_ok.then(|| (unix, id.to_owned()))
}

/// `claims` as the agent's protocol says them.
fn claims_proto(claims: &Claims) -> agent::Claims {
    agent::Claims {
        machine_id: claims.machine_id.clone(),
        dmi_serial: claims.dmi_serial.clone(),
        product_uuid: claims.dmi_product_uuid.clone(),
        addresses: claims.addresses.clone(),
        mac_serial: claims.mac_serial.clone(),
        platform_uuid: claims.mac_platform_uuid.clone(),
        bmc_mac: claims.bmc_mac.clone(),
    }
}

/// The pending request `view` as `Enrollment.List` shows it, matched against `records`.
fn pending_proto(view: &RequestView, records: &[InventoryRecord]) -> proto::PendingEnrollment {
    let mut pending = proto::PendingEnrollment {
        request_id: view.request_id.to_hex(),
        key_sha256: view.request_id.to_hex(),
        claims: Some(claims_proto(&view.claims)),
        image: view.image.clone(),
        first_seen_unix: view.posted_unix,
        renewal_of: view
            .renewal_of
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        duplicate_claim: view.state == RequestState::DuplicateClaim,
        ..proto::PendingEnrollment::default()
    };
    match match_claims(&view.claims, records) {
        InventoryMatch::Matched(record) => {
            pending.set_inventory_match(proto::InventoryMatch::Matched);
            pending.record_host_id.clone_from(&record.host_id);
            pending.record_role.clone_from(&record.role);
        }
        InventoryMatch::Unmatched => pending.set_inventory_match(proto::InventoryMatch::Unmatched),
        InventoryMatch::Ambiguous(ids) => {
            pending.set_inventory_match(proto::InventoryMatch::Ambiguous);
            pending.candidate_host_ids = ids;
        }
    }
    pending
}

impl Coordinator {
    /// `Enrollment.AddRecord`: records a server the operator built, before it first boots.
    /// Under the ops file's lock: a record for the host id is refused, and so is a MAC or a
    /// serial another record holds, and a record past [`MAX_RECORDS`].
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_host_id`, `invalid_role`, `invalid_request` (a
    /// MAC, the serial, the notes, more than [`MAX_DATA_MACS`] data MACs, a data MAC that is
    /// also the BMC MAC, or neither a data MAC nor a serial), `record_exists`,
    /// `record_conflict`, `capacity`, `store_failed`.
    pub fn add_record(
        &self,
        caller: &Caller,
        request: proto::AddRecordRequest,
    ) -> Result<proto::AddRecordResponse, proto::EnrollmentRefusal> {
        let by = caller.operator()?;
        let record = record_of(request, by, self.ops.unix_now())?;
        let mut audit = AuditEntry::new(by, "record_add");
        audit.node_id = Some(record.host_id.clone());
        audit.reason = Some(record.describe());
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let records = &mut state.inventory;
            if records.iter().any(|r| r.host_id == record.host_id) {
                return Err(proto::EnrollmentRefusal::RecordExists);
            }
            let mac_taken = records
                .iter()
                .any(|r| r.macs().any(|held| record.macs().any(|new| new == held)));
            let serial_taken = !record.serial.is_empty()
                && records
                    .iter()
                    .any(|r| r.serial.eq_ignore_ascii_case(&record.serial));
            if mac_taken || serial_taken {
                return Err(proto::EnrollmentRefusal::RecordConflict);
            }
            if records.len() >= MAX_RECORDS {
                return Err(proto::EnrollmentRefusal::Capacity);
            }
            let at = records.partition_point(|r| r.host_id < record.host_id);
            records.insert(at, record.clone());
            Ok(())
        })?;
        Ok(proto::AddRecordResponse {
            record: Some(record_proto(&record)),
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Enrollment.ListRecords`: the records by host id, after the host id `page_token` names,
    /// at most `limit` ([`MAX_PAGE`] for 0 or above it). Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no host id).
    pub fn list_records(
        &self,
        caller: &Caller,
        request: proto::ListRecordsRequest,
    ) -> Result<proto::ListRecordsResponse, proto::EnrollmentRefusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(
                token
                    .parse::<HostId>()
                    .map_err(|_| proto::EnrollmentRefusal::InvalidRequest)?
                    .to_string(),
            ),
        };
        let size = page_size(request.limit);
        let (records, more) = self.ops.read(|state| {
            let mut rest = state
                .inventory
                .iter()
                .filter(|r| after.as_ref().is_none_or(|after| r.host_id > *after));
            let page: Vec<proto::InventoryRecord> =
                rest.by_ref().take(size).map(record_proto).collect();
            (page, rest.next().is_some())
        });
        let next_page_token = match records.last() {
            Some(last) if more => last.host_id.clone(),
            _ => String::new(),
        };
        Ok(proto::ListRecordsResponse {
            records,
            next_page_token,
        })
    }

    /// `Enrollment.RemoveRecord`: removes the record of a host id. A host enrolled under it
    /// keeps its certificate; a pending request it matched shows unmatched.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_host_id`, `invalid_request` (the reason),
    /// `no_record`, `store_failed`.
    pub fn remove_record(
        &self,
        caller: &Caller,
        request: proto::RemoveRecordRequest,
    ) -> Result<proto::RemoveRecordResponse, proto::EnrollmentRefusal> {
        let by = caller.operator()?;
        let host_id = request
            .host_id
            .parse::<HostId>()
            .map_err(|_| proto::EnrollmentRefusal::InvalidHostId)?
            .to_string();
        let why = valid_reason(&request.reason).ok_or(proto::EnrollmentRefusal::InvalidRequest)?;
        let mut audit = AuditEntry::new(by, "record_remove");
        audit.node_id = Some(host_id.clone());
        audit.reason = Some(why);
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let before = state.inventory.len();
            state.inventory.retain(|r| r.host_id != host_id);
            if state.inventory.len() == before {
                return Err(proto::EnrollmentRefusal::NoRecord);
            }
            Ok(())
        })?;
        Ok(proto::RemoveRecordResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Enrollment.List`: the requests waiting for an operator, duplicate claims included,
    /// oldest first, after the request `page_token` names, at most `limit` ([`MAX_PAGE`] for 0
    /// or above it), each with its match against the records. A coordinator without enrollment has
    /// none. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token this call did not give),
    /// `store_failed` (the enrollment store could not be read).
    pub fn list_pending(
        &self,
        caller: &Caller,
        request: proto::ListPendingRequest,
    ) -> Result<proto::ListPendingResponse, proto::EnrollmentRefusal> {
        caller.operator()?;
        let after = match request.page_token.as_str() {
            "" => None,
            token => {
                Some(read_pending_token(token).ok_or(proto::EnrollmentRefusal::InvalidRequest)?)
            }
        };
        let Some(enrollment) = self.enrollment() else {
            return Ok(proto::ListPendingResponse::default());
        };
        let mut pending = enrollment.store().pending().map_err(|why| {
            tracing::error!(%why, "the enrollment store could not be read");
            proto::EnrollmentRefusal::StoreFailed
        })?;
        pending.sort_by(pending_order);
        let size = page_size(request.limit);
        let mut rest = pending.iter().filter(|view| {
            after.as_ref().is_none_or(|(unix, id)| {
                (view.posted_unix, view.request_id.to_hex()) > (*unix, id.clone())
            })
        });
        let page: Vec<&RequestView> = rest.by_ref().take(size).collect();
        let more = rest.next().is_some();
        let records = self.ops.read(|state| state.inventory.clone());
        let next_page_token = match page.last() {
            Some(last) if more => pending_token(last),
            _ => String::new(),
        };
        Ok(proto::ListPendingResponse {
            requests: page
                .iter()
                .map(|view| pending_proto(view, &records))
                .collect(),
            next_page_token,
        })
    }
}
