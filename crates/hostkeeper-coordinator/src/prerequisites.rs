//! The host prerequisites: what the coordinator names, for each host group, that its hosts must
//! have (feature [`FEATURE`], `prerequisites-v1`), and what each daemon reported of them. Today the
//! one prerequisite is Apple's Command Line Tools for Xcode on a Mac
//! (`hostkeeper_wire::prerequisites::Prerequisite`). The file holds no secret.
//!
//! `--host-prerequisites-file` is TOML, one table for each host group that needs a prerequisite:
//!
//! ```toml
//! [host-groups.mac-build]
//! prerequisites = ["xcode-command-line-tools"]
//! ```
//!
//! The top level holds only `host-groups`, which may be absent or empty (no group needs one). Each
//! name is a host group name ([`GroupName`]); each entry holds only `prerequisites`, an array of
//! strings, each a name this release knows (`Prerequisite::parse`), and the group's set keeps the
//! rules of `prerequisites::check_set` (at most 16 names, none twice). A refusal names the flag,
//! the group and the name's place in its array, and for an unknown name the names this release
//! knows; never the path. The file is read as a plain file: a regular file of at most 64 KiB, with
//! no rule of owner or mode.
//!
//! The coordinator reads the file at start and again on the tick that looks at the TLS files
//! (`--tls-reload-secs`, [`crate::listen::reload_every`]): a group's names changed in the file are
//! sent from the next heartbeat of each host of the group, a group taken out of the file is sent
//! the empty set (a daemon never removes a prerequisite, so its hosts are left as they are), and a
//! file that cannot be read again, or is refused, leaves the sets in use.
//!
//! # What a host is sent
//!
//! A daemon that announces [`FEATURE`] reports, in every heartbeat over gRPC, the digest of the
//! set it holds, the last set it refused and the state of each prerequisite ([`Report`]). Its
//! answer carries the set of the host's group ([`Coordinator::prerequisites_for`]) when this
//! coordinator advertises the feature and was given a file, unless the host's last report holds
//! that set's digest as held or as refused. A host in no group, or in a group the file does not
//! list, is sent the empty set. A daemon that does not announce the feature and a daemon that
//! beats over the JSON route are sent and shown nothing.
//!
//! The reports are kept in memory only: after a restart the coordinator holds none until each
//! host's next heartbeat, which carries it again. A report that breaks a rule is counted and
//! logged as a dropped fact, and the one read before is kept. A status value this release does not
//! know is kept as it came. `Hosts.List` and `Hosts.Get` show the report in the host's heartbeat
//! ([`Coordinator::show_prerequisites`]), and the alert `prerequisite_failed` is raised for a host
//! whose last report holds a failed state ([`crate::alerts`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;
use hostkeeper_wire::operator_keys::is_digest;
use hostkeeper_wire::prerequisites::{
    KNOWN, MAX_PREREQUISITES, Prerequisite, PrerequisiteSetError, check_set, digest, is_name,
};

use crate::fleet::Fleet;
use crate::host_groups::GroupName;
use crate::identity::Identity;
use crate::repository_push::{MAX_REASON, cut};
use crate::service::Coordinator;

/// The feature a daemon announces to be sent the host prerequisites of its host group and to
/// report them; the coordinator advertises it ([`crate::features::FEATURES`]).
pub const FEATURE: &str = "prerequisites-v1";

/// The name a prerequisites report that breaks a rule is counted under.
pub(crate) const FACT: &str = "prerequisites";

/// The most bytes of the host prerequisites file.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// The most bytes of a state's version.
const MAX_VERSION: usize = 64;

/// The top-level key of the file that holds the groups.
const GROUPS: &str = "host-groups";

/// The one key a group's entry holds.
const PREREQUISITES: &str = "prerequisites";

/// Why the host prerequisites file is refused. The texts name the flag, never the path; a name is
/// named by its place in its group's array, counting from 1.
#[derive(Debug, thiserror::Error)]
pub enum PrerequisitesError {
    /// The file cannot be read (what reading it said), or is not UTF-8.
    #[error("--host-prerequisites-file: {0}")]
    Read(#[source] io::Error),
    /// The path is not a regular file.
    #[error("--host-prerequisites-file: not a regular file")]
    NotAFile,
    /// The file is longer than [`MAX_FILE_BYTES`].
    #[error(
        "--host-prerequisites-file: {bytes} bytes, more than the {MAX_FILE_BYTES} the file may hold"
    )]
    TooBig {
        /// Its size.
        bytes: u64,
    },
    /// The text is not TOML (the text of the parser).
    #[error("--host-prerequisites-file: not TOML: {0}")]
    NotToml(String),
    /// A top-level key other than `host-groups`.
    #[error(
        "--host-prerequisites-file: the key {key:?} is not known; the file holds only host-groups"
    )]
    UnknownKey {
        /// The key.
        key: String,
    },
    /// `host-groups` is not a table.
    #[error("--host-prerequisites-file: host-groups is not a table of host groups")]
    GroupsNotTable,
    /// A group's name is no host group name.
    #[error(
        "--host-prerequisites-file: {name:?} is not a host group name (1 to 32 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
    )]
    GroupName {
        /// The name.
        name: String,
    },
    /// A group's entry is not a table.
    #[error(
        "--host-prerequisites-file: host group {group}: it is not a table holding prerequisites"
    )]
    GroupNotTable {
        /// The group.
        group: String,
    },
    /// A group's entry holds a key other than `prerequisites`.
    #[error(
        "--host-prerequisites-file: host group {group}: the key {key:?} is not known; it holds only prerequisites"
    )]
    UnknownGroupKey {
        /// The group.
        group: String,
        /// The key.
        key: String,
    },
    /// A group's entry has no `prerequisites`.
    #[error(
        "--host-prerequisites-file: host group {group}: prerequisites is missing (write prerequisites = [] for none)"
    )]
    NoPrerequisites {
        /// The group.
        group: String,
    },
    /// A group's `prerequisites` is not an array.
    #[error(
        "--host-prerequisites-file: host group {group}: prerequisites is not an array of strings"
    )]
    NotArray {
        /// The group.
        group: String,
    },
    /// An element of a group's `prerequisites` is not a string.
    #[error("--host-prerequisites-file: host group {group}: prerequisite {place} is not a string")]
    NotText {
        /// The group.
        group: String,
        /// The element's place.
        place: usize,
    },
    /// An element of a group's `prerequisites` is a name this release does not know.
    #[error(
        "--host-prerequisites-file: host group {group}: prerequisite {place} is not a prerequisite this release knows (it knows {known})"
    )]
    Unknown {
        /// The group.
        group: String,
        /// The name's place.
        place: usize,
        /// The names this release knows, separated by commas.
        known: String,
    },
    /// A group's set breaks a rule of a set: too many names, one twice.
    #[error("--host-prerequisites-file: host group {group}: {why}")]
    Set {
        /// The group.
        group: String,
        /// Why.
        why: PrerequisiteSetError,
    },
}

/// The prerequisites of one host group, as they are sent: in the file's order, with their digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Set {
    /// The names.
    pub names: Vec<String>,
    /// The digest of `names` (`hostkeeper_wire::prerequisites::digest`).
    pub digest: String,
}

impl Set {
    /// The set of `names`, which keep the rules of a set.
    fn of(names: Vec<String>) -> Self {
        Self {
            digest: digest(&names),
            names,
        }
    }

    /// The set as the answer to a heartbeat carries it.
    fn to_proto(&self) -> agent::HostPrerequisites {
        agent::HostPrerequisites {
            digest: self.digest.clone(),
            names: self.names.clone(),
        }
    }
}

/// What a daemon reported of the prerequisites, as it is kept: the digest of the set it holds
/// (empty while it holds none the coordinator sent), the last set it refused, and the state of
/// each prerequisite, with the texts cut.
pub type Report = agent::PrerequisitesReport;

/// The groups' sets of the prerequisites `text` holds.
fn parse(text: &str) -> Result<BTreeMap<String, Set>, PrerequisitesError> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| PrerequisitesError::NotToml(e.message().to_owned()))?;
    let mut sets = BTreeMap::new();
    for (name, value) in table {
        if name != GROUPS {
            return Err(PrerequisitesError::UnknownKey { key: name });
        }
        let toml::Value::Table(groups) = value else {
            return Err(PrerequisitesError::GroupsNotTable);
        };
        for (group, entry) in groups {
            let set = group_set(&group, entry)?;
            sets.insert(group, set);
        }
    }
    Ok(sets)
}

/// The set of the group `group` whose entry is `entry`.
fn group_set(group: &str, entry: toml::Value) -> Result<Set, PrerequisitesError> {
    let named = || group.to_owned();
    if group.parse::<GroupName>().is_err() {
        return Err(PrerequisitesError::GroupName { name: named() });
    }
    let toml::Value::Table(mut entry) = entry else {
        return Err(PrerequisitesError::GroupNotTable { group: named() });
    };
    if let Some(key) = entry.keys().find(|key| *key != PREREQUISITES) {
        return Err(PrerequisitesError::UnknownGroupKey {
            group: named(),
            key: key.clone(),
        });
    }
    let Some(listed) = entry.remove(PREREQUISITES) else {
        return Err(PrerequisitesError::NoPrerequisites { group: named() });
    };
    let toml::Value::Array(listed) = listed else {
        return Err(PrerequisitesError::NotArray { group: named() });
    };
    let mut names = Vec::with_capacity(listed.len());
    for (index, item) in listed.into_iter().enumerate() {
        let place = index + 1;
        let toml::Value::String(name) = item else {
            return Err(PrerequisitesError::NotText {
                group: named(),
                place,
            });
        };
        if Prerequisite::parse(&name).is_none() {
            let known: Vec<&str> = KNOWN.iter().map(|known| known.name()).collect();
            return Err(PrerequisitesError::Unknown {
                group: named(),
                place,
                known: known.join(", "),
            });
        }
        names.push(name);
    }
    check_set(&names).map_err(|why| PrerequisitesError::Set {
        group: named(),
        why,
    })?;
    Ok(Set::of(names))
}

/// The sets of the file at `path`: a regular file of at most [`MAX_FILE_BYTES`].
fn read(path: &Path) -> Result<BTreeMap<String, Set>, PrerequisitesError> {
    let meta = std::fs::metadata(path).map_err(PrerequisitesError::Read)?;
    if !meta.is_file() {
        return Err(PrerequisitesError::NotAFile);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(PrerequisitesError::TooBig { bytes: meta.len() });
    }
    let text = std::fs::read_to_string(path).map_err(PrerequisitesError::Read)?;
    parse(&text)
}

/// What one look at the host prerequisites file did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    /// The file holds other sets than the ones in use: they are in use now.
    Applied,
    /// The file holds the sets in use, or there is no file to look at.
    Unchanged,
    /// The file could not be read or was refused: the sets in use stay.
    Rejected,
}

#[derive(Debug)]
struct Inner {
    /// The file the sets came from; none without `--host-prerequisites-file`.
    source: Option<PathBuf>,
    /// The set of each group the file lists; none without the file.
    sets: RwLock<Option<BTreeMap<String, Set>>>,
    /// The last report of each host that announced [`FEATURE`], under a lock of its own, taken
    /// after the fleet's and holding nothing else.
    reports: Mutex<BTreeMap<HostId, Report>>,
}

/// The host prerequisites file's sets and the reports of the hosts, shared by the coordinator and
/// the tick that re-reads the file.
#[derive(Clone, Debug)]
pub struct HostPrerequisites {
    inner: Arc<Inner>,
}

impl Default for HostPrerequisites {
    /// No file: no host is ever sent a set.
    fn default() -> Self {
        Self::of(None, None)
    }
}

impl HostPrerequisites {
    fn of(source: Option<PathBuf>, sets: Option<BTreeMap<String, Set>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                source,
                sets: RwLock::new(sets),
                reports: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// The sets of the file at `path`, which [`Self::reload`] reads again.
    ///
    /// # Errors
    /// [`PrerequisitesError`]: the file cannot be read, is not a regular file, is over 64 KiB,
    /// is not UTF-8 or TOML of the shape, or holds a group name, a prerequisite or a set the
    /// rules above refuse.
    pub fn from_file(path: &Path) -> Result<Self, PrerequisitesError> {
        read(path).map(|sets| Self::of(Some(path.to_owned()), Some(sets)))
    }

    /// Whether a file was given: only then is any host sent a set.
    #[must_use]
    pub fn is_given(&self) -> bool {
        self.inner.source.is_some()
    }

    /// The set the file lists for `group`; none when there is no file or it does not list the
    /// group.
    #[must_use]
    pub fn set_of(&self, group: &str) -> Option<Set> {
        let sets = self
            .inner
            .sets
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        sets.as_ref()?.get(group).cloned()
    }

    /// The set a host of `group` is sent: the group's, or the empty set when the file does not
    /// list it.
    fn to_send(&self, group: &str) -> Set {
        self.set_of(group).unwrap_or_else(|| Set::of(Vec::new()))
    }

    /// Reads the file again. Sets that changed are in use from now on; a file that cannot be
    /// read or is refused leaves the sets in use and is logged as a warning.
    pub fn reload(&self) -> Reload {
        let Some(path) = &self.inner.source else {
            return Reload::Unchanged;
        };
        match read(path) {
            Ok(read) => {
                let mut held = self
                    .inner
                    .sets
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                if held.as_ref() == Some(&read) {
                    return Reload::Unchanged;
                }
                let groups = read.len();
                *held = Some(read);
                drop(held);
                tracing::info!(
                    groups,
                    "the host prerequisites file changed: its sets are in use"
                );
                Reload::Applied
            }
            Err(why) => {
                tracing::warn!(%why, "the host prerequisites file was not read again: the sets in use stay");
                Reload::Rejected
            }
        }
    }

    /// The last report of the host `id`; none for a host that never announced the feature.
    #[must_use]
    pub fn report_of(&self, id: &HostId) -> Option<Report> {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// The last report of every host that announced the feature.
    #[must_use]
    pub fn reports(&self) -> BTreeMap<HostId, Report> {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Notes `report` as the host `id`'s last.
    fn note(&self, id: &HostId, report: Report) {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), report);
    }

    /// Forgets the host `id`'s report, when the host is retired.
    pub(crate) fn forget(&self, id: &HostId) {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

/// The report `said` holds, or `None` when it breaks a rule: a digest that is neither empty nor
/// 64 lower-case hex characters, a refusal whose digest is no digest or whose reason is empty,
/// more than [`MAX_PREREQUISITES`] states, a state whose name is no prerequisite name or is named
/// twice. A status value it does not know is kept as it came. The version is cut to
/// [`MAX_VERSION`] bytes and the reasons to [`MAX_REASON`] bytes at a character boundary.
fn kept(said: &Report) -> Option<Report> {
    if !said.digest.is_empty() && !is_digest(&said.digest) {
        return None;
    }
    let refused = match &said.refused {
        None => None,
        Some(refusal) => {
            if !is_digest(&refusal.digest) || refusal.reason.is_empty() {
                return None;
            }
            Some(agent::PrerequisitesRefusal {
                digest: refusal.digest.clone(),
                reason: cut(&refusal.reason, MAX_REASON).to_owned(),
            })
        }
    };
    if said.states.len() > MAX_PREREQUISITES {
        return None;
    }
    let mut names = BTreeSet::new();
    let mut states = Vec::with_capacity(said.states.len());
    for state in &said.states {
        if !is_name(&state.name) || !names.insert(state.name.as_str()) {
            return None;
        }
        states.push(agent::PrerequisiteState {
            version: cut(&state.version, MAX_VERSION).to_owned(),
            reason: cut(&state.reason, MAX_REASON).to_owned(),
            ..state.clone()
        });
    }
    Some(Report {
        digest: said.digest.clone(),
        refused,
        states,
    })
}

/// Whether `report` holds the set of `digest`, or refused it: it is not sent again.
fn holds(report: &Report, digest: &str) -> bool {
    report.digest == digest || report.refused.as_ref().is_some_and(|r| r.digest == digest)
}

/// Whether a heartbeat over gRPC announces [`FEATURE`].
fn announces(request: &agent::HeartbeatRequest) -> bool {
    request.features.iter().any(|f| f == FEATURE)
}

impl Coordinator {
    /// The same coordinator over the host prerequisites `prerequisites` instead of none.
    #[must_use]
    pub fn with_host_prerequisites(mut self, prerequisites: HostPrerequisites) -> Self {
        self.prerequisites = prerequisites;
        self
    }

    /// The host prerequisites file's sets and the hosts' reports.
    #[must_use]
    pub fn host_prerequisites(&self) -> &HostPrerequisites {
        &self.prerequisites
    }

    /// Notes what a heartbeat over gRPC from `who` said of the prerequisites, when its daemon
    /// announced [`FEATURE`]: the report, or the empty one when it sent none; a report that breaks
    /// a rule is counted and logged as a dropped fact, and the one read before is kept.
    pub(crate) fn hear_prerequisites(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        if !announces(request) {
            return;
        }
        let report = match &request.prerequisites {
            None => Some(Report::default()),
            Some(said) => kept(said),
        };
        match report {
            Some(report) => self.prerequisites.note(&who.host_id, report),
            None => {
                self.note_extras_dropped(who, "the prerequisites report breaks a rule", &[FACT]);
            }
        }
    }

    /// The host prerequisites the answer to a heartbeat over gRPC from `who` carries, if it
    /// carries them: when the daemon announced [`FEATURE`], this coordinator advertises it and was
    /// given a file, and the host's report holds neither the digest of the set nor refused it. The
    /// set is that of the host's group, as the module says, read under the fleet's lock so that a
    /// move refused after it wrote the host's new group is never seen. A group that cannot be read
    /// gives none: the next heartbeat tries again.
    pub(crate) fn prerequisites_for(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) -> Option<agent::HostPrerequisites> {
        if !announces(request)
            || !self.features.iter().any(|f| f == FEATURE)
            || !self.prerequisites.is_given()
        {
            return None;
        }
        // Under the fleet's lock, which a move holds from the write of the host's group to the
        // end of its commit, as `host_facts` reads it.
        let group = {
            let _fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
            self.approved_group(who)?
        };
        let set = self.prerequisites.to_send(&group);
        let held = self
            .prerequisites
            .report_of(&who.host_id)
            .unwrap_or_default();
        (!holds(&held, &set.digest)).then(|| set.to_proto())
    }

    /// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id` (`fleet` is the
    /// fleet as they read it), with the last report of its daemon, in its heartbeat, when the
    /// daemon announced [`FEATURE`].
    pub(crate) fn show_prerequisites(&self, fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
        if !fleet.announced(id, FEATURE) {
            return;
        }
        if let Some(report) = self.prerequisites.report_of(id) {
            host.heartbeat.get_or_insert_default().prerequisites = Some(report);
        }
    }
}

#[cfg(test)]
#[path = "prerequisites_tests.rs"]
pub(crate) mod tests;
