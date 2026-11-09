//! The repository installer (packages-repositories.md sections 3 to 5, packages-native.md
//! section 10.2): a service's package installed from a package repository of the host's
//! configuration with the host's package manager, one installer kind for every manager, whose
//! per-manager commands are a [`Backend`]. `apt` ([`apt::Apt`]) is the backend built here; a
//! host announces the backend's name as its installer.
//!
//! | Step       | Does | Network |
//! |------------|------|---------|
//! | `stage`    | Refuses a package another service records; finds the order's repository in the host's configuration; makes the work directory see it alone; fetches and verifies the index; reads the version's stanza and checks its digest against the pin and its size against `max_package_bytes`; the dependency check (under the installer lock); downloads the package and hashes it against the pin; checks its control fields, refuses a maintainer script, extracts it with no script run, and checks the tree; keeps the index and the package in the version directory | The update and the download only |
//! | `activate` | Under the installer lock: fills the work directory from the version directory, reads what is installed (a version hostkeeper did not set is drift), records the version pending, installs it with no download (a downgrade for a rollback), checks the database shows it, holds it, enables the `run` units, reloads the service manager, records it done | None |
//! | `retire`   | Deletes a version directory that is neither `current` nor `previous`; runs no tool | None |
//! | `remove`   | Under the installer lock: releases the hold, purges the package whatever version is installed (tried again while the lock is held, within `remove_secs`), takes away the recorded links, reloads, drops the record | None |
//!
//! The version directory is the core's (packages-lifecycle.md section 6.1): `stage` writes
//! `tree/` and the backend's directory (`apt/`) into the empty staging directory it is given,
//! and the core walks, seals and publishes it, and switches `current` after `activate`. A
//! rollback is `activate` of the kept previous version's directory, which fetches nothing.
//!
//! - [`config`]: the host's repository entries, `/etc/hostkeeper/repositories/`, checked at the
//!   daemon's start.
//! - [`outcome`]: a step's failure, its class and its reason.
//! - [`tools`]: the tools behind the [`Tools`] port, and how each run is confined.
//! - [`record`]: the service's `installed` entry, behind the [`Records`] port.
//! - [`lock`]: the host-wide installer lock.
//! - [`tree`] and [`enable`]: the checks of the extracted tree, and the units' enablement.
//! - [`announce`]: when a daemon announces a repository installer (section 4.1).
//! - [`launcher`]: the [`Tools`] port over the step launcher's transient units.

pub mod announce;
pub mod apt;
pub mod backend;
pub mod config;
pub mod enable;
pub mod launcher;
pub mod lock;
pub mod outcome;
pub mod record;
pub mod tools;
pub mod tree;

mod activate;
mod remove;
mod stage;

use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;

pub use backend::Backend;
pub use config::{Repositories, Repository};
pub use lock::InstallerLock;
pub use outcome::{Class, DRIFT_REPAIRED, Failure, Outcome, Reason};
pub use record::{PackageRecord, RecordError, Records};
pub use tools::{Network, Pause, Run, Tool, ToolOutput, Tools};

/// The bounds of the installer's own steps, in seconds (the manifest's `installer_bounds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The index fetch, the download and the checks: each tool run of `stage`.
    pub stage: u64,
    /// Each tool run of `activate`.
    pub activate: u64,
    /// The purge, its tries included.
    pub remove: u64,
}

/// What an order asks of a repository installer, read from its entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub service: ServiceName,
    /// The release's version, `desired.version`.
    pub version: Version,
    /// The package, the manifest's `package`.
    pub package: String,
    /// The version as the package manager spells it, the entry's `package_version`.
    pub package_version: String,
    /// The entry of the host's configuration it comes from.
    pub repository: String,
    /// The SHA-256 of the package file, the entry's required `digest`.
    pub pin: String,
    /// The units hostkeeper starts, stops, enables and observes.
    pub run: Vec<String>,
    /// The hook's boot step, which needs a hook in the package.
    pub boot: Option<String>,
    /// Whether the manifest has params, which need a hook in the package.
    pub params: bool,
    pub bounds: Bounds,
    /// The daemon's `max_package_bytes`.
    pub max_package_bytes: u64,
}

/// The entry's shape against its installer's kind (section 2.3): an artifact installer's entry
/// names no repository and no package version, and a repository installer's names no artifact
/// (`order_invalid`); a repository installer's entry with no repository is one only a
/// coordinator that dropped the field sends (`coordinator_too_old`, deferred).
pub fn judge_kind(installer: Installer, entry: &Target) -> Result<(), Failure> {
    let invalid = |why: &str| Err(Failure::new(Reason::OrderInvalid, why));
    if !installer.is_repository() {
        return match entry.repository.is_some() | entry.package_version.is_some() {
            true => invalid("an artifact entry names a repository or a package version"),
            false => Ok(()),
        };
    }
    let artifact = !entry.locations.is_empty() | (entry.bytes != 0) | entry.attestation.is_some();
    if artifact {
        return invalid("a repository entry names locations, bytes or an attestation");
    }
    match entry.repository {
        Some(_) => Ok(()),
        None => Err(Failure::new(
            Reason::CoordinatorTooOld,
            "a repository installer's entry has no repository: the coordinator dropped the field",
        )),
    }
}

impl Order {
    /// The order of `entry` for `service` at `version`, checked against `backend`'s rules:
    /// the entry's kind ([`judge_kind`]), the pin, the backend's grammars, and the manifest rules
    /// of a repository entry (packages-repositories.md section 3.4, packages-native.md section
    /// 10.2: `hook.command` empty, no `place`, no `containers`, 1 to 64 `run` units, the
    /// installer's bounds).
    pub fn from_entry(
        backend: &dyn Backend,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
        max_package_bytes: u64,
    ) -> Result<Order, Failure> {
        let installer = backend.installer();
        if entry.installer != installer.as_str() {
            return Err(Failure::new(
                Reason::OrderInvalid,
                format!(
                    "the entry is for {:?}, not {}",
                    entry.installer,
                    installer.as_str()
                ),
            ));
        }
        judge_kind(installer, entry)?;
        let pin = &entry.digest;
        let hex = (pin.len() == 64) & pin.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex {
            return Err(Failure::new(
                Reason::OrderInvalid,
                "the entry's digest is not 64 lower-case hex digits",
            ));
        }
        let invalid = |why: String| Failure::new(Reason::ManifestInvalid, why);
        let manifest = entry
            .manifest
            .as_ref()
            .ok_or_else(|| invalid("the entry has no manifest".to_owned()))?;
        let package = manifest
            .package
            .clone()
            .ok_or_else(|| invalid("the manifest names no package".to_owned()))?;
        let package_version = entry.package_version.clone().unwrap_or_default();
        backend
            .check_names(&package, &package_version)
            .map_err(invalid)?;
        let hook = manifest.hook.clone().unwrap_or_default();
        if !hook.command.is_empty() {
            return Err(invalid(
                "the manifest's hook.command is not empty: a package's hook is its program at a fixed path"
                    .to_owned(),
            ));
        }
        if !manifest.place.is_empty() {
            return Err(invalid(
                "the manifest has place entries; the package places its own files".to_owned(),
            ));
        }
        if !manifest.containers.is_empty() {
            return Err(invalid(
                "the manifest has containers; a package runs no image".to_owned(),
            ));
        }
        if !(1..=64).contains(&manifest.run.len()) {
            return Err(invalid(format!(
                "the manifest has {} run units, not 1 to 64",
                manifest.run.len()
            )));
        }
        let bounds = manifest
            .installer_bounds
            .filter(|b| (b.stage_secs > 0) & (b.activate_secs > 0) & (b.remove_secs > 0))
            .ok_or_else(|| invalid("the manifest has no installer bounds above 0".to_owned()))?;
        Ok(Order {
            service: service.clone(),
            version: version.clone(),
            package,
            package_version,
            repository: entry.repository.clone().unwrap_or_default(),
            pin: pin.clone(),
            run: manifest.run.clone(),
            boot: hook.boot.clone(),
            params: !manifest.params.is_empty(),
            bounds: Bounds {
                stage: bounds.stage_secs,
                activate: bounds.activate_secs,
                remove: bounds.remove_secs,
            },
            max_package_bytes,
        })
    }
}

/// Where the installer works on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDirs {
    /// The host's root: `/` on a host, a scratch directory in a test. The units' enablement
    /// links are written under it.
    pub root: PathBuf,
    /// The work directories' parent, `/var/lib/hostkeeper/<backend>/` on a host: one directory
    /// per service, where every run of the package manager works.
    pub work: PathBuf,
}

/// A version `stage` made in its staging directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    /// Whether the package has a hook, `tree/usr/lib/<package>/hostkeeper-hook`; without one
    /// the service gets the built-in steps.
    pub hook: bool,
}

/// A package found at a version hostkeeper did not set (section 4.4), as the heartbeat reports
/// it (`ServiceBeat.drift`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    /// The version the package manager shows; `None`: not installed.
    pub found: Option<String>,
    /// The version the record names; `None`: no record names the package.
    pub recorded: Option<String>,
}

/// How an `activate` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activated {
    /// The drift it found and repaired: its verdict is `Succeeded`, [`DRIFT_REPAIRED`].
    pub drift: Option<Drift>,
}

/// How a `remove` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The drift it found: the package was at a version the record did not name, and was
    /// purged all the same.
    pub drift: Option<Drift>,
}

/// The repository installer of one backend, with the host's ports.
pub struct RepositoryInstaller<'a> {
    pub backend: &'a dyn Backend,
    pub tools: &'a dyn Tools,
    pub records: &'a dyn Records,
    pub lock: &'a InstallerLock,
    pub repositories: &'a Repositories,
    pub dirs: &'a HostDirs,
    pub pause: &'a dyn Pause,
}

/// A failure of the host's files.
fn io_failure(what: &str, error: impl std::fmt::Display) -> Failure {
    Failure::new(Reason::IoError, format!("{what}: {error}"))
}

/// A failure of the record's port.
fn record_failure(error: RecordError) -> Failure {
    Failure::new(Reason::IoError, error.to_string())
}

impl RepositoryInstaller<'_> {
    /// The work directory of `service`, emptied and made again.
    fn fresh_work(&self, service: &ServiceName) -> Result<PathBuf, Failure> {
        let work = self.dirs.work.join(service.as_str());
        remove_tree(&work).map_err(|e| io_failure("the work directory", e))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .create(&work)
            .map_err(|e| io_failure("the work directory", e))?;
        Ok(work)
    }

    /// Deletes a version directory that is neither `current` nor `previous`, and its seal
    /// beside it; runs no tool, so the installed package is untouched.
    pub fn retire(&self, dir: &Path) -> Result<(), Failure> {
        remove_tree(dir).map_err(|e| io_failure("the version directory", e))?;
        let mut seal = dir.as_os_str().to_owned();
        seal.push(".seal");
        remove_tree(Path::new(&seal)).map_err(|e| io_failure("the version's seal", e))
    }
}

/// Removes `path` and everything below it, opening read-only directories first; nothing to do
/// when it is not there.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    if !meta.is_dir() {
        return std::fs::remove_file(path);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    for entry in std::fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}
