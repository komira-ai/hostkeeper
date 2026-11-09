//! The `macos-pkg` installer in the daemon (packages-repositories.md section 4.7): what a Mac
//! announces, and the daemon's side of each step of [`hostkeeper_install::macos_pkg::MacosPkg`].
//!
//! **What a Mac announces.** A daemon announces `macos-pkg` when it runs on a Mac, the signers
//! file `/etc/hostkeeper/macos-pkg.toml` loaded at its start with at least one signer (a file
//! the loader refuses stops the start, [`crate::cli::load_signers`]), and `/usr/sbin/installer`
//! and `/usr/sbin/pkgutil` are present ([`hostkeeper_install::macos_pkg::announces`]). Then, and
//! only then, the daemon holds the installer's ports ([`MacosPkgPorts`], through
//! [`crate::repository::RepositoryPorts::macos_pkg`]); [`announced`] lists the installer from
//! the same value, so the heartbeat never lists an installer the order path cannot run, and a
//! Mac without the signers file, or without the tools, announces `hostkeeper-archive` alone.
//!
//! **The steps.** A `macos-pkg` version is a native version, as an `apt` one is: its
//! directory holds `target.json`, it places nothing, and the order path, the switch, the force,
//! the uninstall, hostkeeper's own update and the heartbeat's drift reach it through
//! [`crate::repository::Repository`], which runs each step here for an entry whose installer is
//! `macos-pkg` ([`crate::repository::native_of_entry`]):
//!
//! | Where | What the daemon does for a `macos-pkg` version |
//! |---|---|
//! | the update's `prepare` ([`Repository::stage_pkg`]) | The order of the entry ([`Order::of`]); the `.pkg` fetched by its pin alone (a `macos-pkg` entry carries no attestation: [`crate::packages::Packages::pinned`]), then the installer's `stage`: the signature checked against the host's signers before anything is expanded, so an unsigned package, or one whose leaf is no signer, is `Failed`, `environment`, `signature`, with nothing staged; then sealed and published with `target.json` as an `apt` version is ([`Repository::fill_sealed`]) |
//! | the switch before `cutover` ([`Repository::activate_pkg`]) | The installer's `activate` of the version, `from` the `macos-pkg` version `current` names when it is another, then `current` switched |
//! | the switch before `rollback` | The previous version activated again from its sealed directory (`from` the one rolled back from); for a first install, its `run` jobs stopped, the package removed ([`Repository::remove_pkg`]) and `current` taken away |
//! | the uninstall's step 5 ([`Repository::remove_pkg`]) | The installer's `remove` over the versions the host keeps, then the record dropped |
//! | each heartbeat ([`Repository::drift_pkg`]) | The receipt and the recorded version's files against the record ([`observe`]), one [`Seen`] per service kept between heartbeats |
//!
//! A step's failure is reported as the repository installer's ([`Failure::from`]): the same
//! reason, class and outcome, so `finished`, `exit_of` and `switch_error` apply unchanged.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::host::Host;
use hostkeeper_install::lock::InstallerLock as PkgLock;
use hostkeeper_install::macos_pkg::drift::{Seen, observe};
use hostkeeper_install::macos_pkg::record::{INSTALLER, PkgRecord, RecordError, Records};
use hostkeeper_install::macos_pkg::signers::Signers;
use hostkeeper_install::macos_pkg::{Bounds, MacosPkg, Order, announces};
use hostkeeper_install::repository::{Drift, Failure, Reason};
use hostkeeper_install::{Announcement, StageOrder, Tools};
use hostkeeper_placed::InstalledEntry;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;

use crate::executor::Reply;
use crate::installed::{CURRENT, PREVIOUS};
use crate::packages::{PackageError, VersionDir};
use crate::repository::{PlacedRecords, REMOVE_SECS_WITHOUT_BOUNDS, Repository, StageFailure};
use crate::target;

/// The detail of a `macos-pkg` step on a daemon that holds no `macos-pkg` installer: one that
/// does not announce it, which the receipt check defers before any step runs.
pub const NO_MACOS_PKG: &str = "this daemon runs no macos-pkg installer: it is no Mac with a signers file, installer and pkgutil";

/// What the `macos-pkg` installer works with on a Mac that announces it: the tools, the host's
/// file system, the signers loaded at the start, and what the observer saw of each service's
/// files at the last heartbeat.
pub struct MacosPkgPorts {
    /// `pkgutil`, `installer`, `lsbom` and `launchctl` (`HostTools` on a Mac).
    pub tools: Arc<dyn Tools>,
    /// The host (`Host::mac()` on a Mac).
    pub host: Host,
    /// The signers of `/etc/hostkeeper/macos-pkg.toml`.
    pub signers: Signers,
    seen: Mutex<BTreeMap<String, Seen>>,
}

impl fmt::Debug for MacosPkgPorts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacosPkgPorts")
            .field("host", &self.host)
            .field("signers", &self.signers)
            .finish_non_exhaustive()
    }
}

/// The `macos-pkg` installer's ports of a host: `Some` only on a Mac (`macos`) with its tools
/// (`tools`, `None` in a build for another platform) whose host `announces` the installer with
/// `signers`, the signers file loaded at the start.
pub fn ports(
    macos: bool,
    host: Host,
    signers: Option<Signers>,
    tools: Option<Arc<dyn Tools>>,
) -> Option<MacosPkgPorts> {
    let tools = tools.filter(|_| macos)?;
    let signers = signers.filter(|signers| announces(&host, Some(signers)))?;
    Some(MacosPkgPorts {
        tools,
        host,
        signers,
        seen: Mutex::default(),
    })
}

/// `announcement` with `macos-pkg` added when the daemon holds its installer (`ports`).
pub fn announced(mut announcement: Announcement, ports: Option<&MacosPkgPorts>) -> Announcement {
    if ports.is_some() {
        announcement.installers.push(Installer::MacosPkg);
    }
    announcement
}

fn io(error: impl fmt::Display) -> Failure {
    Failure::new(Reason::IoError, error.to_string())
}

/// The record of an `installed` entry of `macos-pkg`; `None` for any other entry.
fn pkg_record(entry: &InstalledEntry) -> Option<PkgRecord> {
    (entry.installer == INSTALLER).then_some(())?;
    Some(PkgRecord {
        installer: entry.installer.clone(),
        identifier: entry.package.clone()?,
        version: entry.version.clone()?,
        created_dirs: entry.created_dirs.clone(),
        pending: entry.pending,
    })
}

/// The services' `installed` entries of `placed.json`, as `macos-pkg` reads and writes them: the
/// identifier as the entry's package, the version, the created directories and `pending`.
impl Records for PlacedRecords {
    fn load(&self, service: &ServiceName) -> Result<Option<PkgRecord>, RecordError> {
        let record = self
            .placer()
            .store()
            .load()
            .map_err(|e| RecordError(e.to_string()))?;
        Ok(record.installed.get(service.as_str()).and_then(pkg_record))
    }

    fn save(&self, service: &ServiceName, saved: &PkgRecord) -> Result<(), RecordError> {
        self.change(|installed| {
            let entry = installed
                .entry(service.to_string())
                .or_insert_with(|| InstalledEntry::new(&saved.installer));
            entry.installer.clone_from(&saved.installer);
            entry.package = Some(saved.identifier.clone());
            entry.version = Some(saved.version.clone());
            entry.repository = None;
            entry.links.clear();
            entry.created_dirs.clone_from(&saved.created_dirs);
            entry.pending = saved.pending;
        })
        .map_err(|e| RecordError(e.0))
    }

    fn holder(
        &self,
        identifier: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        <PlacedRecords as hostkeeper_install::repository::Records>::holder(
            self, identifier, service,
        )
        .map_err(|e| RecordError(e.0))
    }
}

impl Repository {
    /// The `macos-pkg` installer's ports; `io_error` on a daemon that holds none.
    fn pkg_ports(&self) -> Result<&MacosPkgPorts, Failure> {
        self.ports
            .macos_pkg
            .as_deref()
            .ok_or_else(|| Failure::new(Reason::IoError, NO_MACOS_PKG))
    }

    /// Runs `step` with the `macos-pkg` installer over the daemon's ports.
    fn with_pkg<R>(
        &self,
        step: impl FnOnce(&MacosPkg<'_>) -> Result<R, hostkeeper_install::Failure>,
    ) -> Result<R, Failure> {
        let ports = self.pkg_ports()?;
        let records = self.records();
        let lock = PkgLock::new(&self.ports.lock);
        step(&MacosPkg {
            tools: &*ports.tools,
            host: &ports.host,
            records: &records,
            lock: &lock,
            signers: &ports.signers,
        })
        .map_err(Failure::from)
    }

    /// The `macos-pkg` order of `entry` for `service` at `version` ([`Order::of`]), and the
    /// entry's digest.
    fn pkg_order(
        service: &ServiceName,
        version: &Version,
        entry: &Target,
    ) -> Result<(Order, Sha256Hex), Failure> {
        let digest = Sha256Hex::parse(&entry.digest).ok_or_else(|| {
            Failure::new(Reason::OrderInvalid, "the entry's digest is no SHA-256")
        })?;
        let order = Order::of(&StageOrder {
            service,
            version,
            package_digest: &digest,
            entry: Some(entry),
        })
        .map_err(Failure::from)?;
        Ok((order, digest))
    }

    /// The sealed version directory of the `macos-pkg` entry `entry` for `service` at
    /// `version`, staged when it is not on the host; nothing is fetched on a daemon that holds no
    /// `macos-pkg` installer. `Err` is the order's reply.
    pub(crate) fn stage_pkg(
        &self,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
        fetch_failed: &dyn Fn(PackageError) -> Reply,
    ) -> Result<VersionDir, Reply> {
        let (order, digest) = self
            .pkg_ports()
            .and_then(|_| Self::pkg_order(service, version, entry))
            .map_err(|failure| self.stage_failed(service, &failure))?;
        let mut fill = |stage: &Path| {
            let pinned = self
                .packages
                .pinned(service, entry)
                .map_err(StageFailure::Fetch)?;
            self.with_pkg(|pkg| pkg.stage(&order, pinned.path(), stage))
                .map_err(StageFailure::Installer)
        };
        self.fill_sealed(service, version, &digest, entry, &mut fill, fetch_failed)
    }

    /// The directory of `version` of `service` in the packages directory.
    fn version_path(&self, service: &ServiceName, version: &str) -> PathBuf {
        self.ports.packages_dir.join(service.as_str()).join(version)
    }

    /// Whether `dir` holds a `macos-pkg` version.
    fn holds_pkg(dir: &Path) -> Result<bool, Failure> {
        let sealed = target::read(dir).map_err(io)?;
        Ok(sealed.is_some_and(|entry| entry.installer == Installer::MacosPkg.as_str()))
    }

    /// The `macos-pkg` version `current` of `service` names when it is not `version`: the one an
    /// `activate` of `version` replaces, or rolls back from.
    fn replaced(
        &self,
        service: &ServiceName,
        version: &Version,
    ) -> Result<Option<PathBuf>, Failure> {
        let current = self.placer().current(service.as_str()).map_err(io)?;
        let Some(current) = current.filter(|current| current != version.as_str()) else {
            return Ok(None);
        };
        let dir = self.version_path(service, &current);
        Ok(Self::holds_pkg(&dir)?.then_some(dir))
    }

    /// The installer's `activate` of the `macos-pkg` version `dir` holds for `entry`, then
    /// `current` switched to `version`.
    pub(crate) fn activate_pkg(
        &self,
        service: &ServiceName,
        version: &Version,
        dir: &Path,
        entry: &Target,
    ) -> Result<(), Failure> {
        let (order, _) = Self::pkg_order(service, version, entry)?;
        let from = self.replaced(service, version)?;
        let activated = self.with_pkg(|pkg| pkg.activate(&order, dir, from.as_deref()))?;
        if let Some(found) = activated.drift {
            let mut repaired = self.repaired.lock().unwrap_or_else(PoisonError::into_inner);
            repaired.insert(
                service.to_string(),
                Drift {
                    found,
                    recorded: None,
                },
            );
        }
        self.placer()
            .switch_current(service, version)
            .map(drop)
            .map_err(io)
    }

    /// The order a `remove` of `service` runs with: its record's version, with the entry's
    /// identifier, jobs and bounds, or, for an entry with no manifest (a `current` gone), the
    /// record's identifier with no job and the bound a removal falls back to. `None` when no
    /// record names the package: nothing of it is left to remove (the record is dropped last).
    fn removal_order(
        &self,
        service: &ServiceName,
        entry: &Target,
    ) -> Result<Option<Order>, Failure> {
        let record = Records::load(&self.records(), service).map_err(io)?;
        let Some(record) = record else {
            return Ok(None);
        };
        let version = Version::new(&record.version).map_err(io)?;
        if entry.manifest.is_some() {
            return Self::pkg_order(service, &version, entry).map(|(order, _)| Some(order));
        }
        let bound = Duration::from_secs(REMOVE_SECS_WITHOUT_BOUNDS);
        Ok(Some(Order {
            service: service.clone(),
            version,
            identifier: record.identifier,
            run: Vec::new(),
            bounds: Bounds {
                stage: bound,
                activate: bound,
                remove: bound,
            },
            boot: false,
        }))
    }

    /// The `macos-pkg` versions of `service` the host keeps (`current` and `previous`), each
    /// once.
    fn kept(&self, service: &ServiceName) -> Result<Vec<PathBuf>, Failure> {
        let placer = self.placer();
        let mut kept: Vec<PathBuf> = Vec::new();
        for link in [CURRENT, PREVIOUS] {
            let named = match link {
                CURRENT => placer.current(service.as_str()).map_err(io)?,
                _ => crate::installed::linked_version(&self.ports.packages_dir, service, link)
                    .map(|version| version.to_string()),
            };
            if let Some(named) = named {
                kept.push(self.version_path(service, &named));
            }
        }
        let mut found = Vec::new();
        for dir in kept {
            if !found.contains(&dir) && dir.is_dir() && Self::holds_pkg(&dir)? {
                found.push(dir);
            }
        }
        Ok(found)
    }

    /// The installer's `remove` of `service`, installed from the `macos-pkg` entry `entry`,
    /// over the versions the host keeps, then its record dropped.
    pub(crate) fn remove_pkg(&self, service: &ServiceName, entry: &Target) -> Result<(), Failure> {
        let Some(order) = self.removal_order(service, entry)? else {
            return Ok(());
        };
        let kept = self.kept(service)?;
        let kept: Vec<&Path> = kept.iter().map(PathBuf::as_path).collect();
        self.with_pkg(|pkg| pkg.remove(&order, &kept))?;
        <PlacedRecords as hostkeeper_install::repository::Records>::clear(&self.records(), service)
            .map_err(io)
    }

    /// The drift of `service`, whose `current` version holds the `macos-pkg` entry `entry`, at
    /// this heartbeat: the receipt's version and the record's, when they differ or a payload
    /// file is missing or changed.
    pub(crate) fn drift_pkg(
        &self,
        service: &ServiceName,
        entry: &Target,
    ) -> Result<Option<Drift>, Failure> {
        let ports = self.pkg_ports()?;
        let identifier = entry
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.package.clone())
            .unwrap_or_default();
        let record = Records::load(&self.records(), service).map_err(io)?;
        let dir = self
            .placer()
            .current(service.as_str())
            .map_err(io)?
            .map(|current| self.version_path(service, &current));
        let mut seen = ports.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let seen = seen.entry(service.to_string()).or_default();
        let found = observe(
            &ports.host,
            &identifier,
            record.as_ref(),
            dir.as_deref(),
            seen,
        )
        .map_err(io)?;
        Ok(found.map(|drift| Drift {
            found: drift.found,
            recorded: drift.recorded,
        }))
    }
}

#[cfg(test)]
#[path = "macos_pkg_tests.rs"]
mod tests;
