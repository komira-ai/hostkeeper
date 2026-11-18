//! The operator keys: the SSH public keys the coordinator sends each host's daemon, for the
//! managed block of its login account's `authorized_keys` (feature [`FEATURE`], `operator-keys-v1`),
//! and what each daemon reported of them. A public key is not a secret; the file holds no user
//! name and no password.
//!
//! `--operator-keys-file` is TOML, one table for each host group that has keys:
//!
//! ```toml
//! [host-groups.farm-linux]
//! keys = ["ssh-ed25519 AAAA... ops laptop", "..."]
//! [host-groups.farm-mac]
//! keys = []
//! ```
//!
//! The top level holds only `host-groups`, which may be absent or empty (no group has keys). Each
//! name is a host group name ([`GroupName`]); each entry holds only `keys`, an array of strings,
//! each one key line in the form `hostkeeper_wire::operator_keys::public_key` reads (extra white
//! space is dropped), and the group's set keeps the rules of `operator_keys::check_set` (at most 64
//! keys, each at most 4096 bytes, none twice). A refusal names the flag, the group and the key's
//! place in its array, never a key's text and never the path. The file is read as a plain file: a
//! regular file of at most 256 KiB, with no rule of owner or mode.
//!
//! The coordinator reads the file at start and again on the tick that looks at the TLS files
//! (`--tls-reload-secs`, [`crate::listen::reload_every`]): a group's keys changed in the file are
//! sent from the next heartbeat of each host of the group, a group taken out of the file takes
//! its keys away from its hosts (they are sent the empty set), and a file that cannot be read
//! again, or is refused, leaves the sets in use.
//!
//! # What a host is sent
//!
//! A daemon that announces [`FEATURE`] reports, in every heartbeat over gRPC, the digest of the
//! set in force and the last set it refused ([`Report`]). Its answer carries the set of the host's
//! group ([`Coordinator::operator_keys_for`]) when this coordinator advertises the feature and was
//! given a file, unless the host's last report holds that set's digest in force or refused. A
//! host in no group, or in a group the file does not list, is sent the empty set. A daemon that
//! does not announce the feature and a daemon that beats over the JSON route are sent and shown
//! nothing.
//!
//! The reports are kept in memory only, beside nothing else: after a restart the coordinator holds
//! none until each host's next heartbeat, which carries it again. A report that breaks a rule is
//! counted and logged as a dropped fact, and the one read before is kept. `Hosts.List` and
//! `Hosts.Get` show the report in the host's heartbeat ([`Coordinator::show_operator_keys`]).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;
use hostkeeper_wire::operator_keys::{
    KeyLineError, KeySetError, check_set, digest, is_digest, public_key,
};

use crate::fleet::Fleet;
use crate::host_groups::GroupName;
use crate::identity::Identity;
use crate::repository_push::{MAX_REASON, cut};
use crate::service::Coordinator;

/// The feature a daemon announces to be sent the operator keys of its host group and to report
/// them; the coordinator advertises it ([`crate::features::FEATURES`]).
pub const FEATURE: &str = "operator-keys-v1";

/// The name an operator keys report that breaks a rule is counted under.
pub(crate) const FACT: &str = "operator_keys";

/// The most bytes of the operator keys file.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// The top-level key of the file that holds the groups.
const GROUPS: &str = "host-groups";

/// The one key a group's entry holds.
const KEYS: &str = "keys";

/// Why the operator keys file is refused. The texts name the flag, never the path or a key's
/// text; a key is named by its place in its group's array, counting from 1.
#[derive(Debug, thiserror::Error)]
pub enum OperatorKeysError {
    /// The file cannot be read (what reading it said), or is not UTF-8.
    #[error("--operator-keys-file: {0}")]
    Read(#[source] io::Error),
    /// The path is not a regular file.
    #[error("--operator-keys-file: not a regular file")]
    NotAFile,
    /// The file is longer than [`MAX_FILE_BYTES`].
    #[error(
        "--operator-keys-file: {bytes} bytes, more than the {MAX_FILE_BYTES} the file may hold"
    )]
    TooBig {
        /// Its size.
        bytes: u64,
    },
    /// The text is not TOML (the text of the parser, which holds none of the file's values).
    #[error("--operator-keys-file: not TOML: {0}")]
    NotToml(String),
    /// A top-level key other than `host-groups`.
    #[error("--operator-keys-file: the key {key:?} is not known; the file holds only host-groups")]
    UnknownKey {
        /// The key.
        key: String,
    },
    /// `host-groups` is not a table.
    #[error("--operator-keys-file: host-groups is not a table of host groups")]
    GroupsNotTable,
    /// A group's name is no host group name.
    #[error(
        "--operator-keys-file: {name:?} is not a host group name (1 to 32 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
    )]
    GroupName {
        /// The name.
        name: String,
    },
    /// A group's entry is not a table.
    #[error("--operator-keys-file: host group {group}: it is not a table holding keys")]
    GroupNotTable {
        /// The group.
        group: String,
    },
    /// A group's entry holds a key other than `keys`.
    #[error(
        "--operator-keys-file: host group {group}: the key {key:?} is not known; it holds only keys"
    )]
    UnknownGroupKey {
        /// The group.
        group: String,
        /// The key.
        key: String,
    },
    /// A group's entry has no `keys`.
    #[error("--operator-keys-file: host group {group}: keys is missing (write keys = [] for none)")]
    NoKeys {
        /// The group.
        group: String,
    },
    /// A group's `keys` is not an array.
    #[error("--operator-keys-file: host group {group}: keys is not an array of strings")]
    KeysNotArray {
        /// The group.
        group: String,
    },
    /// An element of a group's `keys` is not a string.
    #[error("--operator-keys-file: host group {group}: key {place} is not a string")]
    KeyNotText {
        /// The group.
        group: String,
        /// The element's place.
        place: usize,
    },
    /// An element of a group's `keys` is not a public key.
    #[error("--operator-keys-file: host group {group}: key {place}: {why}")]
    Key {
        /// The group.
        group: String,
        /// The key's place.
        place: usize,
        /// Why.
        why: KeyLineError,
    },
    /// A group's set breaks a rule of a set: too many keys, a key too long, one twice.
    #[error("--operator-keys-file: host group {group}: {why}")]
    Set {
        /// The group.
        group: String,
        /// Why.
        why: KeySetError,
    },
}

/// The keys of one host group, as they are sent: normalised, in the file's order, with their
/// digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Set {
    /// The keys, each one line with single spaces.
    pub keys: Vec<String>,
    /// The digest of `keys` (`hostkeeper_wire::operator_keys::digest`).
    pub digest: String,
}

impl Set {
    /// The set of `keys`, which keep the rules of a set.
    fn of(keys: Vec<String>) -> Self {
        Self {
            digest: digest(&keys),
            keys,
        }
    }

    /// The set as the answer to a heartbeat carries it.
    fn to_proto(&self) -> agent::OperatorKeys {
        agent::OperatorKeys {
            digest: self.digest.clone(),
            keys: self.keys.clone(),
        }
    }
}

/// What a daemon reported of the operator keys, as it is kept: the digest of the set in force
/// (empty while it holds none the coordinator sent), the last set it refused, and why its last try
/// failed, the texts cut to [`MAX_REASON`] bytes.
type Report = agent::OperatorKeysReport;

/// The groups' sets of the keys `text` holds.
fn parse(text: &str) -> Result<BTreeMap<String, Set>, OperatorKeysError> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| OperatorKeysError::NotToml(e.message().to_owned()))?;
    let mut sets = BTreeMap::new();
    for (name, value) in table {
        if name != GROUPS {
            return Err(OperatorKeysError::UnknownKey { key: name });
        }
        let toml::Value::Table(groups) = value else {
            return Err(OperatorKeysError::GroupsNotTable);
        };
        for (group, entry) in groups {
            let set = group_set(&group, entry)?;
            sets.insert(group, set);
        }
    }
    Ok(sets)
}

/// The set of the group `group` whose entry is `entry`.
fn group_set(group: &str, entry: toml::Value) -> Result<Set, OperatorKeysError> {
    let named = || group.to_owned();
    if group.parse::<GroupName>().is_err() {
        return Err(OperatorKeysError::GroupName { name: named() });
    }
    let toml::Value::Table(mut entry) = entry else {
        return Err(OperatorKeysError::GroupNotTable { group: named() });
    };
    if let Some(key) = entry.keys().find(|key| *key != KEYS) {
        return Err(OperatorKeysError::UnknownGroupKey {
            group: named(),
            key: key.clone(),
        });
    }
    let Some(listed) = entry.remove(KEYS) else {
        return Err(OperatorKeysError::NoKeys { group: named() });
    };
    let toml::Value::Array(listed) = listed else {
        return Err(OperatorKeysError::KeysNotArray { group: named() });
    };
    let mut keys = Vec::with_capacity(listed.len());
    for (index, item) in listed.into_iter().enumerate() {
        let place = index + 1;
        let toml::Value::String(line) = item else {
            return Err(OperatorKeysError::KeyNotText {
                group: named(),
                place,
            });
        };
        let key = public_key(&line).map_err(|why| OperatorKeysError::Key {
            group: named(),
            place,
            why,
        })?;
        keys.push(key);
    }
    check_set(&keys).map_err(|why| OperatorKeysError::Set {
        group: named(),
        why,
    })?;
    Ok(Set::of(keys))
}

/// The sets of the file at `path`: a regular file of at most [`MAX_FILE_BYTES`].
fn read(path: &Path) -> Result<BTreeMap<String, Set>, OperatorKeysError> {
    let meta = std::fs::metadata(path).map_err(OperatorKeysError::Read)?;
    if !meta.is_file() {
        return Err(OperatorKeysError::NotAFile);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(OperatorKeysError::TooBig { bytes: meta.len() });
    }
    let text = std::fs::read_to_string(path).map_err(OperatorKeysError::Read)?;
    parse(&text)
}

/// What one look at the operator keys file did.
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
    /// The file the sets came from; none without `--operator-keys-file`.
    source: Option<PathBuf>,
    /// The set of each group the file lists; none without the file.
    sets: RwLock<Option<BTreeMap<String, Set>>>,
    /// The last report of each host that announced [`FEATURE`], under a lock of its own, taken
    /// after the fleet's and holding nothing else.
    reports: Mutex<BTreeMap<HostId, Report>>,
}

/// The operator keys file's sets and the reports of the hosts, shared by the coordinator and the
/// tick that re-reads the file.
#[derive(Clone, Debug)]
pub struct OperatorKeys {
    inner: Arc<Inner>,
}

impl Default for OperatorKeys {
    /// No file: no host is ever sent keys.
    fn default() -> Self {
        Self::of(None, None)
    }
}

impl OperatorKeys {
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
    /// [`OperatorKeysError`]: the file cannot be read, is not a regular file, is over 256 KiB,
    /// is not UTF-8 or TOML of the shape, or holds a group name, a key or a set the rules above
    /// refuse.
    pub fn from_file(path: &Path) -> Result<Self, OperatorKeysError> {
        read(path).map(|sets| Self::of(Some(path.to_owned()), Some(sets)))
    }

    /// Whether a file was given: only then is any host sent keys.
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
    /// list it (so a group taken out of the file takes the keys away).
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
                    "the operator keys file changed: its keys are in use"
                );
                Reload::Applied
            }
            Err(why) => {
                tracing::warn!(%why, "the operator keys file was not read again: the keys in use stay");
                Reload::Rejected
            }
        }
    }

    /// The last report of the host `id`; none for a host that never announced the feature.
    #[must_use]
    pub fn report_of(&self, id: &HostId) -> Option<agent::OperatorKeysReport> {
        self.inner
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
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
/// 64 lower-case hex characters, or a refusal whose digest is no digest or whose reason is empty.
/// The reason and the failure are cut to [`MAX_REASON`] bytes at a character boundary.
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
            Some(agent::OperatorKeysRefusal {
                digest: refusal.digest.clone(),
                reason: cut(&refusal.reason, MAX_REASON).to_owned(),
            })
        }
    };
    Some(Report {
        digest: said.digest.clone(),
        refused,
        failed: cut(&said.failed, MAX_REASON).to_owned(),
    })
}

/// Whether `report` holds the set of `digest` in force, or refused: it is not sent again.
fn holds(report: &Report, digest: &str) -> bool {
    report.digest == digest || report.refused.as_ref().is_some_and(|r| r.digest == digest)
}

/// Whether a heartbeat over gRPC announces [`FEATURE`].
fn announces(request: &agent::HeartbeatRequest) -> bool {
    request.features.iter().any(|f| f == FEATURE)
}

impl Coordinator {
    /// The same coordinator over the operator keys `keys` instead of none.
    #[must_use]
    pub fn with_operator_keys(mut self, keys: OperatorKeys) -> Self {
        self.operator_keys = keys;
        self
    }

    /// The operator keys file's sets and the hosts' reports.
    #[must_use]
    pub fn operator_keys(&self) -> &OperatorKeys {
        &self.operator_keys
    }

    /// Notes what a heartbeat over gRPC from `who` said of the operator keys, when its daemon
    /// announced [`FEATURE`]: the report, or the empty one when it sent none; a report that breaks
    /// a rule is counted and logged as a dropped fact, and the one read before is kept.
    pub(crate) fn hear_operator_keys(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        if !announces(request) {
            return;
        }
        let report = match &request.operator_keys {
            None => Some(Report::default()),
            Some(said) => kept(said),
        };
        match report {
            Some(report) => self.operator_keys.note(&who.host_id, report),
            None => {
                self.note_extras_dropped(who, "the operator keys report breaks a rule", &[FACT]);
            }
        }
    }

    /// The operator keys the answer to a heartbeat over gRPC from `who` carries, if it carries
    /// them: when the daemon announced [`FEATURE`], this coordinator advertises it and was given
    /// a file, and the host's report holds neither the digest of the set in force nor refused.
    /// The set is that of the host's group, as the module says, read under the fleet's lock so
    /// that a move refused after it wrote the host's new group is never seen. A group that cannot
    /// be read gives none: the next heartbeat tries again.
    pub(crate) fn operator_keys_for(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) -> Option<agent::OperatorKeys> {
        if !announces(request)
            || !self.features.iter().any(|f| f == FEATURE)
            || !self.operator_keys.is_given()
        {
            return None;
        }
        // Under the fleet's lock, which a move holds from the write of the host's group to the
        // end of its commit, as `host_facts` reads it: a host is never sent the keys of a group
        // a refused move named.
        let group = {
            let _fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
            self.approved_group(who)?
        };
        let set = self.operator_keys.to_send(&group);
        let held = self
            .operator_keys
            .report_of(&who.host_id)
            .unwrap_or_default();
        (!holds(&held, &set.digest)).then(|| set.to_proto())
    }

    /// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id` (`fleet` is the
    /// fleet as they read it), with the last report of its daemon, in its heartbeat, when the
    /// daemon announced [`FEATURE`].
    pub(crate) fn show_operator_keys(&self, fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
        if !fleet.announced(id, FEATURE) {
            return;
        }
        if let Some(report) = self.operator_keys.report_of(id) {
            host.heartbeat.get_or_insert_default().operator_keys = Some(report);
        }
    }
}

#[cfg(test)]
#[path = "operator_keys_tests.rs"]
pub(crate) mod tests;
