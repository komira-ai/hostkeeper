//! The ops store: what operators and the coordinator itself decide, kept apart from CI's
//! desired state (`docs/design/host-updates.md`, sections 3.8 and 5).
//!
//! It holds the hosts an operator excluded from a service, the labels an operator put on a
//! host, the operator's holds ([`crate::holds`]) and assignments ([`crate::assignment`]: which
//! roles and hosts run each service, their minimum versions, the hosts ordered to uninstall
//! it), the host inventory ([`crate::inventory`]), the per-service marks later changes fill (a
//! halt, the last good version, the bad versions, the force that stands: [`crate::force`]), and
//! a ring of the last changes made. A key this build added is written only when it holds
//! something, so a file that never held one keeps the shape the release before reads; once it
//! does, the release before refuses the file, as it should: it would ignore a hold, a force or an
//! assignment.
//! It is one JSON file, `--ops-file`, kept apart from `desired.json` so that an earlier coordinator, which refuses a key it does not
//! know there, still reads the desired state. The file is created by the first change (a
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
//! A change reaches the file in one of two orders ([`Write`]). A change that *raises*
//! protection (a halt) is in memory first and written after, so a failed write never leaves
//! it unraised. Every other change, a clearing one included, is written first and enters
//! memory only if the file holds it, so a failed write never leaves it cleared.
//!
//! A halt is written under a marker ([`Write::Halt`]): the file `<ops-file>.halts.unfinished`
//! is created before the write is tried and removed once a write of the state holding the halt
//! has landed. Until then the halt stands in memory, the store says so
//! ([`OpsStore::halt_unwritten`]), and the coordinator writes the state again at every
//! heartbeat ([`OpsStore::retry_unwritten`]). A start that finds the marker is refused: the
//! halt it was writing may be lost, and the operator settles the halts file by hand and
//! removes the marker.
//!
//! Every change carries an [`AuditEntry`]: the store numbers it, keeps the last
//! [`AUDIT_RING`] in the file and logs it on the target `hostkeeper_coordinator::audit`, and
//! appends it as one JSON line to the audit file beside the ops file (`<ops-file>.audit`,
//! [`crate::audit_file`]), which keeps every change; the ring is what `/v1/status` shows.
//!
//! The audit file is the durable record, so a [`Write::BeforeMemory`] change appends its lines
//! first, before the ops file is written, and is refused with [`OpsError::AuditUnavailable`]
//! when a line cannot be appended (a full disk): nothing of it is made. An audit line can so name
//! a change whose ops-file write then failed, or, for a change of several entries (a force), the
//! entries appended before the one that failed; but no change is made without its lines. Two kinds
//! of change still stand when their line cannot be appended, logged as an error with the entry
//! in the ring: a change that raises protection ([`Write::AfterMemory`], [`Write::Halt`]: a
//! halt, a bad mark), and an operator's `abort` ([`OpsStore::escape`]), the operator's way out
//! when the disk is full.
//!
//! The caps on the counts (`MAX_HOSTS`, `MAX_EXCLUDED`) are checked under the same lock as the
//! write ([`OpsStore::try_change`]), on the state the change is applied to.
//!
//! The operator actions ([`operate`]) write exclusions, labels and the clear of a halt; the
//! coordinator itself writes the raising of a halt ([`crate::halt`]) and, in
//! [`crate::reconcile`], the last good version and the marking of a version bad (a raise, so in
//! memory first); CI's set of a new desired version lifts a halt the updater raised
//! ([`crate::halt::lift_by_ci`]), and never a bad mark. The decision reads a halt and the bad
//! versions ([`crate::halt::decide`]); nothing reads the rest when it decides what a host is
//! told.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::UpdateResult;
use serde::{Deserialize, Serialize};

use crate::assignment::ServiceAssignments;
use crate::audit_file::{
    self, Appender, AuditLine, AuditLog, DEFAULT_ROTATION, Rotation, UnixClock, wall_clock,
};
use crate::desired::StoreError;
use crate::force::ForceRecord;
use crate::holds::HoldMark;
use crate::inventory::InventoryRecord;
use crate::ops_file;

pub use crate::operate::{OperateError, operate};

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
}

/// One change, as the ring and the log say it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    /// Its number: one more than the entry before it, for the life of the file.
    pub seq: u64,
    /// Who made it: the name of the operator's token (`operator` for `--operator-token-file`),
    /// `coordinator` for the coordinator's own changes, `ci` for CI's.
    pub by: String,
    /// What it was (`exclude`, `exclude_abandon`, `include`, `set_labels`, `clear_halt`,
    /// `halt`, `mark_bad`, `unmark_bad`, `abort`, `hold`, `unhold`, `assign`, `unassign`,
    /// `set_min_version`, `force` with the `unmark_bad`, `mark_bad` and `clear_halt` it made;
    /// the coordinator's own: `last_good`, `clear_last_good`, `mark_bad`, `uninstalled` and
    /// `uninstall_cancelled`; CI's: `lift_halt`, `force_ended`, and `unmark_bad` in a file an
    /// earlier build wrote).
    pub action: String,
    /// The service it concerned, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The node it concerned, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The operator's reason, if one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
            reason: None,
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
        }
    }
}

impl OpsState {
    /// Nothing has been changed: the state of a coordinator with no ops file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
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

/// Which order a change reaches the file and memory in.
#[derive(Clone, Copy, Debug)]
pub enum Write {
    /// In memory first, written after: for a change that raises protection (a bad mark). A
    /// failed write leaves it raised, and the next write carries it.
    AfterMemory,
    /// [`Write::AfterMemory`] for a halt: the write is tried under the unfinished-halt marker,
    /// and one that fails is tried again at every heartbeat until it lands
    /// ([`OpsStore::retry_unwritten`]).
    Halt,
    /// Written first, in memory only if the file holds it: for every other change, a
    /// clearing one included. A failed write leaves the old state.
    BeforeMemory,
}

/// A change [`OpsStore::interleave`] makes at the start of the next change.
#[cfg(test)]
type Interleaved = Box<dyn FnOnce(&mut OpsState) + Send>;

/// What puts a state into the file.
pub type Writer = Box<dyn Fn(&OpsState) -> Result<(), OpsError> + Send + Sync>;

/// What a change does when one of its audit lines cannot be appended.
#[derive(Clone, Copy, Debug)]
enum Audited {
    /// It is refused, [`OpsError::AuditUnavailable`], before the ops file is written.
    Required,
    /// It stands: the failure is logged as an error, and the ring holds the entry.
    Logged,
}

impl Audited {
    /// What a change written in the order `write` does: a change that raises protection stands,
    /// every other is refused.
    fn of(write: Write) -> Self {
        match write {
            Write::AfterMemory | Write::Halt => Self::Logged,
            Write::BeforeMemory => Self::Required,
        }
    }
}

/// The ops state, in memory and in its file.
pub struct OpsStore {
    current: Mutex<OpsState>,
    /// Held across a whole change, so that writers wait for each other and not readers:
    /// `current` is only locked to read or to swap, never across the file's fsyncs.
    writing: Mutex<()>,
    write: Writer,
    /// The audit file, for a store over a file; the others have none.
    audit: Option<Arc<AuditLog>>,
    /// What appends a line to the audit file: the audit file's own append, or nothing for a
    /// store with none.
    append: Appender,
    /// Seconds since the epoch, for the lines of the audit file.
    unix: UnixClock,
    /// The unfinished-halt marker, for a store over a file; the others have none.
    marker: Option<PathBuf>,
    /// Whether a halt is in memory and not yet in the file, and when the retry last logged.
    unwritten: Mutex<Unwritten>,
    /// A change another writer makes between a caller's read and its change, applied once at
    /// the start of the next change, under the writers' lock: what a race would do there.
    #[cfg(test)]
    interleave: Mutex<Option<Interleaved>>,
}

/// A halt that is in memory and not yet in the file.
#[derive(Debug, Default)]
struct Unwritten {
    /// The write of a halt failed and no write has landed since.
    pending: bool,
    /// When a retry last logged that it failed.
    logged: Option<Duration>,
}

/// How often a failing retry of an unwritten halt is logged at most.
pub const UNWRITTEN_LOG_EVERY: Duration = Duration::from_secs(60);

fn parse(path: &Path, bytes: &[u8]) -> Result<OpsState, OpsError> {
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
        let state = match fs::read(path) {
            Ok(bytes) => parse(path, &bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => OpsState::default(),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.to_owned(),
                    source,
                }
                .into());
            }
        };
        let (state, _skipped) = ops_file::joined(path, state)?;
        let log = Arc::new(AuditLog::open(audit_file::path_of(path), rotation)?);
        let marker = ops_file::unfinished_path(path);
        let path = path.to_owned();
        let appends = Arc::clone(&log);
        Ok(Self {
            audit: Some(log),
            append: Box::new(move |line| appends.append(line)),
            marker: Some(marker),
            ..Self::with_writer(state, Box::new(move |state| ops_file::write(&path, state)))
        })
    }

    /// A store over `state` that puts every change through `write` instead of a file.
    #[must_use]
    pub fn with_writer(state: OpsState, write: Writer) -> Self {
        Self {
            current: Mutex::new(state),
            writing: Mutex::new(()),
            write,
            audit: None,
            append: Box::new(|_| Ok(())),
            unix: wall_clock(),
            marker: None,
            unwritten: Mutex::new(Unwritten::default()),
            #[cfg(test)]
            interleave: Mutex::new(None),
        }
    }

    /// A store that keeps nothing: changes stand in memory only. For a coordinator built
    /// without a file.
    #[must_use]
    pub fn volatile() -> Self {
        Self::with_writer(OpsState::default(), Box::new(|_| Ok(())))
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
    /// reaches the file and memory in the order `write` says. Returns the entry as numbered.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the file could not be replaced. After [`Write::BeforeMemory`] the
    /// old state stands; after [`Write::AfterMemory`] the change stands in memory.
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
    /// returned and nothing else happens: the state, the ring and its numbering and the file
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
    /// order, in one write of the file (a force: [`crate::force`]). The edit answers at least
    /// one entry.
    ///
    /// # Errors
    /// The edit's own error, or [`OpsError`] as for [`Self::change`].
    pub(crate) fn decide_changes<E: From<OpsError>>(
        &self,
        write: Write,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        self.audited_changes(write, Audited::of(write), edit)
    }

    /// The operator's `abort` (`Hosts.Abort`): a change of nothing but the audit, written
    /// before memory, that stands when its audit line cannot be appended, as a halt does. It is
    /// the operator's way out when the disk is full; the failure is logged as an error and the
    /// ring holds the entry.
    ///
    /// # Errors
    /// [`OpsError::Io`]: the ops file could not be replaced; nothing changed.
    pub(crate) fn escape(&self, entry: AuditEntry) -> Result<AuditEntry, OpsError> {
        let mut entries = self.audited_changes(Write::BeforeMemory, Audited::Logged, |_| {
            Ok::<_, OpsError>(vec![entry])
        })?;
        Ok(entries.swap_remove(0))
    }

    /// [`Self::decide_changes`], doing what `audited` says when an audit line cannot be
    /// appended. A [`Write::BeforeMemory`] change appends its lines first, before the ops file
    /// is written: a change refused for its audit writes nothing else.
    fn audited_changes<E: From<OpsError>>(
        &self,
        write: Write,
        audited: Audited,
        edit: impl FnOnce(&mut OpsState) -> Result<Vec<AuditEntry>, E>,
    ) -> Result<Vec<AuditEntry>, E> {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.interleaved();
        let mut next = self.state();
        let mut entries = edit(&mut next)?;
        let filed = self.audit.as_deref().map_or(0, AuditLog::last_seq);
        let ringed = next.audit.last().map_or(0, |last| last.seq);
        let mut seq = filed.max(ringed);
        for entry in &mut entries {
            seq += 1;
            entry.seq = seq;
        }
        next.audit.extend(entries.iter().cloned());
        let surplus = next.audit.len().saturating_sub(AUDIT_RING);
        next.audit.drain(..surplus);
        match write {
            Write::AfterMemory => {
                *self.lock() = next.clone();
                self.record_all(&entries, audited)?;
                (self.write)(&next)?;
            }
            Write::Halt => {
                *self.lock() = next.clone();
                self.record_all(&entries, audited)?;
                self.mark_unwritten();
                (self.write)(&next)?;
            }
            Write::BeforeMemory => {
                self.record_all(&entries, audited)?;
                (self.write)(&next)?;
                *self.lock() = next;
            }
        }
        // The state just written holds every halt in memory: one whose write had failed has
        // landed with it.
        self.written();
        Ok(entries)
    }

    /// Every change the audit file and its kept segments hold, oldest first; for a store with
    /// no audit file, the ring.
    ///
    /// # Errors
    /// [`OpsError::Io`]: a file of the audit exists and cannot be read.
    pub(crate) fn audit_log(&self) -> Result<Vec<AuditEntry>, OpsError> {
        self.audit_after(0)
    }

    /// The changes of [`Self::audit_log`] numbered after `since_seq`, oldest first. A
    /// `since_seq` in a segment deleted since answers from the oldest change kept.
    ///
    /// # Errors
    /// [`OpsError::Io`]: a file of the audit exists and cannot be read.
    pub(crate) fn audit_after(&self, since_seq: u64) -> Result<Vec<AuditEntry>, OpsError> {
        let Some(log) = &self.audit else {
            let ring = self.lock().audit.clone();
            return Ok(ring.into_iter().filter(|e| e.seq > since_seq).collect());
        };
        let read = log.read_after(since_seq)?;
        Ok(read.lines.into_iter().map(AuditLine::into_entry).collect())
    }

    /// When segments of the audit file were deleted by its retention, the time of the oldest
    /// line kept ([`audit_file::pruned_before`]); `None` when none was, and for a store with no
    /// audit file.
    ///
    /// # Errors
    /// [`OpsError::Io`]: a file of the audit exists and cannot be read.
    pub(crate) fn audit_pruned_before(&self) -> Result<Option<u64>, OpsError> {
        self.audit
            .as_ref()
            .map_or(Ok(None), |log| audit_file::pruned_before(log.path()))
    }

    /// Seconds since the epoch by the clock of the audit file: the time a hold records.
    pub(crate) fn unix_now(&self) -> u64 {
        (self.unix)()
    }

    /// Whether a halt is in memory and not yet in the file: its write failed and no write has
    /// landed since. Orders for its service stay held all the while: the halt is in memory.
    #[must_use]
    pub fn halt_unwritten(&self) -> bool {
        self.unwritten_lock().pending
    }

    /// Writes the state again when a halt is not yet in the file: called at every heartbeat
    /// with the coordinator's time `now`. A retry that fails is logged at most once per
    /// [`UNWRITTEN_LOG_EVERY`]; one that lands removes the marker.
    pub fn retry_unwritten(&self, now: Duration) {
        if !self.halt_unwritten() {
            return;
        }
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        self.mark_unwritten();
        match (self.write)(&self.state()) {
            Ok(()) => {
                self.written();
                tracing::warn!(
                    alert = "halt",
                    "a halt that could not be written is written now"
                );
            }
            Err(why) => {
                let mut unwritten = self.unwritten_lock();
                let due = unwritten
                    .logged
                    .is_none_or(|at| now.saturating_sub(at) >= UNWRITTEN_LOG_EVERY);
                if due {
                    unwritten.logged = Some(now);
                    drop(unwritten);
                    tracing::error!(
                        alert = "halt",
                        %why,
                        "a halt is still not written to the ops file: it stands in memory, and the write is tried at every heartbeat"
                    );
                }
            }
        }
    }

    fn unwritten_lock(&self) -> MutexGuard<'_, Unwritten> {
        self.unwritten
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes that a halt is about to be written: pending until a write lands, and the marker
    /// created (and its directory synced) before the write is tried. A marker that cannot be
    /// created is logged; the write is tried all the same.
    fn mark_unwritten(&self) {
        self.unwritten_lock().pending = true;
        let Some(marker) = &self.marker else {
            return;
        };
        let made = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(marker)
            .and_then(|_| fs::File::open(crate::desired::dir_of(marker))?.sync_all());
        if let Err(why) = made {
            tracing::error!(
                alert = "halt",
                path = %marker.display(),
                %why,
                "the marker of a halt being written could not be made"
            );
        }
    }

    /// Notes that a write of the state landed: a halt pending is in the file, and the marker
    /// is removed. A marker that cannot be removed keeps the halt pending, so the next
    /// heartbeat tries again.
    fn written(&self) {
        let mut unwritten = self.unwritten_lock();
        if !unwritten.pending {
            return;
        }
        if let Some(marker) = &self.marker {
            match fs::remove_file(marker) {
                Err(why) if why.kind() != io::ErrorKind::NotFound => {
                    tracing::error!(
                        path = %marker.display(),
                        %why,
                        "the marker of a halt written could not be removed"
                    );
                    return;
                }
                _ => {}
            }
        }
        *unwritten = Unwritten::default();
    }

    /// [`Self::record`] for each of `entries`, in order; under [`Audited::Required`], none after
    /// the first that cannot be appended.
    ///
    /// # Errors
    /// [`OpsError::AuditUnavailable`]: under [`Audited::Required`], a line could not be appended.
    fn record_all(&self, entries: &[AuditEntry], audited: Audited) -> Result<(), OpsError> {
        entries
            .iter()
            .try_for_each(|entry| self.record(entry, audited))
    }

    /// Appends `entry` to the audit file, if the store has one, and logs it. A line that cannot
    /// be appended is an error in the log; under [`Audited::Logged`] the change stands (the ring
    /// holds it) and under [`Audited::Required`] it is refused.
    ///
    /// # Errors
    /// [`OpsError::AuditUnavailable`]: under [`Audited::Required`], the line could not be
    /// appended.
    fn record(&self, entry: &AuditEntry, audited: Audited) -> Result<(), OpsError> {
        if let Err(why) = (self.append)(&AuditLine::of(entry, (self.unix)())) {
            if let Audited::Required = audited {
                tracing::error!(
                    target: "hostkeeper_coordinator::audit",
                    seq = entry.seq,
                    %why,
                    "the audit file could not be appended to: the change is refused, audit_unavailable"
                );
                return Err(OpsError::AuditUnavailable(why));
            }
            tracing::error!(
                target: "hostkeeper_coordinator::audit",
                seq = entry.seq,
                %why,
                "the audit file could not be appended to: the change stands, and the ring holds it"
            );
        }
        announce(entry);
        Ok(())
    }
}

#[cfg(test)]
impl OpsStore {
    /// The store with `unix` as its clock for the audit file.
    pub(crate) fn with_unix(mut self, unix: UnixClock) -> Self {
        self.unix = unix;
        self
    }

    /// The store with `write` putting its states into the file instead, its marker kept.
    pub(crate) fn with_write(mut self, write: Writer) -> Self {
        self.write = write;
        self
    }

    /// The store with `append` appending its audit lines instead of the audit file, which is
    /// still what the store reads.
    pub(crate) fn with_append(mut self, append: Appender) -> Self {
        self.append = append;
        self
    }

    /// Makes `change` at the start of the next change, under the writers' lock, as another
    /// writer that got there between that caller's read and its change would.
    pub(crate) fn interleave(&self, change: impl FnOnce(&mut OpsState) + Send + 'static) {
        *self
            .interleave
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(change));
    }

    fn interleaved(&self) {
        let change = self
            .interleave
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(change) = change {
            change(&mut self.lock());
        }
    }
}

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
