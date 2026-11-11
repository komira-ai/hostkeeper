//! The ops store: what operators and the coordinator itself decide, kept apart from CI's
//! desired state (`docs/design/host-updates.md`, sections 3.8 and 5).
//!
//! It holds the hosts an operator excluded from a service, the labels an operator put on a
//! host, the operator's holds ([`crate::holds`]) and assignments ([`crate::assignment`]: which
//! roles and hosts run each service, their minimum versions, the hosts ordered to uninstall
//! it), the host inventory ([`crate::inventory`]), the per-service marks later changes fill (a
//! halt, the last good version, the bad versions, the force that stands: [`crate::force`]), where
//! each service's update plan stands (the stages entered, the hosts done, an operator's pause:
//! [`crate::rollouts`]; kept in the halts file beside this one, not in this file), the marks of
//! each (host group, package) (`crate::host_groups`, all of them in this file), and a ring of
//! the last changes made. A key this build added is written only when it holds
//! something, so a file that never held one keeps the shape the release before reads; once it
//! does, the release before refuses the file, as it should: it would ignore a hold, a force or an
//! assignment.
//! Over a file it is one JSON file, `--ops-file`, kept apart from `desired.json` so that an
//! earlier coordinator, which refuses a key it does not know there, still reads the desired
//! state. The file is created by the first change (a
//! farm that never uses it keeps its files as they are) and replaced whole after that, like
//! the desired state: a temporary file, fsync, rename and a directory fsync. What the halt
//! release added to a halt and its acknowledgements is kept in a second file beside it, so that
//! the release before still reads this one ([`crate::ops_file`]).
//!
//! The reader is strict. A key it does not know is a refusal to start, and so is a `schema`
//! newer than this build: a coordinator rolled back below the build that wrote a halt would
//! otherwise ignore the halt and go on rolling. The operator moves the file aside on
//! purpose.
//!
//! The store keeps its state in memory and commits each change to a backend ([`crate::store`]):
//! over a file, the ops file and the files beside it, written as above; without one, an
//! in-memory copy. A change is its edit on a copy of the state, under the writers' lock, handed to
//! the backend with the state before it and its audit lines, in one commit.
//!
//! A change reaches storage and memory in one of two orders ([`Write`]). A change that *raises*
//! protection (a halt, a bad mark, a pause) and an operator's `abort` ([`OpsStore::escape`]) are
//! in memory first and committed after, so a failed commit never leaves them unraised. Every
//! other change, a clearing one included, is committed first and enters memory only once
//! storage holds it, so a failed commit never leaves it cleared.
//!
//! A memory-first change whose commit fails is **pending** ([`OpsStore::halt_unwritten`]): it
//! stands in memory, the coordinator commits it again at every heartbeat
//! ([`OpsStore::retry_unwritten`]), and the next storage-first change commits it first and is
//! refused while it cannot, so no change is committed ahead of an earlier one. Before its commit
//! is tried the store writes a marker listing the audit number of every pending change
//! (`<ops-file>.halts.unfinished` for a store over a file), and removes it once nothing is
//! pending. A start that finds the marker removes it when the ops file holds every change it
//! lists, and is refused otherwise, naming the changes that did not land
//! ([`crate::config::settle_unfinished`]); the operator makes them again by hand if they
//! still matter, and removes the marker.
//!
//! Every change carries an [`AuditEntry`]: the store numbers it, keeps the last
//! [`AUDIT_RING`] in the state and logs it on the target `hostkeeper_coordinator::audit`, and
//! commits it with the change as one line of the audit (over a file, one JSON line of
//! `<ops-file>.audit`, [`crate::audit_file`]), which keeps every change; the ring is what
//! `/v1/status` shows. A change is numbered one past the newest number in memory or in storage
//! ([`crate::store::Backend::last_seq`]): over a backend that commits the lines with the change
//! (the in-memory one), a refused change takes no number; over a file, a change refused after its
//! lines were appended keeps their numbers, so no number is held twice.
//!
//! The audit is the durable record: a change and its lines are committed together. Over a file
//! the lines are appended first, before the ops file is written, and a storage-first change whose
//! line cannot be appended (a full disk) writes nothing of the state and is refused with
//! [`OpsError::AuditUnavailable`]. A memory-first one stands and is answered as made: its state is
//! committed without the lines, as a raise always was, and the lines stay pending until they can
//! be appended. An audit line can still name a change whose ops-file write then failed, or, for a
//! change of several entries (a force), the entries appended before the one that failed; such a
//! line is never appended again.
//!
//! The caps on the counts (`MAX_HOSTS`, `MAX_EXCLUDED`) are checked under the same lock as the
//! write ([`OpsStore::try_change`]), on the state the change is applied to.
//!
//! The operator actions ([`operate`]) write exclusions, labels and the clear of a halt; the
//! coordinator itself writes the raising of a halt ([`crate::halt`]) and, in
//! [`crate::reconcile`], the last good version and the marking of a version bad (a raise, so in
//! memory first), and the entering of a plan's next stage ([`crate::rollouts`]); the operator's
//! `Rollouts.Advance`, `Pause` and `Resume` write the plan's position (a pause is a raise, so in
//! memory first); CI's set of a new desired version lifts a halt the updater raised
//! ([`crate::halt::lift_by_ci`]), and never a bad mark. The decision reads a halt and the bad
//! versions ([`crate::halt::decide`]) and the position of a plan ([`crate::stages::View::of`]);
//! nothing reads the rest when it decides what a host is told.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::UpdateResult;
use serde::{Deserialize, Serialize};

use crate::assignment::ServiceAssignments;
#[cfg(doc)]
use crate::audit_file::AuditLog;
use crate::audit_file::{
    AuditLine, DEFAULT_ROTATION, Logged, Query, Rotation, UnixClock, wall_clock,
};
use crate::desired::{Staged, StoreError};
use crate::facts::Generations;
use crate::force::ForceRecord;
use crate::holds::HoldMark;
use crate::host_groups::GroupsMarks;
use crate::inventory::{InventoryRecord, RecordWindow};
use crate::ops_file;
use crate::rollouts::Rollout;
use crate::store::{BackendError, Commit, Fake, Files, Shared, commit_across};

pub use crate::operate::OperateError;

pub(crate) use crate::operate::label;

/// The schema of the file this build writes and the newest it reads.
pub const SCHEMA: u32 = 1;

/// How many changes the file remembers.
pub const AUDIT_RING: usize = 64;

/// The most hosts with labels the file holds. Raise it here if a farm needs more.
pub const MAX_HOSTS: usize = 4096;

/// The most exclusions the file holds. Raise it here if a farm needs more.
pub const MAX_EXCLUDED: usize = 4096;

/// The most labels one host has.
pub const MAX_LABELS: usize = 8;

/// The most bytes a label's key or value, in `[a-z0-9-]`, has.
pub const MAX_LABEL_BYTES: usize = 32;

/// The most bytes of a reason.
pub const MAX_REASON_BYTES: usize = 256;

/// The labels of one host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostOps {
    /// Label key to value, set by an operator only.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// A host an operator took out of a service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    /// The service.
    pub service: String,
    /// The node id.
    pub node_id: String,
    /// Why, in the operator's words.
    pub reason: String,
}

/// Who raised a halt, which says what lifts it ([`crate::halt`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltOrigin {
    /// A hook's check (its exit 76): only the operator's `clear_halt` lifts it. A halt read
    /// from a file that says nothing is this one, the safe side.
    #[default]
    Hook,
    /// The updater itself (a staged release whose digest changed, `artifact_changed`): CI's
    /// set of a desired version other than the halt's target lifts it too, since a fix-forward
    /// release is the remedy.
    Updater,
    /// An operator (`Releases.Halt`): only `clear_halt`, or a force, lifts it. A release that
    /// does not know it reads it as [`HaltOrigin::Hook`], which is lifted the same way.
    Operator,
}

impl HaltOrigin {
    /// Whether it is [`HaltOrigin::Hook`], which is not written.
    #[must_use]
    pub fn is_hook(&self) -> bool {
        *self == Self::Hook
    }
}

/// A halt of a service: raised by a host's report ([`crate::halt`]), cleared by the operator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Halt {
    /// The host that reported it.
    pub node_id: String,
    /// The check that failed.
    pub check: String,
    /// The version it was updating to.
    pub target: Version,
    /// The `last_update` it reported, whole: what an acknowledgement of it matches. `None` in
    /// a file written before reports were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<UpdateResult>,
    /// Who raised it. Kept in the halts file beside the ops file ([`crate::ops_file`]), so the
    /// ops file keeps the shape the release before reads; written only when it is not
    /// [`HaltOrigin::Hook`].
    #[serde(default, skip_serializing_if = "HaltOrigin::is_hook")]
    pub origin: HaltOrigin,
}

/// A host and the halt report an operator cleared. It matches that report, whole: the same
/// host reporting another `last_update`, of the same target too, is a different report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaltAck {
    /// The host.
    pub node_id: String,
    /// The version.
    pub target: Version,
    /// The `last_update` the host reported. `None` in a file written before reports were kept:
    /// such an acknowledgement matches no report, and ends on the host's first beat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<UpdateResult>,
}

impl HaltAck {
    /// Whether this acknowledges `report` of the host `node`: the same host and the very same
    /// `last_update`.
    #[must_use]
    pub fn matches(&self, node: &str, report: &UpdateResult) -> bool {
        self.node_id == node && self.report.as_ref() == Some(report)
    }
}

impl From<Halt> for HaltAck {
    fn from(halt: Halt) -> Self {
        Self {
            node_id: halt.node_id,
            target: halt.target,
            report: halt.report,
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A failure report an operator override acknowledged: the host and the whole report it made,
/// so that a different report of the same host is not hidden by it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureAck {
    /// The host.
    pub node_id: String,
    /// The `last_update` it reported.
    pub report: UpdateResult,
}

/// The marks of one service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceOps {
    /// The standing halt, if any.
    #[serde(default)]
    pub halt: Option<Halt>,
    /// The halt reports an operator cleared: one per host, the latest.
    #[serde(default)]
    pub halt_acked: Vec<HaltAck>,
    /// The other halt reports received while the standing halt stood: one per host, the
    /// latest. A clear acknowledges them with the halt's own. Empty unless a halt stands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub halt_seen: Vec<HaltAck>,
    /// How many halts an operator has cleared. Agents that announced `halt-v1` are told it
    /// and lift their hold when it is newer than the one they halted under. Written only
    /// once it is not zero, so a file of a service never cleared reads as it always did.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub halt_generation: u64,
    /// The newest version that converged and baked, as the coordinator recorded it; the
    /// version an automatic rollback goes back to.
    #[serde(default)]
    pub last_good: Option<Version>,
    /// The versions never to order again. CI cannot set one: only the operator's unmark or a
    /// force lifts a mark.
    #[serde(default)]
    pub bad: Vec<Version>,
    /// The failure reports standing when a bad version was lifted by hand: one per host. A
    /// report that is acked, whole, does not count towards an automatic rollback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_acked: Vec<FailureAck>,
    /// The force that stands ([`crate::force`]); written only while one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<ForceRecord>,
    /// How many times the service was forced; written only once it is not zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub force_generation: u64,
    /// Of `bad`, the versions a force left; written only when there are any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bad_by_force: Vec<Version>,
    /// The repository of the registry the service is installed from, as
    /// `Services.SetRepository` set it; none for the default ([`crate::services::DEFAULT_REPOSITORY`]).
    /// Written only while one is set, so a file of a service that names none reads as it
    /// always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

/// One change, as the ring and the log say it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    /// Its number: one more than the entry before it, for the life of the file.
    pub seq: u64,
    /// Who made it: the name of the operator's token (`operator` for `--operator-token-file`),
    /// `coordinator` for the coordinator's own changes, `ci` for CI's, `mdm-nonce` for a Mac's
    /// approval by its nonce, `mdm-gate` for the MDM gate's.
    pub by: String,
    /// What it was (`exclude`, `exclude_abandon`, `include`, `set_labels`, `clear_halt`, `halt`,
    /// `mark_bad`, `unmark_bad`, `abort`, `record_cleared`, `hold`, `unhold`, `assign`, `unassign`,
    /// `set_min_version`, `set_repository`, `force` with the `unmark_bad`, `mark_bad` and `clear_halt` it made; the
    /// coordinator's own: `last_good`, `clear_last_good`, `mark_bad`, `uninstalled` and
    /// `uninstall_cancelled`; the update plans': `rollout_advance` (the coordinator's, when a plan
    /// enters a stage by itself, or an operator's `Rollouts.Advance`), `rollout_pause` and
    /// `rollout_resume` (an operator's); the host groups': `host_group_put`, `host_group_remove` and
    /// `host_move` (an operator's `HostGroups` calls); CI's: `lift_halt`, `force_ended`, and `unmark_bad` in a
    /// file an earlier build wrote; enrollment's: `enroll_approve`, `enroll_replace`,
    /// `enroll_approve_refused`, `mac_auto_approve_declined`, and the coordinator's
    /// `enroll_requested`, `enroll_issued`, `cert_renewed` and `request_expired`; the MDM gate's
    /// `mdm_nonce_pushed` and `mdm_install_sent`, by `mdm-gate`; `state_imported`, by
    /// `coordinator`, the last row of an import into the database).
    pub action: String,
    /// The service it concerned, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The node it concerned, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The host group it concerned, if any ([`crate::host_groups`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_group: Option<String>,
    /// The operator's reason, if one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The user or API token the action concerned, if any (`password_reset`, `user_create`,
    /// `api_token_create` and the rest of [`crate::audit_actions`]); `by` is who did it. Kept in
    /// memory and in the audit file, never in the ops file's ring (`skip`): the release before
    /// refuses a key it does not know there (B8).
    #[serde(skip)]
    pub subject: Option<String>,
    /// The peer address a login came from, if the entry records one. Kept like `subject`: not in
    /// the ops file's ring.
    #[serde(skip)]
    pub source: Option<String>,
}

impl AuditEntry {
    /// An entry by `by` for `action`; the store numbers it.
    #[must_use]
    pub fn new(by: &str, action: &str) -> Self {
        Self {
            seq: 0,
            by: by.to_owned(),
            action: action.to_owned(),
            service: None,
            node_id: None,
            host_group: None,
            reason: None,
            subject: None,
            source: None,
        }
    }
}

/// Everything the file holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpsState {
    schema: u32,
    /// Per node id.
    #[serde(default)]
    pub hosts: BTreeMap<String, HostOps>,
    /// The hosts taken out of a service.
    #[serde(default)]
    pub excluded: Vec<Exclusion>,
    /// Per service name.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceOps>,
    /// The last [`AUDIT_RING`] changes, oldest first.
    #[serde(default)]
    pub audit: Vec<AuditEntry>,
    /// The operator's holds ([`crate::holds`]); written only when there are any, so a file
    /// with none keeps the shape the release before reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<HoldMark>,
    /// The operator's assignments of each service, by service name ([`crate::assignment`]):
    /// role and host entries, minimum versions and uninstall intents. Written only when there
    /// are any, so a file with none keeps the shape the release before reads.
    #[serde(default, skip_serializing_if = "crate::assignment::none")]
    pub assignments: BTreeMap<String, ServiceAssignments>,
    /// The host inventory ([`crate::inventory`]), by host id; written only when there is a
    /// record, so a file with none keeps the shape the release before reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inventory: Vec<InventoryRecord>,
    /// Where each service's update plan stands, by service name ([`crate::rollouts`]): kept in
    /// the halts file beside this one ([`crate::ops_file`]), never in this file.
    #[serde(skip)]
    pub rollouts: BTreeMap<String, Rollout>,
    /// The marks of each (host group, package) (`crate::host_groups`): its halt, plan position
    /// and every other mark in this file, written only when there are any.
    #[serde(default, skip_serializing_if = "GroupsMarks::is_empty")]
    pub(crate) host_groups: GroupsMarks,
}

impl Default for OpsState {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            hosts: BTreeMap::new(),
            excluded: Vec::new(),
            services: BTreeMap::new(),
            audit: Vec::new(),
            holds: Vec::new(),
            assignments: BTreeMap::new(),
            inventory: Vec::new(),
            rollouts: BTreeMap::new(),
            host_groups: GroupsMarks::default(),
        }
    }
}

impl OpsState {
    /// Nothing has been changed: the state of a coordinator with no ops file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The state with every entry at its default left out (a host with no labels, a service's
    /// marks, assignments and plan position that hold nothing): the code reads a missing entry as
    /// its default, so it reads the two the same, and this is the one a backend that stores no
    /// such entry gives back.
    #[must_use]
    pub(crate) fn without_defaults(mut self) -> Self {
        self.hosts.retain(|_, host| *host != HostOps::default());
        self.services
            .retain(|_, marks| *marks != ServiceOps::default());
        self.assignments
            .retain(|_, entries| *entries != ServiceAssignments::default());
        self.rollouts
            .retain(|_, rollout| *rollout != Rollout::default());
        self.host_groups = self.host_groups.without_defaults();
        self
    }
}

/// Why the ops file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// A file operation failed.
    #[error(transparent)]
    Io(#[from] StoreError),
    /// The file is not an ops file this build accepts.
    #[error("{path}: {why}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        why: String,
    },
    /// The audit line of a change could not be appended to the audit file (a full disk, a
    /// file that cannot be opened): the change is not made. The operator's refusal token is
    /// `audit_unavailable`.
    #[error("the audit file could not be appended to, so the change is not made: {0}")]
    AuditUnavailable(#[source] io::Error),
    /// The file was written by a newer build.
    #[error(
        "{path}: schema {found} is newer than the {SCHEMA} this build reads; starting would \
         ignore what it holds (a halt, an exclusion), so move it aside to start without it"
    )]
    Newer {
        /// The file.
        path: PathBuf,
        /// Its schema.
        found: u64,
    },
}

/// Which order a change reaches storage and memory in.
#[derive(Clone, Copy, Debug)]
pub enum Write {
    /// In memory first, committed after: for a change that raises protection (a bad mark, a
    /// pause). A failed commit leaves it raised and pending: it is committed again at every
    /// heartbeat ([`OpsStore::retry_unwritten`]) and before the next change, under the marker.
    AfterMemory,
    /// [`Write::AfterMemory`] for a halt, in the same order and under the same marker.
    Halt,
    /// Committed first, in memory only if storage holds it: for every other change, a clearing
    /// one included. A failed commit leaves the old state.
    BeforeMemory,
}

/// The order of one change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    StorageFirst,
    MemoryFirst,
}

impl Order {
    fn of(write: Write) -> Self {
        match write {
            Write::AfterMemory | Write::Halt => Self::MemoryFirst,
            Write::BeforeMemory => Self::StorageFirst,
        }
    }
}

/// A change [`OpsStore::interleave`] makes at the start of the next change.
#[cfg(test)]
type Interleaved = Box<dyn FnOnce(&mut OpsState) + Send>;

impl From<BackendError> for OpsError {
    fn from(why: BackendError) -> Self {
        match why {
            BackendError::Ops(why) => why,
            other => Self::Io(other.into_store()),
        }
    }
}

/// The ops state, in memory and in its backend ([`crate::store`]).
pub struct OpsStore {
    current: Mutex<OpsState>,
    /// Held across a whole change, so that writers wait for each other and not readers:
    /// `current` is only locked to read or to swap, never across the backend's commit.
    writing: Mutex<()>,
    backend: Shared,
    /// The generation of each host's facts ([`crate::facts`]).
    generations: Generations,
    /// Seconds since the epoch, for the lines of the audit.
    unix: UnixClock,
    /// How long the approval window of a new inventory record is, and the longest it may be.
    record_window: RecordWindow,
    /// The marker of changes in memory and not yet committed: `<ops-file>.halts.unfinished` for a
    /// store over a file; the others have none.
    marker: Option<PathBuf>,
    /// The changes in memory and not yet committed.
    pending: Mutex<pending::Pending>,
    /// Told when the store is deposed: another coordinator led its database while its link was
    /// lost, and the coordinator stops ([`OpsStore::depose`]).
    deposed: tokio::sync::Notify,
    /// The epoch the deposing coordinator held, once deposed.
    deposed_by: std::sync::OnceLock<u64>,
    /// The file and rotation of a store over a file, which a test hook opens again.
    #[cfg(test)]
    files: Option<(PathBuf, Rotation)>,
    /// A change another writer makes between a caller's read and its change, applied once at
    /// the start of the next change, under the writers' lock: what a race would do there.
    #[cfg(test)]
    interleave: Mutex<Option<Interleaved>>,
}

/// How often a failing retry of a pending change is logged at most.
pub const UNWRITTEN_LOG_EVERY: Duration = Duration::from_secs(60);

/// The state the ops file at `path`, whose bytes are `bytes`, holds.
///
/// # Errors
/// [`OpsError`]: not JSON this build reads, or a `schema` other than [`SCHEMA`].
pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<OpsState, OpsError> {
    let invalid = |why: String| OpsError::Invalid {
        path: path.to_owned(),
        why,
    };
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    let newer = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .filter(|&n| n > u64::from(SCHEMA));
    if let Some(found) = newer {
        return Err(OpsError::Newer {
            path: path.to_owned(),
            found,
        });
    }
    let state: OpsState = serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?;
    if state.schema != SCHEMA {
        return Err(invalid(format!(
            "schema {} is not the {SCHEMA} this build reads",
            state.schema
        )));
    }
    state.host_groups.check().map_err(invalid)?;
    Ok(state)
}

impl OpsStore {
    /// The store of the file at `path`: its state if it exists, empty if not. Nothing is
    /// created until the first change.
    ///
    /// The halts file beside it ([`crate::ops_file`]) is read and joined to it, and every
    /// change writes both.
    ///
    /// # Errors
    /// [`OpsError`]: the file exists and cannot be read, holds a key this build does not
    /// know, or has another schema; or a halts file a change staged cannot be settled, or the
    /// halts file exists and cannot be read or parsed, or the audit file beside it
    /// ([`crate::audit_file`]) exists and cannot be read.
    pub fn open(path: &Path) -> Result<Self, OpsError> {
        Self::open_with(path, DEFAULT_ROTATION)
    }

    /// [`Self::open`], with the audit file rotated by `rotation`.
    ///
    /// # Errors
    /// [`OpsError`], as [`Self::open`].
    pub(crate) fn open_with(path: &Path, rotation: Rotation) -> Result<Self, OpsError> {
        let files = Files::default().with_ops(path, rotation)?;
        let state = files.load_ops()?;
        let store = Self::over(Arc::new(files), state).with_marker(ops_file::unfinished_path(path));
        #[cfg(test)]
        let store = Self {
            files: Some((path.to_owned(), rotation)),
            ..store
        };
        Ok(store)
    }

    /// A store over `state`, as loaded from `backend`, to which every change is committed.
    pub(crate) fn over(backend: Shared, state: OpsState) -> Self {
        Self {
            current: Mutex::new(state),
            writing: Mutex::new(()),
            generations: Generations::from(backend.last_seq()),
            backend,
            unix: wall_clock(),
            record_window: RecordWindow::default(),
            marker: None,
            pending: Mutex::new(pending::Pending::default()),
            deposed: tokio::sync::Notify::new(),
            deposed_by: std::sync::OnceLock::new(),
            #[cfg(test)]
            files: None,
            #[cfg(test)]
            interleave: Mutex::new(None),
        }
    }

    /// The generation of the facts of the host `node_id` ([`crate::facts`]).
    pub(crate) fn facts_generation(&self, node_id: &str) -> u64 {
        self.generations.of(node_id)
    }

    /// The store with `marker` as the file it lists its pending changes in.
    #[must_use]
    pub(crate) fn with_marker(mut self, marker: PathBuf) -> Self {
        self.marker = Some(marker);
        self
    }

    /// The store with `record_window` as the limits of the windows of new inventory records
    /// (`--record-approve-for`, `--record-approve-max`).
    #[must_use]
    pub fn with_record_window(mut self, record_window: RecordWindow) -> Self {
        self.record_window = record_window;
        self
    }

    /// The limits of the windows of new inventory records.
    #[must_use]
    pub fn record_window(&self) -> RecordWindow {
        self.record_window
    }

    /// A store that keeps nothing durable: changes stand in memory only (an in-memory backend,
    /// [`crate::store::Fake`]). For a coordinator built without a file.
    #[must_use]
    pub fn volatile() -> Self {
        Self::over(Arc::new(Fake::new()), OpsState::default())
    }

    /// The state now.
    #[must_use]
    pub fn state(&self) -> OpsState {
        self.lock().clone()
    }

    /// What `read` makes of the state now, without copying it: for a decision that looks at
    /// a little of it on every heartbeat.
    pub fn read<T>(&self, read: impl FnOnce(&OpsState) -> T) -> T {
        read(&self.lock())
    }

    /// The marks of `service` now: none if it has none.
    #[must_use]
    pub fn marks(&self, service: &str) -> ServiceOps {
        self.lock()
            .services
            .get(service)
            .cloned()
            .unwrap_or_default()
    }

    fn lock(&self) -> MutexGuard<'_, OpsState> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes `edit` to the state, numbers `entry`, appends it to the ring, logs it, and
    /// reaches storage and memory in the order `write` says. Returns the entry as numbered.
    ///
    /// # Errors
    /// [`OpsError::Io`]: storage did not take the change. After [`Write::BeforeMemory`] the
    /// old state stands; after [`Write::AfterMemory`] the change stands in memory, pending.
    pub fn change(
        &self,
        write: Write,
        entry: AuditEntry,
        edit: impl FnOnce(&mut OpsState),
    ) -> Result<AuditEntry, OpsError> {
        self.try_change(write, entry, |state| {
            edit(state);
            Ok::<(), OpsError>(())
        })
    }

    /// [`Self::change`] with an edit that may refuse. The edit runs under the lock that
    /// serialises writes, on the state the change is applied to, so a limit it checks (a cap
    /// on a count) holds however many callers there are. When it refuses, its error is
    /// returned and nothing else happens: the state, the ring and its numbering and storage
    /// are as they were, and nothing is logged.
    ///
    /// # Errors
    /// The edit's own error, or [`OpsError`] as for [`Self::change`].
    pub fn try_change<E: From<OpsError>>(
        &self,
        write: Write,
        entry: AuditEntry,
        edit: impl FnOnce(&mut OpsState) -> Result<(), E>,
    ) -> Result<AuditEntry, E> {
        self.decide_change(write, |state| edit(state).map(|()| entry))
    }

    /// [`Self::try_change`] with an edit that also says what the change is: it reads the state
    /// under the lock that serialises writes and answers the entry to audit, so that a change
    /// whose kind or subject depends on the state (raising a halt or keeping a report of one,
    /// clearing the halt that stands) is decided on the state it is applied to, not on one
    /// read before another writer changed it.
    ///
    /// # Errors
    /// The edit's own error, or [`OpsError`] as for [`Self::change`].
    pub(crate) fn decide_change<E: From<OpsError>>(
        &self,
        write: Write,
        edit: impl FnOnce(&mut OpsState) -> Result<AuditEntry, E>,
    ) -> Result<AuditEntry, E> {
        let mut entries =
            self.decide_changes(write, |state| edit(state).map(|entry| vec![entry]))?;
        Ok(entries.swap_remove(0))
    }

    /// [`Self::decide_change`] for a change that is several entries, numbered and audited in
    /// order, in one commit. The edit answers at least one entry.
    ///
    /// # Errors
    /// The edit's own error, or [`OpsError`] as for [`Self::change`].
    pub(crate) fn decide_changes<E: From<OpsError>>(
        &self,
        write: Write,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        self.audited_changes(Order::of(write), None, edit)
    }

    /// [`Self::decide_changes`], storage first, committed with the desired state `staged` in one
    /// commit ([`crate::store::commit_across`]): a force, whose desired state and entries land
    /// together or not at all. The desired state enters memory with the change.
    ///
    /// # Errors
    /// The edit's own error, or [`OpsError`] as for [`Self::change`]: nothing changed.
    pub(crate) fn decide_changes_with(
        &self,
        staged: Staged<'_>,
        edit: impl FnOnce(&mut OpsState) -> Vec<AuditEntry>,
    ) -> Result<Vec<AuditEntry>, OpsError> {
        self.audited_changes(Order::StorageFirst, Some(staged), |state| {
            Ok::<_, OpsError>(edit(state))
        })
    }

    /// The operator's `abort` (`Hosts.Abort`): a change of nothing but the audit, in memory
    /// first, that stands whether or not it is committed: the operator's way out when storage
    /// fails. A commit that fails leaves it pending, as a halt's does, and is logged.
    pub(crate) fn escape(&self, entry: AuditEntry) -> AuditEntry {
        let Ok((mut entries, _committed)) = self.held_first(|_| Ok::<_, Infallible>(vec![entry]));
        entries.swap_remove(0)
    }

    /// [`Self::decide_changes`] in the order `order`, the desired state `staged` committed with
    /// it when there is one.
    fn audited_changes<E: From<OpsError>>(
        &self,
        order: Order,
        staged: Option<Staged<'_>>,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        if order == Order::StorageFirst {
            return self.stored_first(staged, edit);
        }
        let (entries, committed) = self.held_first(edit)?;
        match committed {
            // A raise stands when only its audit line failed, as it always has; the line is
            // committed with it later.
            Ok(()) | Err(OpsError::AuditUnavailable(_)) => Ok(entries),
            Err(why) => Err(why.into()),
        }
    }

    /// A storage-first change: what is pending committed first, so that no change is committed
    /// ahead of an earlier one, then the change (with the desired state `staged`, if any), and
    /// memory only once it is committed.
    fn stored_first<E: From<OpsError>>(
        &self,
        staged: Option<Staged<'_>>,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.interleaved();
        let before = self.state();
        let mut next = before.clone();
        let mut entries = edit(&mut next)?;
        self.flush()?;
        let lines = self.numbered(&mut next, &mut entries);
        let commit = Commit {
            desired: staged.as_ref().and_then(Staged::part),
            ops: Some((&before, &next)),
            audit: &lines,
            ..Commit::default()
        };
        match &staged {
            Some(staged) => commit_across(staged.backend(), &self.backend, &commit),
            None => self.backend.commit(&commit),
        }
        .map_err(OpsError::from)?;
        *self.lock() = next;
        self.generations.raise(&entries);
        if let Some(staged) = staged {
            staged.finish();
        }
        entries.iter().for_each(announce);
        Ok(entries)
    }

    /// A memory-first change: in memory, then held pending under the marker and committed. Answers
    /// the entries and how the commit went; a commit that failed leaves the change pending.
    fn held_first<E>(
        &self,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<(Vec<AuditEntry>, Result<(), OpsError>), E> {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.interleaved();
        let before = self.state();
        let mut next = before.clone();
        let mut entries = edit(&mut next)?;
        let lines = self.numbered(&mut next, &mut entries);
        *self.lock() = next;
        entries.iter().for_each(announce);
        self.hold(before, lines);
        let committed = self.flush();
        if let Err(why) = &committed {
            tracing::error!(
                alert = "halt",
                %why,
                "a change stands in memory and is not yet written: its commit is tried again at every heartbeat and before the next change"
            );
        }
        Ok((entries, committed))
    }

    /// Numbers `entries` on from the newest number in memory or in storage, puts them in the
    /// ring of `next`, and answers their audit lines, stamped now.
    fn numbered(&self, next: &mut OpsState, entries: &mut [AuditEntry]) -> Vec<AuditLine> {
        let ringed = next.audit.last().map_or(0, |last| last.seq);
        let mut seq = self.backend.last_seq().max(ringed);
        for entry in entries.iter_mut() {
            seq += 1;
            entry.seq = seq;
        }
        next.audit.extend(entries.iter().cloned());
        let surplus = next.audit.len().saturating_sub(AUDIT_RING);
        next.audit.drain(..surplus);
        let at = (self.unix)();
        entries.iter().map(|e| AuditLine::of(e, at)).collect()
    }

    /// The page `query` asks for, from the backend: the audit files, read without reading them
    /// whole ([`AuditLog::page`]), for a store over a file.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the audit cannot be read.
    pub(crate) fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, OpsError> {
        Ok(self.backend.audit_page(query)?)
    }

    /// Visits every change the audit holds, oldest first ([`crate::store::Backend::audit_each`]).
    ///
    /// # Errors
    /// [`OpsError::Io`]: the audit cannot be read.
    pub(crate) fn audit_each(&self, visit: &mut dyn FnMut(AuditEntry)) -> Result<(), OpsError> {
        Ok(self.backend.audit_each(&mut |logged| visit(logged.entry))?)
    }

    /// The changes the audit holds numbered after `since_seq`, oldest first, for the tests.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the audit cannot be read.
    #[cfg(test)]
    pub(crate) fn audit_after(&self, since_seq: u64) -> Result<Vec<AuditEntry>, OpsError> {
        let mut all = Vec::new();
        self.audit_each(&mut |entry| {
            if entry.seq > since_seq {
                all.push(entry);
            }
        })?;
        Ok(all)
    }

    /// When older audit lines were deleted by a retention, the time of the oldest line kept
    /// ([`crate::audit_file::pruned_before`] for a store over a file); `None` when none was.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the audit cannot be read.
    pub(crate) fn audit_pruned_before(&self) -> Result<Option<u64>, OpsError> {
        Ok(self.backend.audit_pruned_before()?)
    }

    /// Seconds since the epoch by the clock of the audit: the time a hold records.
    pub(crate) fn unix_now(&self) -> u64 {
        (self.unix)()
    }
}

/// The store's changes in memory and not yet committed, their retry and their marker.
#[path = "ops_pending.rs"]
mod pending;

pub(crate) use pending::{Marker, unlanded};

/// The store's hooks for tests: a clock, a writer and an appender put in, a rival writer's change
/// made between a read and a change.
#[cfg(test)]
#[path = "ops_hooks.rs"]
mod hooks;

fn announce(entry: &AuditEntry) {
    tracing::info!(
        target: "hostkeeper_coordinator::audit",
        seq = entry.seq,
        by = %entry.by,
        action = %entry.action,
        service = %entry.service.as_deref().unwrap_or(""),
        node_id = %entry.node_id.as_deref().unwrap_or(""),
        reason = %entry.reason.as_deref().unwrap_or(""),
        "operator change"
    );
}
