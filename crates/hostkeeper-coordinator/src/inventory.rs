//! The host inventory (design 5.7, **Host inventory**): the record an operator adds for each
//! server when building it, and the match of a pending enrollment request against the records.
//!
//! A record names a host id, an optional role, the server's data MACs, its BMC MAC, an optional
//! serial and notes. It lives in the ops file ([`crate::ops::OpsState::inventory`]) with the
//! other operator state, and every change to it is made under the ops file's lock with its
//! audit entry: `Enrollment.AddRecord` ([`Coordinator::add_record`]),
//! `Enrollment.ListRecords` ([`Coordinator::list_records`]), `Enrollment.RemoveRecord`
//! ([`Coordinator::remove_record`]) and `Enrollment.OpenRecord` ([`Coordinator::open_record`]).
//! No two records hold one host id, one MAC or one serial.
//!
//! A record that names a role is the approval of the host it was entered for: it has a window
//! (`approve_until_unix`) and, once the coordinator approved a request by it, a mark that it is
//! used ([`RecordUse`]), and while its window is open and it is unused a pending request that
//! matches it exactly is approved by the coordinator itself
//! ([`crate::enrollment::Enrollment::approve_by_record`]). The window, its limits, the state
//! it shows (open, closed, used) and `OpenRecord` are in `inventory_window.rs`. Exactly is
//! [`InventoryRecord::matches_exactly`]: [`match_claims`] finds the one record, and the claims
//! then agree with every MAC the record holds, its BMC MAC included.
//!
//! `Enrollment.List` ([`Coordinator::enrollment_list`]) shows each pending request with its
//! match ([`show_match`], [`match_claims`]): the one record it matches, none, or several (every
//! candidate named, none chosen). A match fills in a host id, a role, the record's serial and
//! BMC MAC and where its window stands, for the operator to read: a request that is still
//! pending against an open record waits for an operator for another reason. Two pending
//! requests that match one record contest it ([`contests`]): the list names, on each, the
//! others, neither is approved by the record, and `Enrollment.Approve` of any of them is
//! refused `record_contested` until all but one are rejected. `Enrollment.Approve`
//! matches the request again ([`match_claims`]): a request (not a renewal) that matches exactly
//! one record is approved as that record's host id, which an empty host id defaults to, and
//! another host id is `record_mismatch` unless the approval replaces. An approval an operator
//! makes does not mark a record used; the enrollment store's one host per host id keeps the
//! record from approving a second key.
//!
//! An enrolled host ([`EnrolledHost`]: it holds an approval and is not denied) is matched the
//! same way, by the claims of the request its key was approved from ([`RecordIndex`], which
//! answers what [`match_claims`] answers, looking only at the records that share a data MAC or
//! the serial with the claims): `Hosts.List` and `Hosts.Get` show the record it matches
//! ([`show_inventory`]), and `Enrollment.ListRecords` marks each record a host is enrolled
//! against (`enrolled`), and lists only those, or only the others, when asked.
//!
//! Apart from the match, each record says what the enrollment store holds for its host id
//! (`host_held`, [`Coordinator::host_held`]): no host, an approved host, or a denied or retired
//! one. It is read from the store by host id, so it holds after a restart and for a host never
//! heard from, and for a denied or retired host, which `Hosts.List` does not show when it is not heard.
//!
//! A record may also state the platform its server is installed as by network boot
//! ([`InventoryRecord::platform`]: `linux-x86_64` or `linux-arm64`), which `hostkeeper-pxe` needs
//! in router mode (network-install design, section 3). It is kept with the rest of the record in
//! the operations state, but the ops file, whose reader in the release before refuses a key it
//! does not know, never holds it: the files keep it in the halts file beside it
//! ([`crate::ops_file`]).

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_enrollment::enroll::{Claims, EnrolledHost, HostHeld, RequestView, StoreError};
use hostkeeper_enrollment::names::{KeyHash, MacAddress};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::{HostId, Platform, RoleName};
use serde::{Deserialize, Serialize};

pub(crate) use window::mark_used;
pub use window::{
    ACTION_RECORD_OPEN, DEFAULT_APPROVE_FOR_SECS, DEFAULT_APPROVE_MAX_SECS, RecordUse,
    RecordWindow, WindowAboveMax,
};

use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The most records the ops file keeps. Raise it here if a farm needs more.
pub const MAX_RECORDS: usize = 10_000;

/// The most data MACs one record holds.
pub const MAX_DATA_MACS: usize = 16;

/// The most records one page answers: a `limit` of 0, or one above this, is this.
pub const MAX_PAGE: usize = 256;

/// The longest serial a record holds.
const MAX_SERIAL: usize = 64;

/// The approval window of a record, and `Enrollment.OpenRecord`.
#[path = "inventory_window.rs"]
mod window;

/// Whether `n` is zero: a window a record does not have is left out of the ops file.
fn is_zero(n: &u64) -> bool {
    *n == 0
}

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
    /// The host group it is meant for; empty when it names none. Written only when it names one,
    /// so a file whose records name none keeps the shape the release before reads.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host_group: String,
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
    /// The end of the record's approval window, in unix seconds: a request that matches the
    /// record exactly is approved by the coordinator while the time is before it. 0 for a
    /// record with no window (one that names no role, or one a release before windows added).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub approve_until_unix: u64,
    /// The approval that used the record, once the coordinator approved a request by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<RecordUse>,
    /// The platform to install it as by network boot: a Linux platform, or `None` when the
    /// record states none. Never in the ops file: the files keep it in the halts file
    /// ([`crate::ops_file`]).
    #[serde(skip)]
    pub platform: Option<Platform>,
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
    /// among the claimed addresses, with no claimed BMC MAC other than its own (a record with no BMC MAC accepts any).
    fn matches(&self, claims: &Claims) -> bool {
        if !claims.mac_serial.is_empty() {
            return !self.serial.is_empty() && self.serial.eq_ignore_ascii_case(&claims.mac_serial);
        }
        let claimed = claims.data_macs();
        let shares_data_mac = self
            .data_macs
            .iter()
            .any(|mac| claimed.iter().any(|claim| claim.to_string() == *mac));
        // A field the record leaves out does not constrain the match.
        let bmc_agrees = self.bmc_mac.is_empty()
            || claims
                .bmc()
                .is_none_or(|bmc| bmc.to_string() == self.bmc_mac);
        shares_data_mac && bmc_agrees
    }

    /// Whether the record matches a request that claims `claims` exactly, which is what the
    /// coordinator approves by: a Mac by its serial ([`Self::matches`]); otherwise by one of the
    /// data MACs ([`Self::matches`]) and, when the record holds a BMC MAC, the claim of that
    /// BMC MAC (a request that claims none does not match a record that holds one, which
    /// [`Self::matches`] accepts).
    pub(crate) fn matches_exactly(&self, claims: &Claims) -> bool {
        let bmc_claimed = self.bmc_mac.is_empty()
            || claims
                .bmc()
                .is_some_and(|bmc| bmc.to_string() == self.bmc_mac);
        self.matches(claims) && (!claims.mac_serial.is_empty() || bmc_claimed)
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
        if self.approve_until_unix > 0 {
            parts.push(format!("approve until {}", self.approve_until_unix));
        }
        if let Some(platform) = self.platform {
            parts.push(format!("platform {platform}"));
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

/// The contested requests among `pending` (network-install design section 7): for each request
/// that matches a record another of them matches, the ids of those others, sorted. A request
/// that shares no matching record with another is left out. Records are matched as
/// [`match_claims`] matches them, so a request that matches several records contests the
/// requests of each.
#[must_use]
pub fn contests(
    pending: &[RequestView],
    records: &[InventoryRecord],
) -> BTreeMap<KeyHash, BTreeSet<KeyHash>> {
    let index = RecordIndex::new(records);
    let mut by_record: BTreeMap<usize, BTreeSet<KeyHash>> = BTreeMap::new();
    for view in pending {
        for at in index.matching(&view.claims) {
            by_record.entry(at).or_default().insert(view.request_id);
        }
    }
    let mut contested: BTreeMap<KeyHash, BTreeSet<KeyHash>> = BTreeMap::new();
    for ids in by_record.values().filter(|ids| ids.len() > 1) {
        for id in ids {
            contested
                .entry(*id)
                .or_default()
                .extend(ids.iter().filter(|other| *other != id));
        }
    }
    contested
}

/// The records, by the data MACs and the serials that pick them out.
pub struct RecordIndex<'a> {
    records: &'a [InventoryRecord],
    /// `mac <MAC>` and `serial <serial, lower case>`: the records that hold it.
    by_key: BTreeMap<String, Vec<usize>>,
}

fn mac_key(mac: &str) -> String {
    format!("mac {mac}")
}

fn serial_key(serial: &str) -> String {
    format!("serial {}", serial.to_ascii_lowercase())
}

impl<'a> RecordIndex<'a> {
    /// The index of `records`.
    #[must_use]
    pub fn new(records: &'a [InventoryRecord]) -> Self {
        let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (at, record) in records.iter().enumerate() {
            let serial = Some(&record.serial).filter(|serial| !serial.is_empty());
            let keys = record
                .data_macs
                .iter()
                .map(|mac| mac_key(mac))
                .chain(serial.map(|serial| serial_key(serial)));
            for key in keys {
                by_key.entry(key).or_default().push(at);
            }
        }
        Self { records, by_key }
    }

    /// Every record a request claiming `claims` matches, as [`match_claims`] matches them, as
    /// their places in the records, in order. A record that matches holds the claimed Mac
    /// serial or one of the claimed data MACs, so only those records are compared.
    #[must_use]
    pub fn matching(&self, claims: &Claims) -> Vec<usize> {
        let keys: Vec<String> = if claims.mac_serial.is_empty() {
            claims
                .data_macs()
                .iter()
                .map(|mac| mac_key(&mac.to_string()))
                .collect()
        } else {
            vec![serial_key(&claims.mac_serial)]
        };
        let candidates: BTreeSet<usize> = keys
            .iter()
            .filter_map(|key| self.by_key.get(key))
            .flatten()
            .copied()
            .collect();
        candidates
            .into_iter()
            .filter(|&at| self.records.get(at).is_some_and(|r| r.matches(claims)))
            .collect()
    }

    /// The one record a request claiming `claims` matches, as [`match_claims`] matches it;
    /// `None` when none or several do.
    #[must_use]
    pub fn matched(&self, claims: &Claims) -> Option<&'a InventoryRecord> {
        match self.matching(claims).as_slice() {
            [one] => self.records.get(*one),
            _ => None,
        }
    }

    /// The host ids of the records one of `hosts` is enrolled against.
    #[must_use]
    pub fn enrolled(&self, hosts: &[EnrolledHost]) -> BTreeSet<String> {
        hosts
            .iter()
            .filter_map(|host| self.matched(&host.claims))
            .map(|record| record.host_id.clone())
            .collect()
    }
}

/// Fills in what the host inventory says of `host`, enrolled as `enrolled` (`None` when it is
/// not): when it was approved, when its first heartbeat since then was accepted, when its first
/// heartbeat that reported a managed service serving was, and the record its claims match, if
/// one does.
pub fn show_inventory(
    host: &mut proto::Host,
    enrolled: Option<&EnrolledHost>,
    index: &RecordIndex<'_>,
) {
    let Some(enrolled) = enrolled else {
        return;
    };
    host.enrolled_unix = enrolled.approved_unix;
    host.first_heartbeat_unix = enrolled.first_heartbeat_unix;
    host.first_serving_unix = enrolled.first_serving_unix;
    if let Some(record) = index.matched(&enrolled.claims) {
        host.inventory_record_id.clone_from(&record.host_id);
        host.data_macs.clone_from(&record.data_macs);
        host.bmc_mac.clone_from(&record.bmc_mac);
        host.serial.clone_from(&record.serial);
    }
}

/// What to answer, and log, when the enrolled hosts cannot be read.
pub(crate) fn unreadable<E>(refusal: E) -> impl FnOnce(StoreError) -> E {
    move |why| {
        tracing::error!(%why, "cannot read the enrolled hosts");
        refusal
    }
}

/// What the store holds for a host id, as `hostkeeper.operator.v1` says it.
fn held_proto(held: HostHeld) -> proto::HostHeld {
    match held {
        HostHeld::None => proto::HostHeld::None,
        HostHeld::Approved => proto::HostHeld::Approved,
        HostHeld::Denied => proto::HostHeld::Denied,
        HostHeld::Retired => proto::HostHeld::Retired,
    }
}

/// `record` as `hostkeeper.operator.v1` says it at `now`; `enrolled` when a host is enrolled
/// against it, and `held` what the enrollment store holds for its host id.
fn record_proto(
    record: &InventoryRecord,
    enrolled: bool,
    held: proto::HostHeld,
    now: u64,
) -> proto::InventoryRecord {
    proto::InventoryRecord {
        host_id: record.host_id.clone(),
        role: record.role.clone(),
        data_macs: record.data_macs.clone(),
        bmc_mac: record.bmc_mac.clone(),
        serial: record.serial.clone(),
        notes: record.notes.clone(),
        added_by: record.by.clone(),
        added_unix: record.at_unix,
        enrolled,
        host_held: held.into(),
        approve_until_unix: record.approve_until_unix,
        state: record.state_at(now).into(),
        used_by_request_id: record
            .used
            .as_ref()
            .map(|used| used.request_id.clone())
            .unwrap_or_default(),
        used_unix: record.used.as_ref().map_or(0, |used| used.at_unix),
        platform: record
            .platform
            .map_or(agent::Platform::Unspecified, platform_proto)
            .into(),
        host_group: String::new(),
    }
}

/// A Linux platform as `hostkeeper.agent.v1` says it.
pub(crate) fn platform_proto(platform: Platform) -> agent::Platform {
    match platform {
        Platform::LinuxX86_64 => agent::Platform::LinuxX8664,
        Platform::LinuxArm64 => agent::Platform::LinuxArm64,
        Platform::DarwinArm64 => agent::Platform::DarwinArm64,
    }
}

/// The platform a record may state, from `AddRecordRequest.platform`'s number: `None` for
/// `PLATFORM_UNSPECIFIED`, a Linux platform for its value; any other (a Mac's platform, which
/// is not installed by network boot, or a number this release does not know) is refused.
fn record_platform(number: i32) -> Result<Option<Platform>, proto::EnrollmentRefusal> {
    match agent::Platform::try_from(number) {
        Ok(agent::Platform::Unspecified) => Ok(None),
        Ok(agent::Platform::LinuxX8664) => Ok(Some(Platform::LinuxX86_64)),
        Ok(agent::Platform::LinuxArm64) => Ok(Some(Platform::LinuxArm64)),
        Ok(agent::Platform::DarwinArm64) | Err(_) => Err(proto::EnrollmentRefusal::InvalidRequest),
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

/// The record `request` asks for, by `by` at `at_unix`, checked field by field, with the window
/// `window` gives it.
fn record_of(
    request: proto::AddRecordRequest,
    by: &str,
    at_unix: u64,
    window: &RecordWindow,
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
    let platform = record_platform(request.platform)?;
    let notes = match request.notes.trim() {
        "" => String::new(),
        text => valid_reason(text).ok_or(InvalidRequest)?,
    };
    let identified = !data_macs.is_empty() || !serial.is_empty();
    let bounded = data_macs.len() <= MAX_DATA_MACS && data_macs.iter().all(|m| *m != bmc_mac);
    if !identified || !bounded || !(serial.is_empty() || is_serial(&serial)) {
        return Err(InvalidRequest);
    }
    let approve_until_unix = window.end(
        request.approve_until_unix,
        request.approve_for_secs,
        !role.is_empty(),
        at_unix,
    )?;
    Ok(InventoryRecord {
        host_id,
        role,
        host_group: String::new(),
        data_macs,
        bmc_mac,
        serial,
        notes,
        by: by.to_owned(),
        at_unix,
        approve_until_unix,
        used: None,
        platform,
    })
}

/// The store's spelling of what a pending request claims, as `Enrollment.List` carries it.
fn claims_of(claims: &agent::Claims) -> Claims {
    Claims {
        machine_id: claims.machine_id.clone(),
        dmi_serial: claims.dmi_serial.clone(),
        dmi_product_uuid: claims.product_uuid.clone(),
        addresses: claims.addresses.clone(),
        mac_serial: claims.mac_serial.clone(),
        mac_platform_uuid: claims.platform_uuid.clone(),
        bmc_mac: claims.bmc_mac.clone(),
    }
}

/// Fills in how the pending request `pending` matches `records`: the one record's host id,
/// role, serial, BMC MAC and where its window stands at `now`, every candidate of an ambiguous
/// match, or unmatched.
pub fn show_match(pending: &mut proto::PendingEnrollment, records: &[InventoryRecord], now: u64) {
    let claims = claims_of(&pending.claims.clone().unwrap_or_default());
    match match_claims(&claims, records) {
        InventoryMatch::Matched(record) => {
            pending.set_inventory_match(proto::InventoryMatch::Matched);
            pending.record_host_id.clone_from(&record.host_id);
            pending.record_role.clone_from(&record.role);
            pending.record_serial.clone_from(&record.serial);
            pending.record_bmc_mac.clone_from(&record.bmc_mac);
            pending.set_record_state(record.state_at(now));
            pending.record_approve_until_unix = record.approve_until_unix;
        }
        InventoryMatch::Unmatched => pending.set_inventory_match(proto::InventoryMatch::Unmatched),
        InventoryMatch::Ambiguous(ids) => {
            pending.set_inventory_match(proto::InventoryMatch::Ambiguous);
            pending.candidate_host_ids = ids;
        }
    }
}

/// The page `limit` asks for: [`MAX_PAGE`] for 0 or for a limit above it.
fn page_size(limit: u32) -> usize {
    match usize::try_from(limit) {
        Ok(0) | Err(_) => MAX_PAGE,
        Ok(limit) => limit.min(MAX_PAGE),
    }
}

impl Coordinator {
    /// Every enrolled host ([`hostkeeper_enrollment::enroll::EnrollmentStore::enrolled_hosts`]);
    /// none without enrollment.
    pub(crate) fn enrolled_hosts(&self) -> Result<Vec<EnrolledHost>, StoreError> {
        self.enrollment()
            .map_or_else(|| Ok(Vec::new()), |e| e.store().enrolled_hosts())
    }

    /// The host `node_id` as enrolled
    /// ([`hostkeeper_enrollment::enroll::EnrollmentStore::enrolled_host`]); `None` without
    /// enrollment, or for a host not enrolled.
    pub(crate) fn enrolled_host(&self, node_id: &str) -> Result<Option<EnrolledHost>, StoreError> {
        let Some(enrollment) = self.enrollment() else {
            return Ok(None);
        };
        hostkeeper_enrollment::names::HostId::parse(node_id)
            .ok()
            .map(|host| enrollment.store().enrolled_host(&host))
            .transpose()
            .map(Option::flatten)
    }

    /// What the enrollment store holds for the host id `host_id`
    /// ([`hostkeeper_enrollment::enroll::EnrollmentStore::held`]): `UNSPECIFIED` without
    /// enrollment, which has no store to read, and `NONE` for an id the store cannot hold.
    ///
    /// # Errors
    /// [`StoreError`] when the host id's deny or host file cannot be read.
    pub(crate) fn host_held(&self, host_id: &str) -> Result<proto::HostHeld, StoreError> {
        let Some(enrollment) = self.enrollment() else {
            return Ok(proto::HostHeld::Unspecified);
        };
        let held = hostkeeper_enrollment::names::HostId::parse(host_id)
            .ok()
            .map(|host| enrollment.store().held(&host))
            .transpose()?
            .unwrap_or(HostHeld::None);
        Ok(held_proto(held))
    }

    /// `Enrollment.AddRecord`: records a server the operator built, before it first boots.
    /// Under the ops file's lock: a record for the host id is refused, and so is a MAC or a
    /// serial another record holds, and a record past [`MAX_RECORDS`]. A record that names a
    /// role is added with its approval window ([`RecordWindow::end`]), and the pending requests
    /// that match it are tried once it is kept ([`Coordinator::approve_waiting`]). The answer's
    /// record is as added, `enrolled` when a host already enrolled matches it, and no other
    /// record, and says what the enrollment store holds for its host id (`host_held`).
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_host_id`, `invalid_role`, `invalid_request` (a
    /// MAC, the serial, the notes, more than [`MAX_DATA_MACS`] data MACs, a data MAC that is
    /// also the BMC MAC, neither a data MAC nor a serial, or a platform that is not a Linux
    /// one), `invalid_window`,
    /// `record_exists`, `record_conflict`, `capacity`, `store_failed` (also when the enrolled
    /// hosts, or what the store holds for the host id, cannot be read: nothing is written
    /// then).
    pub fn add_record(
        &self,
        caller: &Caller,
        request: proto::AddRecordRequest,
    ) -> Result<proto::AddRecordResponse, proto::EnrollmentRefusal> {
        let by = caller.operator()?;
        let now = self.ops.unix_now();
        let record = record_of(request, by, now, &self.ops.record_window())?;
        let hosts = self
            .enrolled_hosts()
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let held = self
            .host_held(&record.host_id)
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let mut audit = AuditEntry::new(by, "record_add");
        audit.node_id = Some(record.host_id.clone());
        audit.reason = Some(record.describe());
        let mut enrolled = false;
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
            enrolled = RecordIndex::new(records)
                .enrolled(&hosts)
                .contains(&record.host_id);
            Ok(())
        })?;
        let shown = record_proto(&record, enrolled, held, now);
        self.approve_waiting(&record.host_id);
        Ok(proto::AddRecordResponse {
            record: Some(shown),
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Enrollment.ListRecords`: the records by host id, after the host id `page_token` names,
    /// at most `limit` ([`MAX_PAGE`] for 0 or above it), each `enrolled` when a host is
    /// enrolled against it; with `enrolled` set, only the records whose `enrolled` is that.
    /// Each record of the page says what the enrollment store holds for its host id
    /// (`host_held`, [`Coordinator::host_held`]), read after the page is chosen, and where its
    /// approval window stands at the time of the call (`state`). Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a page token that is no host id),
    /// `store_failed` (the enrolled hosts, or what the store holds for a host id of the page,
    /// cannot be read).
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
        let hosts = self
            .enrolled_hosts()
            .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
        let now = self.ops.unix_now();
        let (mut records, more) = self.ops.read(|state| {
            let enrolled = RecordIndex::new(&state.inventory).enrolled(&hosts);
            let mut rest = state
                .inventory
                .iter()
                .filter(|r| after.as_ref().is_none_or(|after| r.host_id > *after))
                .map(|r| {
                    record_proto(
                        r,
                        enrolled.contains(&r.host_id),
                        proto::HostHeld::Unspecified,
                        now,
                    )
                })
                .filter(|r| request.enrolled.is_none_or(|want| r.enrolled == want));
            let page: Vec<proto::InventoryRecord> = rest.by_ref().take(size).collect();
            (page, rest.next().is_some())
        });
        for record in &mut records {
            let held = self
                .host_held(&record.host_id)
                .map_err(unreadable(proto::EnrollmentRefusal::StoreFailed))?;
            record.set_host_held(held);
        }
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
}
