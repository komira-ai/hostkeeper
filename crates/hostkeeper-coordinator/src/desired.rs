//! The desired state of each service (`docs/design/host-updates.md`), and its file.
//!
//! The coordinator keeps three files: `desired.json`, which this module reads and writes and
//! which holds the desired state of each service; the operations file (`crate::ops`,
//! `crate::ops_file`), which holds operator actions, version marks and the audit log; and the
//! halts file beside it, which holds halt reports and clear generations.
//!
//! CI sets a desired state with one call: the version every host should run, and two maps the
//! file format carries, how many hosts of a role may update at once and how many of a role must
//! stay up. Both are kept and printed as CI sent them, and the update plan of a service reads
//! them: the hosts of a stage update at once up to a role's `max_updating`, and never so that
//! fewer than its `min_up` stay up ([`crate::stages`]); the convergence needs no number of
//! hosts up. The store keeps one such state per service name; a service must be one
//! the rollout file defines (`crate::services`). The file is small JSON, replaced whole by a
//! temporary file, fsync, rename and a directory fsync, so a crash leaves the old state or the
//! new one. A directory fsync that fails after the rename leaves the new state in place
//! ([`StoreError::NotSynced`]). While the legacy service (the coordinator's `--legacy-service`)
//! is the only service the file is the single state itself (the format earlier builds read and
//! wrote, so a downgrade keeps working); a file with `{"services": {name: state}}` is the map.
//! A file that cannot be read back is a refusal to start: the coordinator does not guess what
//! an operator meant.
//!
//! A desired state may name the digest of its version's package (`release_digest`), or the
//! record of the version's release index (`release_index`: the index's digest and the package
//! digest of each platform it lists, [`crate::release_index`]), never both; and each service's
//! state in the file carries the registry of every version CI has set with a release
//! (`digests`, [`crate::registry`]: a package digest, or the record of an index), written in the
//! same replacement of the file as the state, so a crash leaves both or neither. The keys are
//! written only when they hold something: a farm whose CI sends no digest keeps the bytes it
//! had, which earlier builds read, and a farm that sets no index never has `release_index`.
//! Earlier builds refuse a file that has the key `release_index` or `digests` (their reader
//! refuses a key it does not know), so a coordinator rolled back below this one refuses to
//! start rather than run without the digests. A request never writes the record: the keys
//! `release_index` and `release_index_digest` of a set are the index's text and its SHA-256,
//! which are checked and turned into the record ([`Desired::parse_request_noting`]).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_wire::version::Version;
use hostkeeper_wire::{Platform, RoleName};

use serde::{Deserialize, Serialize};

use crate::registry::{Digest, IndexRecord, Package, Registry, RegistryError, Release};
use crate::release_index;
use crate::releases::Release as Kept;
use crate::services::Services;
use crate::store::{BackendError, Commit, Files, Shared};
use crate::write_record::{WriteAsk, WriteError, WriteId, WriteRecord};

/// The most any count may be: a typo's order of magnitude, not a plan.
pub const MAX_COUNT: u32 = 10_000;

/// The role names the desired-state file format carries (part of the file format, not roles of
/// any service): the `max_updating` keys the format fills in on read, with the defaults 1, 1,
/// 2 and 2, and writes first, in this order. A role that is not one of these is written after
/// them, by name.
pub const FORMAT_ROLES: [&str; 4] = ["frontend", "storage", "daemon", "mac-daemon"];

/// The `min_up` keys the file format names in every state it writes, 0 when the state sets none,
/// first and in this order (part of the file format: earlier builds need both).
pub const FORMAT_MIN_UP: [&str; 2] = ["storage", "frontend"];

/// Whether `name` can name a role: the grammar of a role name ([`RoleName`]'s parse), which
/// is the one place that says.
#[must_use]
pub fn is_role_name(name: &str) -> bool {
    name.parse::<RoleName>().is_ok()
}

/// A number per role name: the limit on hosts updating at once, or the minimum that stays up, as
/// the desired-state file carries them. They are kept and printed, and the update plan of a
/// service reads them ([`crate::stages`]).
///
/// A role the map does not name has the default of the question asked ([`Self::max_updating_of`],
/// [`Self::min_up_of`]). The defaults are for a role a service definition names: whether a role
/// is run at all is the definition's to say, never this map's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct RoleLimits(BTreeMap<String, u32>);

impl<const N: usize> From<[(&str, u32); N]> for RoleLimits {
    fn from(pairs: [(&str, u32); N]) -> Self {
        Self(pairs.map(|(role, n)| (role.to_owned(), n)).into())
    }
}

impl RoleLimits {
    /// The limits of a state that says none: one frontend and one storage host, two daemons
    /// and two Macs at a time (the format's roles, [`FORMAT_ROLES`]).
    #[must_use]
    pub fn max_updating_default() -> Self {
        Self(
            FORMAT_ROLES
                .into_iter()
                .map(|role| (role.to_owned(), default_max_updating(role)))
                .collect(),
        )
    }

    /// What the map says of `role`, if it names it.
    #[must_use]
    pub fn get(&self, role: &str) -> Option<u32> {
        self.0.get(role).copied()
    }

    /// Every role the map names with its number, in role-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.0.iter().map(|(role, n)| (role.as_str(), *n))
    }

    /// Sets the number of `role`.
    pub fn set(&mut self, role: &str, n: u32) {
        self.0.insert(role.to_owned(), n);
    }

    /// How many hosts of `role` may update at once: the map's number, else the default (2 for
    /// the daemon roles, 1 for any other).
    #[must_use]
    pub fn max_updating_of(&self, role: &str) -> u32 {
        self.get(role).unwrap_or_else(|| default_max_updating(role))
    }

    /// How many hosts of `role` must stay up: the map's number, else none.
    #[must_use]
    pub fn min_up_of(&self, role: &str) -> u32 {
        self.get(role).unwrap_or(0)
    }

    /// The entries: the names in `legacy` first, in that order, then the others by name.
    fn ordered<'a>(&'a self, legacy: &[&'a str]) -> Vec<(&'a str, u32)> {
        let first = legacy
            .iter()
            .filter_map(|role| self.get(role).map(|n| (*role, n)));
        let rest = self
            .0
            .iter()
            .filter(|(role, _)| !legacy.contains(&role.as_str()))
            .map(|(role, n)| (role.as_str(), *n));
        first.chain(rest).collect()
    }

    /// Drops the limits for roles that are neither in `legacy` nor accepted by `keep`, and
    /// returns their names.
    fn retain(&mut self, legacy: &[&str], keep: impl Fn(&str) -> bool) -> Vec<String> {
        let mut dropped = Vec::new();
        self.0.retain(|role, _| {
            let kept = legacy.contains(&role.as_str()) || keep(role);
            if !kept {
                dropped.push(role.clone());
            }
            kept
        });
        dropped
    }
}

const fn default_max_updating(role: &str) -> u32 {
    match role.as_bytes() {
        b"daemon" | b"mac-daemon" => 2,
        _ => 1,
    }
}

/// The limits in the order the format writes them.
fn write_max_updating<S: serde::Serializer>(limits: &RoleLimits, s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(limits.ordered(&FORMAT_ROLES))
}

/// The minimums in the order the format writes them. The format requires `storage` and
/// `frontend`, so a state that names neither writes them as 0, which is what none means.
fn write_min_up<S: serde::Serializer>(limits: &RoleLimits, s: S) -> Result<S::Ok, S::Error> {
    s.collect_map(limits.min_up_written().ordered(&FORMAT_MIN_UP))
}

impl RoleLimits {
    /// The minimums as the format writes them: `storage` and `frontend` named, 0 when not set.
    pub(crate) fn min_up_written(&self) -> RoleLimits {
        let mut written = self.clone();
        for role in FORMAT_MIN_UP {
            written.0.entry(role.to_owned()).or_insert(0);
        }
        written
    }
}

/// The limits as written, with the format's four roles filled in with their defaults.
fn read_max_updating<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RoleLimits, D::Error> {
    let mut limits = RoleLimits::deserialize(d)?;
    for (role, n) in RoleLimits::max_updating_default().0 {
        limits.0.entry(role).or_insert(n);
    }
    Ok(limits)
}

/// The desired state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Desired {
    /// The version every host should run. An older one than the hosts run is a rollback.
    pub version: Version,
    /// The per-role limit on hosts updating at once. Kept, read and written as before so
    /// older states load and older coordinators read this one's. The hosts of a plan's stage
    /// update in parallel up to it for their role ([`crate::stages`]).
    #[serde(
        default = "RoleLimits::max_updating_default",
        deserialize_with = "read_max_updating",
        serialize_with = "write_max_updating"
    )]
    pub max_updating: RoleLimits,
    /// The minimum number of hosts of a role that stay up. Kept, read and written as before. A
    /// plan orders no host that would leave fewer live hosts of its role up
    /// ([`crate::stages`]); the convergence needs no number of hosts up.
    #[serde(serialize_with = "write_min_up")]
    pub min_up: RoleLimits,
    /// The digest of the package of `version`, as CI set it ([`crate::registry`]); none when CI
    /// sent none. Written only when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_digest: Option<Digest>,
    /// The record of the release index of `version`, as CI set it: each host is ordered the
    /// package the index lists for its platform. None when CI sent no index; never with a
    /// `release_digest`. Written only when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_index: Option<IndexRecord>,
}

/// Why a desired state is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DesiredError {
    /// Not JSON of the shape (a missing field, an unknown one, a version the rule refuses).
    #[error("the desired state is malformed: {0}")]
    Malformed(String),
    /// A count is over [`MAX_COUNT`].
    #[error("a count is over {MAX_COUNT}")]
    Count,
    /// The `service` is not one the coordinator manages.
    #[error("no such service")]
    UnknownService,
    /// The file holds one desired state, the legacy service's, and no legacy service is set.
    #[error("the file holds one desired state and no legacy service is set")]
    NoLegacyService,
    /// The registry of digests of a service is refused.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// A release index that cannot be taken as the version's: see [`crate::release_index`].
    #[error("index_invalid: {0}")]
    IndexInvalid(&'static str),
    /// The text of a release index does not have the SHA-256 the set named.
    #[error("index_digest_mismatch: the release index does not have the digest named")]
    IndexDigestMismatch,
    /// The set names `if_generation` and it is not the service's generation now.
    #[error("generation_conflict: the service's generation is {now}, not {asked}")]
    GenerationConflict {
        /// The generation the set named.
        asked: u64,
        /// The generation now.
        now: u64,
    },
    /// The set names `if_generation` or a write id, and the store keeps neither (the
    /// desired-state file).
    #[error("the desired-state store keeps no generation or write id")]
    Unrecorded,
}

impl DesiredError {
    /// The closed reason token of a refusal of the release index, which the answer says
    /// (`index_invalid`, `index_digest_mismatch`); none for every other error.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self {
            Self::IndexInvalid(_) => Some("index_invalid"),
            Self::IndexDigestMismatch => Some("index_digest_mismatch"),
            Self::Malformed(_)
            | Self::Count
            | Self::UnknownService
            | Self::NoLegacyService
            | Self::Registry(_)
            | Self::GenerationConflict { .. }
            | Self::Unrecorded => None,
        }
    }
}

impl Desired {
    /// The desired state in `bytes`: the single state of the state file.
    ///
    /// # Errors
    /// [`DesiredError`].
    pub fn parse(bytes: &[u8]) -> Result<Self, DesiredError> {
        let desired: Self =
            serde_json::from_slice(bytes).map_err(|e| DesiredError::Malformed(e.to_string()))?;
        desired.checked()?;
        Ok(desired)
    }

    /// The service and desired state of a `POST /v1/desired` body: the state's fields and an
    /// optional `service` (absent means `legacy`, the coordinator's legacy service), for the
    /// services of `services`.
    ///
    /// The body is the one every service gets; the map may be omitted. A limit for a role the
    /// service does not have is dropped, so that one body does for all services; the format's
    /// four roles' keys stay as sent, the form earlier builds read.
    ///
    /// # Errors
    /// [`DesiredError`]; [`DesiredError::UnknownService`] for a service `services` lacks.
    pub fn parse_request_for(
        bytes: &[u8],
        services: &Services,
        legacy: Option<&str>,
    ) -> Result<(String, Self), DesiredError> {
        Self::parse_request_noting(bytes, services, legacy)
            .map(|(service, desired, _)| (service, desired))
    }

    /// [`Self::parse_request_for`], also returning the names of the roles whose limits were
    /// dropped because the service does not have them (sorted, each once), which it logs as a
    /// warning. A body without `service` is the legacy service's; with no legacy service it is
    /// malformed.
    ///
    /// # Errors
    /// [`DesiredError`], as [`Self::parse_request_for`].
    pub fn parse_request_noting(
        bytes: &[u8],
        services: &Services,
        legacy: Option<&str>,
    ) -> Result<(String, Self, Vec<String>), DesiredError> {
        let malformed = |e: serde_json::Error| DesiredError::Malformed(e.to_string());
        let mut fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(bytes).map_err(malformed)?;
        let service = match fields.remove("service") {
            None => legacy
                .ok_or_else(|| DesiredError::Malformed("service is required".to_owned()))?
                .to_owned(),
            Some(serde_json::Value::String(name)) => name,
            Some(_) => return Err(DesiredError::Malformed("service is not text".to_owned())),
        };
        let def = services.get(&service).ok_or(DesiredError::UnknownService)?;
        // The index's text and digest are taken out before the strict read, which knows the
        // record's key and must never be handed one: a request cannot write the record.
        let index_text = fields.remove("release_index");
        let index_digest = fields.remove("release_index_digest");
        // `min_up` is optional for every service: it is 0 when CI sends none, and the file
        // written names the format's two keys either way (`write_min_up`).
        fields
            .entry("min_up")
            .or_insert_with(|| serde_json::json!({}));
        let mut desired: Self = serde_json::from_value(fields.into()).map_err(malformed)?;
        desired.checked()?;
        desired.refuse_unnamed(services)?;
        desired.release_index = release_index::checked(
            index_text.as_ref(),
            index_digest.as_ref(),
            desired.release_digest.is_some(),
            &service,
            &desired.version,
        )?;
        let has = |role: &str| def.roles.contains(role);
        let mut dropped = desired.max_updating.retain(&FORMAT_ROLES, has);
        dropped.extend(desired.min_up.retain(&FORMAT_MIN_UP, has));
        dropped.sort();
        dropped.dedup();
        if !dropped.is_empty() {
            tracing::warn!(%service, roles = ?dropped, "dropped limits for roles the service does not have");
        }
        Ok((service, desired, dropped))
    }

    /// Refuses a limits key that is neither a legacy name nor a role of any service, so that a
    /// typo is an error and not a limit silently lost; one body still serves every service.
    fn refuse_unnamed(&self, services: &Services) -> Result<(), DesiredError> {
        let maps = [
            ("max_updating", &self.max_updating, &FORMAT_ROLES[..]),
            ("min_up", &self.min_up, &FORMAT_MIN_UP[..]),
        ];
        for (map, limits, legacy) in maps {
            let unnamed = limits.0.keys().find(|role| {
                !legacy.contains(&role.as_str())
                    && !services.defs().any(|def| def.roles.contains(role.as_str()))
            });
            if let Some(role) = unnamed {
                return Err(DesiredError::Malformed(format!(
                    "{map}: {role} is no role of any service"
                )));
            }
        }
        Ok(())
    }

    /// What CI registered for the version: the package digest, or the release index.
    #[must_use]
    pub fn release(&self) -> Option<Release> {
        self.release_digest
            .clone()
            .map(Release::Package)
            .or_else(|| self.release_index.clone().map(Release::Index))
    }

    /// The state with `release` as its package digest or release index, and no other.
    pub fn set_release(&mut self, release: Option<Release>) {
        self.release_digest = None;
        self.release_index = None;
        match release {
            Some(Release::Package(digest)) => self.release_digest = Some(digest),
            Some(Release::Index(record)) => self.release_index = Some(record),
            None => {}
        }
    }

    /// The package a host of `platform` is ordered of the version; none when CI set no release
    /// or the release index lists no package for the platform.
    #[must_use]
    pub fn package_for(&self, platform: Platform) -> Option<Package> {
        self.release()
            .and_then(|release| release.package_for(platform))
    }

    /// Whether the version has a release index that lists no package for `platform`: a host of
    /// that platform is not ordered the version.
    #[must_use]
    pub fn lacks_package_for(&self, platform: Platform) -> bool {
        self.release_index
            .as_ref()
            .is_some_and(|record| record.package_for(platform).is_none())
    }

    /// Whether the state is acceptable: every limit key a role name, every count within
    /// [`MAX_COUNT`], and no package digest beside a release index.
    pub(crate) fn checked(&self) -> Result<(), DesiredError> {
        if self.release_digest.is_some() && self.release_index.is_some() {
            return Err(DesiredError::Malformed(
                "release_digest and release_index are exclusive".to_owned(),
            ));
        }
        let limits = [&self.max_updating, &self.min_up];
        for role in limits.iter().flat_map(|l| l.0.keys()) {
            if !is_role_name(role) {
                return Err(DesiredError::Malformed(format!(
                    "{role:?} is not a role name"
                )));
            }
        }
        if limits
            .iter()
            .flat_map(|l| l.0.values())
            .any(|&n| n > MAX_COUNT)
        {
            return Err(DesiredError::Count);
        }
        Ok(())
    }
}

/// What the store holds: the desired state of each service, and the registry of digests of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    /// The desired state, by service.
    pub(crate) desired: BTreeMap<String, Desired>,
    /// The registry of digests, by service; a service with none is not listed.
    pub(crate) digests: BTreeMap<String, Registry>,
    /// How each service's writes are recorded ([`crate::write_record`]), kept only over a backend
    /// that keeps them; a service never written is not listed.
    pub(crate) records: BTreeMap<String, WriteRecord>,
    /// The releases each service's versions were set with ([`crate::releases`]), kept beside the
    /// state and its registry, never in their file; a service with none is not listed.
    pub(crate) releases: crate::releases::Books,
}

impl Held {
    fn digests_of(&self, service: &str) -> Registry {
        self.digests.get(service).cloned().unwrap_or_default()
    }

    /// The record of `service`'s writes: the default for one never written.
    fn record_of(&self, service: &str) -> WriteRecord {
        self.records.get(service).cloned().unwrap_or_default()
    }
}

#[path = "desired_file.rs"]
mod file;
pub(crate) use file::{encode, parse_file};

/// Why the state file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A file operation failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The new file is in place, but the directory holding it could not be synced: a crash now
    /// may bring the old file back. The state is written, not durable; writing it again is
    /// safe.
    #[error("{path}: written, but the directory was not synced: {source}")]
    NotSynced {
        /// The directory.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The file holds no desired state this build accepts.
    #[error("{path}: {source}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        source: DesiredError,
    },
}

/// Syncs a directory to disk.
pub(crate) type SyncDir = fn(&Path) -> io::Result<()>;

/// Whether a set changed the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// The state and the registry were already what the set asked for: nothing was written.
    Unchanged,
    /// The file was replaced.
    Written,
}

/// The desired state, in memory and in its backend ([`crate::store`]).
pub struct DesiredStore {
    backend: Shared,
    current: Mutex<Held>,
    /// Held across a whole replacement, so that writers wait for each other and not readers:
    /// `current` is only locked to read or to swap, never across the backend's commit.
    writing: Mutex<()>,
}

impl std::fmt::Debug for DesiredStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesiredStore").finish_non_exhaustive()
    }
}

/// A desired state made and not yet in memory: what [`DesiredStore::stage`] answers, for a change
/// that commits it with another part ([`crate::force`]). The store's writers wait while it lives.
/// What a write records beside the state: its version's digest or index in the registry, its
/// release ([`crate::releases`]), and its write id.
struct Staging {
    digests: Option<(Version, Release)>,
    release: Option<Arc<Kept>>,
    id: Option<WriteId>,
}

impl Staging {
    /// A write of `digests` alone.
    const fn digests(digests: Option<(Version, Release)>) -> Self {
        Self {
            digests,
            release: None,
            id: None,
        }
    }
}

pub(crate) struct Staged<'a> {
    store: &'a DesiredStore,
    _writer: MutexGuard<'a, ()>,
    pub(crate) before: Held,
    pub(crate) after: Held,
}

impl Staged<'_> {
    /// The backend the store commits to.
    pub(crate) fn backend(&self) -> &Shared {
        &self.store.backend
    }

    /// The part of a commit it is: none when it changes nothing.
    pub(crate) fn part(&self) -> Option<(&Held, &Held)> {
        (self.before != self.after).then_some((&self.before, &self.after))
    }

    /// Takes it into memory: its commit is durable.
    pub(crate) fn finish(self) {
        *self.store.lock() = self.after;
    }
}

pub(crate) fn io_at(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The directory a file is in: `.` for a bare file name.
pub(crate) fn dir_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

impl DesiredStore {
    /// The store of the file at `path`: its state if it exists, none if not. A file in the
    /// single-state form is the state of `legacy`; with no legacy service it is refused
    /// ([`DesiredError::NoLegacyService`]).
    ///
    /// # Errors
    /// [`StoreError`]: the file exists and cannot be read, or holds no desired state.
    pub fn open(path: &Path, legacy: Option<&str>) -> Result<Self, StoreError> {
        let files = Files::default().with_desired(path, legacy);
        let held = files.load_desired()?;
        Ok(Self::over(Arc::new(files), held))
    }

    /// The store of `held`, as loaded from `backend`, to which its changes are committed.
    pub(crate) fn over(backend: Shared, held: Held) -> Self {
        Self {
            backend,
            current: Mutex::new(held),
            writing: Mutex::new(()),
        }
    }

    /// The desired state of `service` now, if one was ever set.
    #[must_use]
    pub fn get(&self, service: &str) -> Option<Desired> {
        self.lock().desired.get(service).cloned()
    }

    /// The desired state of every service, by name.
    #[must_use]
    pub fn all(&self) -> BTreeMap<String, Desired> {
        self.lock().desired.clone()
    }

    /// The registry of digests of `service` now: every version CI has set with a digest.
    #[must_use]
    pub fn digests(&self, service: &str) -> Registry {
        self.lock().digests_of(service)
    }

    /// [`Self::open`], with `sync_dir` as the directory sync of the file, to fail the sync after
    /// the rename.
    #[cfg(test)]
    pub(crate) fn open_syncing(
        path: &Path,
        legacy: Option<&str>,
        sync_dir: SyncDir,
    ) -> Result<Self, StoreError> {
        let files = Files::default()
            .with_desired(path, legacy)
            .with_sync_dir(sync_dir);
        let held = files.load_desired()?;
        Ok(Self::over(Arc::new(files), held))
    }

    /// Replaces the desired state of `service` (the others stay), in the file first and in
    /// memory only if the file holds it. The registry of digests stays as it is.
    ///
    /// # Errors
    /// [`StoreError::Io`]: the file could not be replaced; the old state stands.
    /// [`StoreError::NotSynced`]: the file holds the new state, so memory does too; the caller
    /// still reports the failure, and setting the same state again is safe.
    pub fn set(&self, service: &str, desired: Desired) -> Result<(), StoreError> {
        self.replace(service, desired, None, &|_| true).map(drop)
    }

    /// Sets the desired state of `service` as [`Self::set`] does, and, when it names a release
    /// (a package digest or a release index), records its version and release in the service's
    /// registry ([`Registry::record`], which
    /// drops past the cap only a version `keep` does not keep), in the same replacement of the
    /// file. Over a backend that keeps write records ([`Backend::keeps_write_records`]) the
    /// service's generation is raised by one in the same commit, so every set is written;
    /// otherwise, when the state and the registry are already what is asked, nothing is written.
    ///
    /// # Errors
    /// [`StoreError`], as [`Self::set`]: on [`StoreError::Io`] neither the state nor the
    /// registry changed, in the file or in memory.
    ///
    /// [`Backend::keeps_write_records`]: crate::store::Backend::keeps_write_records
    pub fn record(
        &self,
        service: &str,
        desired: Desired,
        keep: impl Fn(&Version) -> bool,
    ) -> Result<Recorded, StoreError> {
        let release = desired
            .release()
            .map(|release| (desired.version.clone(), release));
        self.replace(service, desired, release, &keep)
    }

    /// [`Self::record`] for a write that asks `ask` of the store: with `if_generation`, only
    /// over that generation; with a write id, kept as the last write's; with `release`, kept as
    /// the release of the state's version in the same commit ([`crate::releases::record`], under
    /// the cap's `keep`). Answers the service's record after the write, none over a backend that
    /// keeps no records.
    ///
    /// # Errors
    /// [`WriteError`], as [`Self::admits`] (checked under the writers' lock, so no write lands
    /// between the check and the commit), or the store's; on either nothing changed.
    pub(crate) fn record_as(
        &self,
        service: &str,
        desired: Desired,
        keep: impl Fn(&Version) -> bool,
        ask: &WriteAsk,
        release: Option<Arc<Kept>>,
    ) -> Result<Option<WriteRecord>, WriteError> {
        let digests = desired
            .release()
            .map(|release| (desired.version.clone(), release));
        let release = Staging {
            digests,
            release,
            id: ask.write_id.clone(),
        };
        let staged = self.staged(service, desired, release, &keep);
        self.admit(&staged.before, service, ask)?;
        let record = staged.after.records.get(service).cloned();
        self.commit_staged(staged)?;
        Ok(record)
    }

    /// Whether a write of `service` that asks `ask` would be taken now.
    ///
    /// # Errors
    /// [`WriteError::Unrecorded`]: it asks for a precondition or a write id and the backend keeps
    /// no write records. [`WriteError::Conflict`]: its `if_generation` is not the service's
    /// generation now.
    pub(crate) fn admits(&self, service: &str, ask: &WriteAsk) -> Result<(), WriteError> {
        self.admit(&self.lock(), service, ask)
    }

    fn admit(&self, held: &Held, service: &str, ask: &WriteAsk) -> Result<(), WriteError> {
        if ask.needs_records() && !self.backend.keeps_write_records() {
            return Err(WriteError::Unrecorded);
        }
        ask.admits(held.record_of(service).generation)
    }

    /// Whether the backend keeps write records ([`crate::write_record`]).
    #[must_use]
    pub(crate) fn keeps_write_records(&self) -> bool {
        self.backend.keeps_write_records()
    }

    /// How the writes of `service` are recorded now: none when the backend keeps no records,
    /// and generation 0 with nothing else before the first write.
    #[must_use]
    pub(crate) fn write_record(&self, service: &str) -> Option<WriteRecord> {
        self.backend
            .keeps_write_records()
            .then(|| self.lock().record_of(service))
    }

    /// The one replacement of the file: the state of `service`, and the version and digest to
    /// record, if any, with the cap's `keep`.
    fn replace(
        &self,
        service: &str,
        desired: Desired,
        release: Option<(Version, Release)>,
        keep: &dyn Fn(&Version) -> bool,
    ) -> Result<Recorded, StoreError> {
        let staged = self.staged(service, desired, Staging::digests(release), keep);
        self.commit_staged(staged)
    }

    /// Commits `staged` and takes it into memory; nothing when it changes nothing.
    fn commit_staged(&self, staged: Staged<'_>) -> Result<Recorded, StoreError> {
        let Some(part) = staged.part() else {
            return Ok(Recorded::Unchanged);
        };
        let commit = Commit {
            desired: Some(part),
            ..Commit::default()
        };
        match self
            .backend
            .commit(&commit)
            .map_err(BackendError::into_store)
        {
            Ok(()) => {}
            Err(e @ StoreError::NotSynced { .. }) => {
                staged.finish();
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        staged.finish();
        Ok(Recorded::Written)
    }

    /// The state [`Self::record`] would make, staged under the writers' lock and not committed:
    /// for a force, which commits it with its operations entries.
    pub(crate) fn stage(
        &self,
        service: &str,
        desired: Desired,
        keep: impl Fn(&Version) -> bool,
    ) -> Staged<'_> {
        let release = desired
            .release()
            .map(|release| (desired.version.clone(), release));
        self.staged(service, desired, Staging::digests(release), &keep)
    }

    /// The state with `desired` set for `service` and what `staging` records with it: its
    /// version's digest or index in the registry, its release beside it; over a backend that
    /// keeps write records, with the service's record raised by one write of `staging`'s id.
    fn staged(
        &self,
        service: &str,
        desired: Desired,
        staging: Staging,
        keep: &dyn Fn(&Version) -> bool,
    ) -> Staged<'_> {
        let Staging {
            digests: release,
            release: kept,
            id,
        } = staging;
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.lock().clone();
        let mut after = before.clone();
        if self.backend.keeps_write_records() {
            let record = before
                .record_of(service)
                .after(before.desired.get(service), &desired, id);
            after.records.insert(service.to_owned(), record);
        }
        if let Some(kept) = kept {
            let book = after.releases.entry(service.to_owned()).or_default();
            crate::releases::record(book, &desired.version, kept, keep);
        }
        after.desired.insert(service.to_owned(), desired);
        if let Some((version, release)) = release {
            let mut registry = after.digests_of(service);
            registry.record(&version, release, keep);
            after.digests.insert(service.to_owned(), registry);
        }
        Staged {
            store: self,
            _writer: writer,
            before,
            after,
        }
    }

    /// The release `version` of `service` was set with, if any.
    #[must_use]
    pub(crate) fn release(&self, service: &str, version: &str) -> Option<Arc<Kept>> {
        self.lock()
            .releases
            .get(service)
            .and_then(|book| book.get(version))
            .cloned()
    }

    /// Takes the desired state of a commit of unknown outcome that `landing` found landed into
    /// memory, with what memory changed since laid over it (`docs/design/coordinator-storage.md`,
    /// section 4.4).
    pub(crate) fn take_landed(&self, landing: &crate::store::Landing) {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = landing.desired_over(&self.lock());
        if let Some(taken) = taken {
            *self.lock() = taken;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Replaces the file at `path` with `text`, whole: a temporary file beside it, created new
/// and owner-only, fsynced, renamed over `path`, and the directory fsynced, so a crash leaves
/// the old file or the new one. A leftover temporary file of a crash is replaced, never
/// followed.
///
/// # Errors
/// [`StoreError::Io`]: before the rename; the old file stands. [`StoreError::NotSynced`]: the
/// rename was made and the directory could not be synced; the new file is in place.
pub(crate) fn write_whole(path: &Path, text: &[u8]) -> Result<(), StoreError> {
    write_whole_syncing(path, text, sync_dir)
}

/// The directory fsync of [`write_whole`].
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir).and_then(|d| d.sync_all())
}

/// [`write_whole`] with `sync` as the directory fsync.
pub(crate) fn write_whole_syncing(
    path: &Path,
    text: &[u8],
    sync: SyncDir,
) -> Result<(), StoreError> {
    let mut temp = path.to_path_buf().into_os_string();
    temp.push(".new");
    let temp = PathBuf::from(temp);
    match fs::remove_file(&temp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_at(&temp)(e)),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(io_at(&temp))?;
    file.write_all(text).map_err(io_at(&temp))?;
    file.sync_all().map_err(io_at(&temp))?;
    fs::rename(&temp, path).map_err(io_at(path))?;
    let dir = dir_of(path);
    sync(dir).map_err(|source| StoreError::NotSynced {
        path: dir.to_owned(),
        source,
    })
}
