//! The desired state of each service (`docs/design/host-updates.md`), and its file.
//!
//! The coordinator keeps three files: `desired.json`, which this module reads and writes and
//! which holds the desired state of each service; the operations file (`crate::ops`,
//! `crate::ops_file`), which holds operator actions, version marks and the audit log; and the
//! halts file beside it, which holds halt reports and clear generations.
//!
//! CI sets a desired state with one call: the version every host should run, and two maps the
//! file format carries, how many hosts of a kind may update at once and how many of a role must
//! stay up. Both are read, kept and printed as CI sent them, and no decision reads either: one
//! update is live at a time across every service ([`crate::decide`]), and the convergence needs
//! no number of hosts up. The store keeps one such state per service name; a service must be one
//! the rollout file defines (`crate::services`). The file is small JSON, replaced whole by a
//! temporary file, fsync, rename and a directory fsync, so a crash leaves the old state or the
//! new one. A directory fsync that fails after the rename leaves the new state in place
//! ([`StoreError::NotSynced`]). While the legacy service (the coordinator's `--legacy-service`)
//! is the only service the file is the single state itself (the format earlier builds read and
//! wrote, so a downgrade keeps working); a file with `{"services": {name: state}}` is the map.
//! A file that cannot be read back is a refusal to start: the coordinator does not guess what
//! an operator meant.
//!
//! A desired state may name the digest of its version's package (`release_digest`), and each
//! service's state in the file carries the registry of every version CI has set with a digest
//! (`digests`, [`crate::registry`]), written in the same replacement of the file as the state,
//! so a crash leaves both or neither. Both keys are written only when they hold something: a
//! farm whose CI sends no digest keeps the bytes it had, which earlier builds read. Earlier
//! builds refuse a file that has either key (their reader refuses a key it does not know), so a
//! coordinator rolled back below this one refuses to start rather than run without the
//! digests.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hostkeeper_wire::RoleName;
use hostkeeper_wire::version::Version;

use serde::{Deserialize, Serialize};

use crate::registry::{Digest, Registry, RegistryError};
use crate::services::Services;

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
/// the desired-state file carries them. They are kept and printed; no decision reads them.
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
    let mut written = limits.clone();
    for role in FORMAT_MIN_UP {
        written.0.entry(role.to_owned()).or_insert(0);
    }
    s.collect_map(written.ordered(&FORMAT_MIN_UP))
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
    /// older states load and older coordinators read this one's; no decision reads it: one
    /// update is live at a time across every service ([`crate::decide`]).
    #[serde(
        default = "RoleLimits::max_updating_default",
        deserialize_with = "read_max_updating",
        serialize_with = "write_max_updating"
    )]
    pub max_updating: RoleLimits,
    /// The minimum number of hosts that stay up. Kept, read and written as before; nothing
    /// decides on it, and the convergence needs no number of hosts up.
    #[serde(serialize_with = "write_min_up")]
    pub min_up: RoleLimits,
    /// The digest of the package of `version`, as CI set it ([`crate::registry`]); none when CI
    /// sent none. Written only when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_digest: Option<Digest>,
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
        // `min_up` is optional for every service: nothing decides on it any more, and the file
        // written names the format's two keys either way (`write_min_up`).
        fields
            .entry("min_up")
            .or_insert_with(|| serde_json::json!({}));
        let mut desired: Self = serde_json::from_value(fields.into()).map_err(malformed)?;
        desired.checked()?;
        desired.refuse_unnamed(services)?;
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

    /// Whether the state is acceptable: every limit key a role name, every count within
    /// [`MAX_COUNT`].
    fn checked(&self) -> Result<(), DesiredError> {
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

/// What the file holds of one service: its desired state and its registry of digests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Held {
    desired: BTreeMap<String, Desired>,
    digests: BTreeMap<String, Registry>,
}

/// One service's state as the file writes it: the desired state, then its registry when it holds
/// any.
#[derive(Serialize)]
struct EntryOut<'a> {
    #[serde(flatten)]
    desired: &'a Desired,
    #[serde(skip_serializing_if = "Option::is_none")]
    digests: Option<&'a Registry>,
}

/// The state file in its map form, as written.
#[derive(Serialize)]
struct MapFileOut<'a> {
    services: BTreeMap<&'a str, EntryOut<'a>>,
}

/// The desired state and the registry of one service's entry in the file: the state's own keys,
/// read strictly, and `digests`.
fn parse_entry(value: serde_json::Value) -> Result<(Desired, Registry), DesiredError> {
    let malformed = |e: serde_json::Error| DesiredError::Malformed(e.to_string());
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(value).map_err(malformed)?;
    let digests = match fields.remove("digests") {
        Some(value) => Registry::read(value)?,
        None => Registry::default(),
    };
    let desired: Desired = serde_json::from_value(fields.into()).map_err(malformed)?;
    desired.checked()?;
    Ok((desired, digests))
}

/// The services in `bytes`: a map file, or the single state of earlier builds as the
/// legacy service's (`legacy`; with none set that file is refused).
fn parse_file(bytes: &[u8], legacy: Option<&str>) -> Result<Held, DesiredError> {
    let malformed = |e: serde_json::Error| DesiredError::Malformed(e.to_string());
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(malformed)?;
    let mut held = Held::default();
    let Some(services) = value.get("services").cloned() else {
        let name = legacy.ok_or(DesiredError::NoLegacyService)?;
        let (desired, digests) = parse_entry(value)?;
        held.insert(name, desired, digests);
        return Ok(held);
    };
    if let Some(other) = value
        .as_object()
        .and_then(|map| map.keys().find(|key| *key != "services"))
    {
        return Err(DesiredError::Malformed(format!("unknown field `{other}`")));
    }
    let services: BTreeMap<String, serde_json::Value> =
        serde_json::from_value(services).map_err(malformed)?;
    for (name, entry) in services {
        let (desired, digests) = parse_entry(entry)?;
        held.insert(&name, desired, digests);
    }
    Ok(held)
}

impl Held {
    fn insert(&mut self, name: &str, desired: Desired, digests: Registry) {
        self.desired.insert(name.to_owned(), desired);
        if !digests.is_empty() {
            self.digests.insert(name.to_owned(), digests);
        }
    }

    fn digests_of(&self, service: &str) -> Registry {
        self.digests.get(service).cloned().unwrap_or_default()
    }

    /// The entry of `name`, whose state is `desired`, as the file writes it.
    fn entry<'a>(&'a self, name: &str, desired: &'a Desired) -> EntryOut<'a> {
        EntryOut {
            desired,
            digests: self.digests.get(name).filter(|r| !r.is_empty()),
        }
    }
}

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

/// The desired state, in memory and in its file.
#[derive(Debug)]
pub struct DesiredStore {
    path: PathBuf,
    /// The legacy service: the one whose state alone is written as the single state.
    legacy: Option<String>,
    current: Mutex<Held>,
    /// Syncs the directory of the file after a rename.
    sync_dir: SyncDir,
    /// Held across a whole replacement, so that writers wait for each other and not readers:
    /// `current` is only locked to read or to swap, never across the file's fsyncs.
    writing: Mutex<()>,
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
        let current = match fs::read(path) {
            Ok(bytes) => parse_file(&bytes, legacy).map_err(|source| StoreError::Invalid {
                path: path.to_owned(),
                source,
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Held::default(),
            Err(e) => return Err(io_at(path)(e)),
        };
        let sync_dir: SyncDir = sync_dir;
        Ok(Self {
            path: path.to_owned(),
            legacy: legacy.map(str::to_owned),
            current: Mutex::new(current),
            sync_dir,
            writing: Mutex::new(()),
        })
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

    /// The store with `sync_dir` as its directory sync, to fail the sync after the rename.
    #[cfg(test)]
    pub(crate) fn with_sync_dir(mut self, sync_dir: SyncDir) -> Self {
        self.sync_dir = sync_dir;
        self
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

    /// Sets the desired state of `service` as [`Self::set`] does, and, when it names a digest,
    /// records its version and digest in the service's registry ([`Registry::record`], which
    /// drops past the cap only a version `keep` does not keep), in the same replacement of the
    /// file. When the state and the registry are already what is asked, nothing is written.
    ///
    /// # Errors
    /// [`StoreError`], as [`Self::set`]: on [`StoreError::Io`] neither the state nor the
    /// registry changed, in the file or in memory.
    pub fn record(
        &self,
        service: &str,
        desired: Desired,
        keep: impl Fn(&Version) -> bool,
    ) -> Result<Recorded, StoreError> {
        let digest = desired
            .release_digest
            .clone()
            .map(|digest| (desired.version.clone(), digest));
        self.replace(service, desired, digest, &keep)
    }

    /// The one replacement of the file: the state of `service`, and the version and digest to
    /// record, if any, with the cap's `keep`.
    fn replace(
        &self,
        service: &str,
        desired: Desired,
        digest: Option<(Version, Digest)>,
        keep: &dyn Fn(&Version) -> bool,
    ) -> Result<Recorded, StoreError> {
        let _writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.lock().clone();
        let mut next = before.clone();
        next.desired.insert(service.to_owned(), desired);
        if let Some((version, digest)) = digest {
            let mut registry = next.digests_of(service);
            registry.record(&version, digest, keep);
            next.digests.insert(service.to_owned(), registry);
        }
        if next == before {
            return Ok(Recorded::Unchanged);
        }
        match self.write(&next) {
            Ok(()) => {}
            Err(e @ StoreError::NotSynced { .. }) => {
                *self.lock() = next;
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        *self.lock() = next;
        Ok(Recorded::Written)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self, held: &Held) -> Result<(), StoreError> {
        // Only the legacy service is the single state of earlier builds, which
        // refuses the map form.
        let encoded = match held.desired.iter().collect::<Vec<_>>().as_slice() {
            [(name, only)] if Some(name.as_str()) == self.legacy.as_deref() => {
                serde_json::to_vec_pretty(&held.entry(name, only))
            }
            _ => serde_json::to_vec_pretty(&MapFileOut {
                services: held
                    .desired
                    .iter()
                    .map(|(name, desired)| (name.as_str(), held.entry(name, desired)))
                    .collect(),
            }),
        };
        let mut text = encoded
            .map_err(io::Error::from)
            .map_err(io_at(&self.path))?;
        text.push(b'\n');
        write_whole_syncing(&self.path, &text, self.sync_dir)
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
fn sync_dir(dir: &Path) -> io::Result<()> {
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
