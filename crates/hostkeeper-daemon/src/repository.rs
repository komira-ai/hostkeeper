//! The repository installer in the daemon (packages-repositories.md section 4): an order whose
//! entry is a repository entry (`apt`) is staged, activated, rolled back, removed and observed by
//! [`hostkeeper_install::repository::RepositoryInstaller`] over its backend
//! ([`hostkeeper_install::backend_of`]), with the daemon's ports.
//!
//! | Where | What the daemon does for a repository version |
//! |---|---|
//! | the update's `prepare` ([`Repository::stage`]) | The order of the entry ([`Order::from_entry`]); the version staged into an empty staging directory by the installer's `stage`, the entry written beside it as `target.json` ([`crate::target`]), sealed and published as any version is ([`hostkeeper_unpack::stage_filled`]). A version already on the host is the sealed one, and an order whose entry is not the sealed `target.json` field for field is `target_changed` |
//! | the switch before `cutover` ([`Repository::activate`]) | The installer's `activate` of the version, from its own copy of the index and the package, then `current` switched to it |
//! | the switch before `rollback` ([`Repository::back`]) | The installer's `activate` of the previous version from its sealed directory, no fetch, then `current` switched back; for a first install, the `run` units stopped and the package removed, then `current` |
//! | the uninstall's step 5 ([`Repository::remove`]) | The installer's `remove`, within the `remove_secs` of the version `current` names |
//! | each heartbeat ([`Repository::drift`]) | The installed version against the recorded one: drift, reported as `ServiceBeat.drift` |
//!
//! A step's [`Failure`] keeps its outcome, class and reason (section 4.5). Before the runner it is
//! the order's verdict ([`finished`]): `Failed` before the commit point with its class, or
//! `Deferred` for [`DEFER_SECS`]; its detail is logged. In the switch it is the step's exit
//! ([`exit_of`]): the runner reads it as the hook's own last line, so a failed `activate` rolls
//! back as a failed `cutover` does. A failure of the host's files (`io_error`: the record or the
//! installer lock could not be written) is no verdict of the package: the switch fails and is
//! tried again ([`crate::place::retried`]).
//!
//! Each tool runs through the daemon's step launcher as a transient unit of the service
//! ([`LauncherTools`]); a purge whose lock is held waits on the daemon's clock ([`ClockPause`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class as HookClass, Phase, Verdict};
use hostkeeper_install::backend_of;
use hostkeeper_install::repository::launcher::LauncherTools;
use hostkeeper_install::repository::{
    Backend, Class, Drift, Failure, HostDirs, InstallerLock, Order, Outcome, PackageRecord, Pause,
    Reason, RecordError, Records, Repositories, RepositoryInstaller,
};
use hostkeeper_placed::{Fs, InstalledEntry, Placer, Roots, UnitControl};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_steps::{Clock, StepExit, StepLauncher, SwitchError, UpdateOrder};
use hostkeeper_unpack::{FillError, FillOrder, HostFs, Owner, stage_filled};

use crate::executor::Reply;
use crate::packages::VersionDir;
use crate::real_packages::{self, refusal_of};
use crate::refusal::Refusal;
use crate::target::{self, TargetError};

/// The seconds a repository step that deferred asks to be ordered again after: a suite seen
/// through a cache may lag its publish by minutes, and an unattended upgrade holds the
/// package manager's lock for as long.
pub const DEFER_SECS: u64 = 60;

/// The bound of the heartbeat's read of the installed version (`dpkg-query`), in seconds.
pub const DRIFT_QUERY_SECS: u64 = 30;

/// The bound of a purge whose entry names no installer bounds (a version on the host whose
/// `target.json` is gone with its directory), in seconds.
pub const REMOVE_SECS_WITHOUT_BOUNDS: u64 = 120;

/// The reason of an order whose entry is not the sealed `target.json` of a version already on
/// the host (packages-lifecycle.md section 6.1).
pub const REASON_TARGET_CHANGED: &str = "target_changed";

/// The work directories' parent and the installer lock, beside the packages directory
/// (`/var/lib/hostkeeper/apt`, `/var/lib/hostkeeper/installer.lock` on a host).
pub const WORK_DIR: &str = "apt";
/// The installer lock's file name, beside the packages directory.
pub const LOCK_FILE: &str = "installer.lock";

/// What the repository installer works with on the host, besides the step launcher and the
/// clock: the host's repository entries, read once at the start, the packages directory a version
/// is staged in and the owner of what is staged, the host's root and work directories, and the
/// installer lock. Nothing is opened or made until a step needs it.
#[derive(Clone, Debug)]
pub struct RepositoryPorts {
    /// The entries of `/etc/hostkeeper/repositories/`; none on a Mac or a host without one.
    pub repositories: Repositories,
    /// The packages directory, where a version is staged, sealed and published.
    pub packages_dir: PathBuf,
    /// The owner of everything staged: [`Owner::ROOT`] in the daemon.
    pub owner: Owner,
    /// The host's root (`/`) and the work directories' parent.
    pub dirs: HostDirs,
    /// The installer lock's file.
    pub lock: PathBuf,
}

impl RepositoryPorts {
    /// The ports of the host whose root is `root` and whose packages directory is
    /// `packages_dir`, everything staged given to `owner`: the work directories in
    /// `<packages_dir>/../apt`, the lock `<packages_dir>/../installer.lock`.
    pub fn new(
        packages_dir: &Path,
        root: &Path,
        repositories: Repositories,
        owner: Owner,
    ) -> RepositoryPorts {
        let state = packages_dir.parent().unwrap_or(packages_dir);
        RepositoryPorts {
            repositories,
            packages_dir: packages_dir.to_path_buf(),
            owner,
            dirs: HostDirs {
                root: root.to_path_buf(),
                work: state.join(WORK_DIR),
            },
            lock: state.join(LOCK_FILE),
        }
    }
}

/// The purge's wait, on the daemon's clock.
pub struct ClockPause(pub Arc<dyn Clock>);

impl Pause for ClockPause {
    fn pause(&self, secs: u64) {
        self.0.sleep_secs(secs);
    }
}

/// The services' `installed` entries of `placed.json`, as the repository installer reads and
/// writes them: the placed lock is taken for each write alone, never across a tool's run.
pub struct PlacedRecords {
    pub roots: Roots,
    pub fs: Arc<dyn Fs + Send + Sync>,
    pub units: Arc<dyn UnitControl + Send + Sync>,
}

fn record_error(error: impl std::fmt::Display) -> RecordError {
    RecordError(error.to_string())
}

impl PlacedRecords {
    fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots.clone(), &*self.fs, &*self.units)
    }

    /// Changes the `installed` entries with `change`, under the placed lock.
    fn change(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, InstalledEntry>),
    ) -> Result<(), RecordError> {
        let placer = self.placer();
        let lock = placer.lock().map_err(record_error)?;
        let mut record = placer.store().load().map_err(record_error)?;
        change(&mut record.installed);
        placer.store().save(&record, &lock).map_err(record_error)
    }
}

/// The package record an `installed` entry holds; `None` for an entry that names no package.
fn package_record(entry: &InstalledEntry) -> Option<PackageRecord> {
    Some(PackageRecord {
        installer: entry.installer.clone(),
        package: entry.package.clone()?,
        version: entry.version.clone()?,
        repository: entry.repository.clone()?,
        links: entry.links.clone(),
        pending: entry.pending,
    })
}

impl Records for PlacedRecords {
    fn load(&self, service: &ServiceName) -> Result<Option<PackageRecord>, RecordError> {
        let record = self.placer().store().load().map_err(record_error)?;
        Ok(record
            .installed
            .get(service.as_str())
            .and_then(package_record))
    }

    fn save(&self, service: &ServiceName, saved: &PackageRecord) -> Result<(), RecordError> {
        self.change(|installed| {
            let entry = installed
                .entry(service.to_string())
                .or_insert_with(|| InstalledEntry::new(&saved.installer));
            entry.installer.clone_from(&saved.installer);
            entry.package = Some(saved.package.clone());
            entry.version = Some(saved.version.clone());
            entry.repository = Some(saved.repository.clone());
            entry.links.clone_from(&saved.links);
            entry.pending = saved.pending;
        })
    }

    fn clear(&self, service: &ServiceName) -> Result<(), RecordError> {
        self.change(|installed| {
            installed.remove(service.as_str());
        })
    }

    fn holder(
        &self,
        package: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        let record = self.placer().store().load().map_err(record_error)?;
        Ok(record
            .installed
            .iter()
            .filter(|(other, entry)| {
                (other.as_str() != service.as_str()) & (entry.package.as_deref() == Some(package))
            })
            .find_map(|(other, _)| ServiceName::new(other).ok()))
    }
}

/// The repository installer with the daemon's ports, for every service.
#[derive(Clone)]
pub struct Repository {
    pub launcher: Arc<dyn StepLauncher>,
    pub ports: Arc<RepositoryPorts>,
    pub clock: Arc<dyn Clock>,
    pub roots: Roots,
    pub fs: Arc<dyn Fs + Send + Sync>,
    pub units: Arc<dyn UnitControl + Send + Sync>,
    /// The daemon's `max_package_bytes`.
    pub max_package_bytes: u64,
    /// The drift each service's last `activate` found and repaired, until its update's verdict
    /// takes it ([`Repository::take_repaired`]).
    pub repaired: Arc<Mutex<BTreeMap<String, Drift>>>,
}

/// Why a version could not be staged.
#[derive(Debug, thiserror::Error)]
enum StageFailure {
    #[error("{0}")]
    Installer(Failure),
    #[error("{0}")]
    Target(TargetError),
    #[error("{0}")]
    Changed(String),
}

/// The repository installer's backend of `entry`, `None` for an artifact entry.
pub fn backend_of_entry(entry: &Target) -> Option<&'static dyn Backend> {
    entry
        .installer
        .parse::<Installer>()
        .ok()
        .and_then(backend_of)
}

/// The order's verdict for a step that failed before the runner: `Failed` before the commit
/// point with the failure's class, or `Deferred` for [`DEFER_SECS`], with its reason.
pub fn finished(failure: &Failure) -> Reply {
    let reason = failure.reason();
    let verdict = match reason.outcome() {
        Outcome::Failed => Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: hook_class(reason.class()),
        },
        Outcome::Deferred => Verdict::Deferred {
            retry_after_secs: Some(DEFER_SECS),
        },
    };
    Reply::Finished {
        verdict,
        reason: Some(reason.token().to_owned()),
        resent: false,
    }
}

fn hook_class(class: Class) -> HookClass {
    match class {
        Class::Release => HookClass::Release,
        Class::Environment => HookClass::Environment,
    }
}

/// The exit a step ends with when its installer half failed in the switch: 1 for `Failed`, 75 for
/// `Deferred`, and the report's line with the failure's class, reason and detail.
pub fn exit_of(failure: &Failure) -> StepExit {
    let reason = failure.reason();
    let (code, retry) = match reason.outcome() {
        Outcome::Failed => (1, None),
        Outcome::Deferred => (75, Some(DEFER_SECS)),
    };
    let mut line = serde_json::json!({
        "class": reason.class().token(),
        "reason": reason.token(),
        "detail": failure.detail(),
    });
    if let Some(secs) = retry {
        line["retry_after_secs"] = serde_json::json!(secs);
    }
    StepExit {
        code: Some(code),
        last_line: Some(line.to_string()),
    }
}

/// The switch's error for a step's failure: a failure of the host's files is tried again; any
/// other ends the step with its exit ([`exit_of`]).
pub fn switch_error(failure: Failure) -> SwitchError {
    match failure.reason() {
        Reason::IoError => SwitchError::Failed(failure.to_string()),
        _ => SwitchError::Ended(exit_of(&failure)),
    }
}

/// The names of `order`.
fn names(order: &UpdateOrder) -> Result<(ServiceName, Version), SwitchError> {
    let failed = |e: String| SwitchError::Failed(e);
    let service = ServiceName::new(&order.service).map_err(|e| failed(e.to_string()))?;
    let version = Version::new(&order.target_version).map_err(|e| failed(e.to_string()))?;
    Ok((service, version))
}

impl Repository {
    fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots.clone(), &*self.fs, &*self.units)
    }

    /// Runs `step` with the installer of `backend` for `service`.
    fn with<R>(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        step: impl FnOnce(&RepositoryInstaller<'_>) -> R,
    ) -> R {
        let tools = LauncherTools::new(&*self.launcher, service.as_str());
        let records = PlacedRecords {
            roots: self.roots.clone(),
            fs: Arc::clone(&self.fs),
            units: Arc::clone(&self.units),
        };
        let lock = InstallerLock::new(&self.ports.lock);
        let pause = ClockPause(Arc::clone(&self.clock));
        step(&RepositoryInstaller {
            backend,
            tools: &tools,
            records: &records,
            lock: &lock,
            repositories: &self.ports.repositories,
            dirs: &self.ports.dirs,
            pause: &pause,
        })
    }

    /// The order of `entry` for `service` at `version`.
    fn order(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
    ) -> Result<Order, Failure> {
        Order::from_entry(backend, service, version, entry, self.max_package_bytes)
    }

    /// The sealed version directory of `entry` for `service` at `version`, staged by the
    /// installer of `backend` when it is not on the host. `Err` is the order's reply.
    pub fn stage(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
    ) -> Result<VersionDir, Reply> {
        let order = self
            .order(backend, service, version, entry)
            .map_err(|failure| finished(&failure))?;
        let digest = Sha256Hex::parse(&order.pin).ok_or_else(|| {
            Reply::refused(Refusal::OrderInvalid, "the entry's digest is no SHA-256")
        })?;
        let fill_order = FillOrder {
            service,
            version,
            package_digest: &digest,
        };
        let mut same = |dir: &Path| match target::read(dir) {
            Ok(Some(sealed)) if sealed == *entry => Ok(()),
            Ok(_) => Err(StageFailure::Changed(format!(
                "{version} of {service} is on the host for another entry than the order's"
            ))),
            Err(error) => Err(StageFailure::Target(error)),
        };
        let mut fill = |stage: &Path| {
            self.with(backend, service, |installer| installer.stage(&order, stage))
                .map_err(StageFailure::Installer)?;
            target::write(stage, entry).map_err(StageFailure::Target)
        };
        let packages = HostFs::open(&self.ports.packages_dir, self.ports.owner)
            .map_err(|error| Reply::refused(Refusal::PackageIoError, error))?;
        let filled = stage_filled(&fill_order, &packages, &mut same, &mut fill).map_err(
            |error| match error {
                FillError::Unpack(error) => {
                    let refused = refusal_of(real_packages::Failure::Unpack(&error));
                    Reply::refused(refused.refusal, refused.detail)
                }
                FillError::Fill(StageFailure::Installer(failure)) => finished(&failure),
                FillError::Fill(StageFailure::Changed(_)) => Reply::Finished {
                    verdict: Verdict::Failed {
                        phase: Phase::BeforeCommit,
                        class: HookClass::Release,
                    },
                    reason: Some(REASON_TARGET_CHANGED.to_owned()),
                    resent: false,
                },
                FillError::Fill(StageFailure::Target(error)) => {
                    Reply::refused(Refusal::ReleaseUnreadable, error)
                }
            },
        )?;
        hostkeeper_steps::Staged::seal(&filled.dir)
            .map(VersionDir::new)
            .map_err(|error| Reply::refused(Refusal::ReleaseUnreadable, error))
    }

    /// The installer's `activate` of the version `dir` holds for `entry`, then `current`
    /// switched to `version`: the switch of a hooked order, and step 11 of a force.
    pub fn activate_version(
        &self,
        service: &ServiceName,
        version: &Version,
        dir: &Path,
        entry: &Target,
    ) -> Result<(), SwitchError> {
        let backend = backend_of_entry(entry).ok_or_else(|| {
            SwitchError::Failed(format!(
                "{version} of {service} was staged for the installer {:?}, which has no backend here",
                entry.installer
            ))
        })?;
        let order = self
            .order(backend, service, version, entry)
            .map_err(switch_error)?;
        let activated = self
            .with(backend, service, |installer| {
                installer.activate(&order, dir)
            })
            .map_err(switch_error)?;
        if let Some(drift) = activated.drift {
            let mut repaired = self.repaired.lock().unwrap_or_else(PoisonError::into_inner);
            repaired.insert(service.to_string(), drift);
        }
        self.placer()
            .switch_current(service, version)
            .map(drop)
            .map_err(|e| SwitchError::Failed(e.to_string()))
    }

    /// The switch before `cutover` of `order`, whose version directory holds `entry`.
    pub fn activate(&self, order: &UpdateOrder, entry: &Target) -> Result<(), SwitchError> {
        let (service, version) = names(order)?;
        self.activate_version(&service, &version, order.staged.dir(), entry)
    }

    /// The switch before `rollback` of `order`: the previous version activated again from its
    /// sealed directory, which holds `previous`; or, for a first install (`previous` none), the
    /// `run` units of `entry` stopped and the package removed, then `current` when it names the
    /// target.
    pub fn back(
        &self,
        order: &UpdateOrder,
        entry: &Target,
        previous: Option<(&Version, &Path, &Target)>,
    ) -> Result<(), SwitchError> {
        let (service, version) = names(order)?;
        if let Some((previous, dir, sealed)) = previous {
            return self.activate_version(&service, previous, dir, sealed);
        }
        let failed = |e: String| SwitchError::Failed(e);
        let manifest = entry.manifest.clone().unwrap_or_default();
        for unit in &manifest.run {
            self.units
                .stop_unit(unit)
                .map_err(|e| failed(e.to_string()))?;
        }
        let remove_secs = manifest
            .installer_bounds
            .map_or(REMOVE_SECS_WITHOUT_BOUNDS, |bounds| bounds.remove_secs);
        let backend = backend_of_entry(entry).ok_or_else(|| {
            failed(format!(
                "the installer {:?} has no backend here",
                entry.installer
            ))
        })?;
        self.with(backend, &service, |installer| {
            installer.remove(&service, remove_secs)
        })
        .map_err(switch_error)?;
        let placer = self.placer();
        let named = placer
            .current(service.as_str())
            .map_err(|e| failed(e.to_string()))?;
        if named.as_deref() != Some(version.as_str()) {
            return Ok(());
        }
        let current = placer.roots().packages().current(service.as_str());
        self.fs
            .remove_file(&current)
            .and_then(|()| self.fs.sync_dir(current.parent().unwrap_or(Path::new("/"))))
            .map_err(|e| failed(e.to_string()))
    }

    /// The installer's `remove` of `service`, installed from `entry`, within its `remove_secs`.
    pub fn remove(&self, service: &ServiceName, entry: &Target) -> Result<(), Failure> {
        let remove_secs = entry
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.installer_bounds)
            .map_or(REMOVE_SECS_WITHOUT_BOUNDS, |bounds| bounds.remove_secs);
        let backend = backend_of_entry(entry).ok_or_else(|| {
            Failure::new(
                Reason::OrderInvalid,
                format!("the installer {:?} has no backend here", entry.installer),
            )
        })?;
        self.with(backend, service, |installer| {
            installer.remove(service, remove_secs)
        })
        .map(drop)
    }

    /// The drift of `service`, whose `current` version holds `entry`, at this heartbeat: `None`
    /// when there is none, or when the installed version cannot be read (said by `log`).
    pub fn drift(&self, service: &ServiceName, entry: &Target) -> Result<Option<Drift>, Failure> {
        let backend = backend_of_entry(entry).ok_or_else(|| {
            Failure::new(
                Reason::OrderInvalid,
                format!("the installer {:?} has no backend here", entry.installer),
            )
        })?;
        let package = entry
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.package.clone())
            .unwrap_or_default();
        self.with(backend, service, |installer| {
            installer.drift(service, &package, DRIFT_QUERY_SECS)
        })
    }

    /// The drift the last `activate` of `service` repaired, taken: the update that ran it ends
    /// `Succeeded` with the reason `drift_repaired`.
    pub fn take_repaired(&self, service: &ServiceName) -> Option<Drift> {
        let mut repaired = self.repaired.lock().unwrap_or_else(PoisonError::into_inner);
        repaired.remove(service.as_str())
    }
}
