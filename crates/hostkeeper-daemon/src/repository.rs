//! The repository installer in the daemon (packages-repositories.md section 4): an order whose
//! entry is a repository entry (`apt`) is staged, activated, rolled back, removed and observed by
//! [`hostkeeper_install::repository::RepositoryInstaller`] over its backend
//! ([`hostkeeper_install::backend_of`]), with the daemon's ports. An order whose entry is a
//! `macos-pkg` entry goes through the same steps, each run by the `macos-pkg` installer instead
//! ([`crate::macos_pkg`]); [`native_of_entry`] tells the two apart, and every step below
//! dispatches on the entry's installer.
//!
//! | Where | What the daemon does for a repository version |
//! |---|---|
//! | the update's `prepare` ([`Repository::stage`]) | The order of the entry ([`Order::from_entry`]); the version staged into an empty staging directory by the installer's `stage`, the entry written beside it as `target.json` ([`crate::target`]), sealed and published as any version is ([`hostkeeper_install::version::stage_filled`]). A version already on the host is the sealed one, and an order whose entry is not the sealed `target.json` field for field is `target_changed` |
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
//! ([`LauncherTools`]); a purge or a hold whose lock is held waits on the daemon's clock, up to
//! `--package-lock-wait-secs` ([`RepositoryPorts::lock_wait_secs`]), and the wait is said in the
//! log ([`ClockPause`], [`WARN_LOCK_WAIT`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class as HookClass, Phase, Verdict};
use hostkeeper_install::backend_of;
use hostkeeper_install::repository::launcher::LauncherTools;
use hostkeeper_install::repository::loaded;
use hostkeeper_install::repository::{
    Backend, Class, Drift, Failure, HostDirs, InstallerLock, LockWait, Mirror, Order, Outcome,
    PackageRecord, Pause, Reason, RecordError, Records, Repositories, RepositoryInstaller, TREE,
};
use hostkeeper_install::version::{
    FillError, FillOrder, HostFs, Owner, REASON_ARTIFACT_CHANGED, REASON_DAEMON_TOO_OLD,
    REASON_IO_ERROR, REASON_NO_SPACE, REASON_RESERVED_VERSION, REASON_VERSION_TOO_LONG,
    UnpackError, stage_filled,
};
use hostkeeper_placed::{Fs, InstalledEntry, PlacedError, Placer, Roots, UnitControl};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_steps::{Clock, Staged, StepExit, StepLauncher, SwitchError, UpdateOrder};

use crate::executor::Reply;
use crate::macos_pkg::MacosPkgPorts;
use crate::mirror::{CoordinatorMirror, Fetch};
use crate::packages::{PackageError, Packages};
use crate::refusal::Refusal;
use crate::repository_push::{Managed, SharedRepositories};
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
/// The mirrors' parent, beside the packages directory (`/var/lib/hostkeeper/mirror`).
pub const MIRROR_ROOT: &str = "mirror";

/// What the repository installer works with on the host, besides the step launcher and the
/// clock: the host's repository entries, read at the start and replaced by each set the
/// coordinator pushes ([`crate::repository_push`]), the packages directory a version
/// is staged in and the owner of what is staged, the host's root and work directories, and the
/// installer lock. Nothing is opened or made until a step needs it.
#[derive(Clone, Debug)]
pub struct RepositoryPorts {
    /// The entries of `/etc/hostkeeper/repositories/`; none on a Mac or a host without one. A
    /// step takes a snapshot of them at its start.
    pub repositories: SharedRepositories,
    /// The directory the coordinator's sets are written to; none on a Mac, whose daemon takes no
    /// set.
    pub managed: Option<Managed>,
    /// The packages directory, where a version is staged, sealed and published.
    pub packages_dir: PathBuf,
    /// The owner of everything staged: [`Owner::ROOT`] in the daemon.
    pub owner: Owner,
    /// The host's root (`/`) and the work directories' parent.
    pub dirs: HostDirs,
    /// The installer lock's file.
    pub lock: PathBuf,
    /// The `macos-pkg` installer's ports, on a Mac that announces it ([`crate::macos_pkg`]);
    /// `None` on every other host.
    pub macos_pkg: Option<Arc<MacosPkgPorts>>,
    /// The mirror of the private repositories ([`crate::mirror`]); none on a host that reads
    /// none (a Mac), where a private repository's stage defers.
    pub mirror: Option<Arc<CoordinatorMirror>>,
    /// The seconds a step waits at most for the package manager's lock
    /// (`--package-lock-wait-secs`).
    pub lock_wait_secs: u64,
}

impl RepositoryPorts {
    /// The ports of the host whose root is `root` and whose packages directory is
    /// `packages_dir`, everything staged given to `owner`: the work directories in
    /// `<packages_dir>/../apt`, the lock `<packages_dir>/../installer.lock`, the mirrors
    /// `<packages_dir>/../mirror`; the lock waited for the default of `--package-lock-wait-secs`.
    pub fn new(
        packages_dir: &Path,
        root: &Path,
        repositories: Repositories,
        owner: Owner,
    ) -> RepositoryPorts {
        let state = packages_dir.parent().unwrap_or(packages_dir);
        RepositoryPorts {
            repositories: SharedRepositories::new(repositories),
            managed: None,
            packages_dir: packages_dir.to_path_buf(),
            owner,
            dirs: HostDirs {
                root: root.to_path_buf(),
                work: state.join(WORK_DIR),
                mirror: state.join(MIRROR_ROOT),
            },
            lock: state.join(LOCK_FILE),
            macos_pkg: None,
            mirror: None,
            lock_wait_secs: crate::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
        }
    }

    /// These ports, a step waiting `secs` at most for the package manager's lock.
    pub fn with_lock_wait_secs(self, secs: u64) -> RepositoryPorts {
        RepositoryPorts {
            lock_wait_secs: secs,
            ..self
        }
    }

    /// These ports with the mirror of the private repositories, filled through `fetch`, under
    /// [`HostDirs::mirror`].
    pub fn with_mirror(self, fetch: Arc<dyn Fetch>) -> RepositoryPorts {
        let mirror = CoordinatorMirror::new(self.dirs.mirror.clone(), fetch);
        RepositoryPorts {
            mirror: Some(Arc::new(mirror)),
            ..self
        }
    }

    /// The same ports, with the `macos-pkg` installer's (`None`: the host does not announce it).
    pub fn with_macos_pkg(self, macos_pkg: Option<MacosPkgPorts>) -> RepositoryPorts {
        RepositoryPorts {
            macos_pkg: macos_pkg.map(Arc::new),
            ..self
        }
    }

    /// These ports with the directory `managed` names as the one the coordinator's sets are
    /// written to.
    pub fn with_managed(mut self, managed: Managed) -> RepositoryPorts {
        self.managed = Some(managed);
        self
    }
}

/// The warning of a step of `service` that waits for the package manager's lock, said when the
/// wait begins and every 30 seconds while it lasts
/// ([`hostkeeper_install::repository::wait::LOCK_REPORT_SECS`]):
/// `hostkeeper daemon: level=warn alert=lock_wait service=<service> tool=<tool>
/// waited_secs=<n> bound_secs=<n>: <the tool's last line>`.
pub const WARN_LOCK_WAIT: &str = "lock_wait";

/// The wait for the package manager's lock of a step of `service`: a pause is a sleep on the
/// daemon's clock, and a wait is said in `log` ([`WARN_LOCK_WAIT`]).
pub struct ClockPause {
    pub clock: Arc<dyn Clock>,
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
    pub service: String,
}

impl Pause for ClockPause {
    fn pause(&self, secs: u64) {
        self.clock.sleep_secs(secs);
    }

    fn lock_wait(&self, wait: &LockWait) {
        let detail = format!(
            "service={} tool={} waited_secs={} bound_secs={}: {}",
            self.service,
            wait.tool.name(),
            wait.waited_secs,
            wait.bound_secs,
            wait.holder
        );
        (self.log)(&crate::boot::warn_line(WARN_LOCK_WAIT, &detail));
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
    pub(crate) fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots.clone(), &*self.fs, &*self.units)
    }

    /// Changes the `installed` entries with `change`, under the placed lock.
    pub(crate) fn change(
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
    /// The daemon's package store, which fetches a `macos-pkg` entry's `.pkg` by its pin
    /// ([`Packages::pinned`]).
    pub packages: Arc<dyn Packages>,
    pub clock: Arc<dyn Clock>,
    pub roots: Roots,
    pub fs: Arc<dyn Fs + Send + Sync>,
    pub units: Arc<dyn UnitControl + Send + Sync>,
    /// The daemon's `max_package_bytes`.
    pub max_package_bytes: u64,
    /// The drift each service's last `activate` found and repaired, until its update's verdict
    /// takes it ([`Repository::take_repaired`]).
    pub repaired: Arc<Mutex<BTreeMap<String, Drift>>>,
    /// The counter that numbers every tool run's unit, shared by all of the daemon's steps and
    /// heartbeat reads ([`LauncherTools::sharing`]).
    pub tool_runs: Arc<AtomicU64>,
    /// Where the daemon's log lines go: a stage's failure is said there with its detail
    /// ([`ALERT_STAGE_FAILED`]), which the order's verdict does not carry.
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The alert of a stage that failed or deferred, with the failure's reason and detail (apt's
/// own last line): at error level for a failure, at warn level for a deferral.
pub const ALERT_STAGE_FAILED: &str = "stage_failed";

/// The first number of the daemon's tool units: its process id in the high half, so that a
/// daemon started again never names a unit a run of the one before it may still hold.
pub fn first_tool_run() -> u64 {
    u64::from(std::process::id()) << 32
}

/// Why a version could not be staged.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StageFailure {
    #[error("{0}")]
    Installer(Failure),
    #[error("{0}")]
    Target(TargetError),
    #[error("{0}")]
    Changed(String),
    /// The package store could not give the `.pkg` of a `macos-pkg` entry.
    #[error("{0}")]
    Fetch(PackageError),
}

/// The repository installer's backend of `entry`, `None` for an artifact entry.
pub fn backend_of_entry(entry: &Target) -> Option<&'static dyn Backend> {
    entry
        .installer
        .parse::<Installer>()
        .ok()
        .and_then(backend_of)
}

/// A native installer whose steps the daemon runs itself, with its own ports: the repository
/// installer over a backend (`apt`), or `macos-pkg`.
#[derive(Clone, Copy)]
pub enum Native {
    /// A repository installer's backend.
    Repository(&'static dyn Backend),
    /// The `macos-pkg` installer ([`crate::macos_pkg`]).
    MacosPkg,
}

/// The native installer of `entry`; `None` for an entry of an artifact installer the package
/// store stages (`hostkeeper-archive`).
pub fn native_of_entry(entry: &Target) -> Option<Native> {
    match entry.installer.parse::<Installer>() {
        Ok(Installer::MacosPkg) => Some(Native::MacosPkg),
        _ => backend_of_entry(entry).map(Native::Repository),
    }
}

/// The answer `no_backend` gives for an entry of `installer`.
fn no_backend(installer: &str) -> String {
    format!("the installer {installer:?} has no backend here")
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

/// The reason token of a failure of the stage's own unpack steps, and the refusal it is
/// reported as.
const UNPACK_TOKENS: &[(&str, Refusal)] = &[
    (REASON_DAEMON_TOO_OLD, Refusal::DaemonTooOld),
    (REASON_NO_SPACE, Refusal::NoSpace),
    (REASON_ARTIFACT_CHANGED, Refusal::ArtifactChanged),
    (REASON_RESERVED_VERSION, Refusal::ReservedVersion),
    (REASON_VERSION_TOO_LONG, Refusal::VersionTooLong),
    (REASON_IO_ERROR, Refusal::PackageIoError),
];

/// The refusal of a failure of the staging directory's own steps (the version's name, the seal
/// of one already on the host, the disk): the unpack's reason token as the closed set spells it,
/// `package_failed` for a token this table does not know.
pub fn unpack_refusal(error: &UnpackError) -> Refusal {
    let token = error.reason();
    UNPACK_TOKENS
        .iter()
        .find(|(known, _)| *known == token)
        .map_or(Refusal::PackageFailed, |(_, refusal)| *refusal)
}

/// The switch's error for a step's failure: a failure of the host's files is tried again; any
/// other ends the step with its exit ([`exit_of`]).
pub fn switch_error(failure: Failure) -> SwitchError {
    match failure.reason() {
        Reason::IoError => SwitchError::Failed(failure.to_string()),
        _ => SwitchError::Ended(exit_of(&failure)),
    }
}

/// The names of `order` ([`crate::place::names`]).
fn names(order: &UpdateOrder) -> Result<(ServiceName, Version), SwitchError> {
    crate::place::names(order).map_err(SwitchError::Failed)
}

impl Repository {
    pub(crate) fn placer(&self) -> Placer<'_> {
        Placer::new(self.roots.clone(), &*self.fs, &*self.units)
    }

    /// Runs `step` with the installer of `backend` for `service`.
    fn with<R>(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        step: impl FnOnce(&RepositoryInstaller<'_>) -> R,
    ) -> R {
        let tools = LauncherTools::sharing(&*self.launcher, service.as_str(), &self.tool_runs);
        let records = self.records();
        let lock = InstallerLock::new(&self.ports.lock);
        let pause = ClockPause {
            clock: Arc::clone(&self.clock),
            log: Arc::clone(&self.log),
            service: service.to_string(),
        };
        let repositories = self.ports.repositories.snapshot();
        step(&RepositoryInstaller {
            backend,
            tools: &tools,
            records: &records,
            lock: &lock,
            repositories: &repositories,
            dirs: &self.ports.dirs,
            pause: &pause,
            lock_wait_secs: self.ports.lock_wait_secs,
            mirror: self
                .ports
                .mirror
                .as_deref()
                .map(|mirror| mirror as &dyn Mirror),
        })
    }

    /// The verdict of a stage of `service` that ended with `failure` ([`finished`]), said in
    /// the log with its detail.
    pub(crate) fn stage_failed(&self, service: &ServiceName, failure: &Failure) -> Reply {
        let detail = format!("service={service}: {failure}");
        (self.log)(&match failure.reason().outcome() {
            Outcome::Failed => crate::boot::alert_line(ALERT_STAGE_FAILED, &detail),
            Outcome::Deferred => crate::boot::warn_line(ALERT_STAGE_FAILED, &detail),
        });
        finished(failure)
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

    /// The sealed version directory of `entry` for `service` at `version`, staged by its
    /// native installer `native` when it is not on the host. `Err` is the order's reply; a
    /// refusal of the package store (the fetch of a `.pkg`) is refused with its token.
    pub fn stage(
        &self,
        native: Native,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
    ) -> Result<Staged, Reply> {
        self.stage_with(native, service, version, entry, &|error| {
            Reply::refused(error.refusal(), error)
        })
    }

    /// [`Repository::stage`], with `fetch_failed` the reply to a refusal of the package store
    /// (a force keeps its class: [`crate::verbs::force`]).
    pub fn stage_with(
        &self,
        native: Native,
        service: &ServiceName,
        version: &Version,
        entry: &Target,
        fetch_failed: &dyn Fn(PackageError) -> Reply,
    ) -> Result<Staged, Reply> {
        let backend = match native {
            Native::Repository(backend) => backend,
            Native::MacosPkg => return self.stage_pkg(service, version, entry, fetch_failed),
        };
        let order = self
            .order(backend, service, version, entry)
            .map_err(|failure| self.stage_failed(service, &failure))?;
        let digest = Sha256Hex::parse(&order.pin).ok_or_else(|| {
            Reply::refused(Refusal::OrderInvalid, "the entry's digest is no SHA-256")
        })?;
        let mut fill = |stage: &Path| {
            self.with(backend, service, |installer| installer.stage(&order, stage))
                .map(drop)
                .map_err(StageFailure::Installer)
        };
        self.fill_sealed(service, version, &digest, entry, &mut fill, fetch_failed)
    }

    /// The sealed version directory of `entry` for `service` at `version`, whose package has
    /// `digest`: the one on the host when it is sealed for the same entry (another entry is
    /// `target_changed`), or one staged by `fill` into an empty staging directory, the entry
    /// written beside it as `target.json`, sealed and published. `Err` is the order's reply.
    pub(crate) fn fill_sealed(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &Sha256Hex,
        entry: &Target,
        fill: &mut dyn FnMut(&Path) -> Result<(), StageFailure>,
        fetch_failed: &dyn Fn(PackageError) -> Reply,
    ) -> Result<Staged, Reply> {
        let fill_order = FillOrder {
            service,
            version,
            package_digest: digest,
        };
        let mut same = |dir: &Path| match target::read(dir) {
            Ok(Some(sealed)) if sealed == *entry => Ok(()),
            Ok(_) => Err(StageFailure::Changed(format!(
                "{version} of {service} is on the host for another entry than the order's"
            ))),
            Err(error) => Err(StageFailure::Target(error)),
        };
        let mut fill = |stage: &Path| {
            fill(stage)?;
            target::write(stage, entry).map_err(StageFailure::Target)
        };
        let packages = HostFs::open(&self.ports.packages_dir, self.ports.owner)
            .map_err(|error| Reply::refused(Refusal::PackageIoError, error))?;
        let filled = stage_filled(&fill_order, &packages, &mut same, &mut fill).map_err(
            |error| match error {
                FillError::Unpack(error) => Reply::refused(unpack_refusal(&error), error),
                FillError::Fill(StageFailure::Installer(failure)) => {
                    self.stage_failed(service, &failure)
                }
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
                FillError::Fill(StageFailure::Fetch(error)) => fetch_failed(error),
            },
        )?;
        Staged::seal(&filled.dir).map_err(|error| Reply::refused(Refusal::ReleaseUnreadable, error))
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
        self.activate_installed(service, version, dir, entry)
            .map_err(switch_error)
    }

    /// [`Repository::activate_version`] with the installer's failure as it is: what the new
    /// binary of hostkeeper's own update runs after it confirmed itself
    /// ([`crate::own_package`]). A version staged for an installer with no backend here, and a
    /// `current` that cannot be switched, are `io_error`: nothing of the package said so.
    ///
    /// A service an archive version of an earlier release placed paths for (the move,
    /// docs/design/packages-native.md section 10.6) has them taken away first, and systemd
    /// reloaded ([`Placer::take_away_archive_paths`]); after the installer's `activate`, each unit
    /// the archive placed that the package installs must be loaded from the package's own file
    /// ([`loaded::check_units`]: `unit_not_loaded` otherwise), and only then are the paths
    /// dropped from `placed.json` ([`Placer::forget_archive_paths`]), so a failed or deferred
    /// `activate` takes them away and checks again on its next try. An installer's `activate`
    /// that fails or defers puts the links it removed back first ([`Repository::put_back`]). A
    /// failure of the removal or of the record is `io_error`.
    pub fn activate_installed(
        &self,
        service: &ServiceName,
        version: &Version,
        dir: &Path,
        entry: &Target,
    ) -> Result<(), Failure> {
        if let Some(Native::MacosPkg) = native_of_entry(entry) {
            return self.activate_pkg(service, version, dir, entry);
        }
        let backend = backend_of_entry(entry).ok_or_else(|| {
            Failure::new(
                Reason::IoError,
                format!(
                    "{version} of {service} was staged for the installer {:?}, which has no backend here",
                    entry.installer
                ),
            )
        })?;
        let order = self.order(backend, service, version, entry)?;
        let archive = |e: PlacedError| {
            Failure::new(
                Reason::IoError,
                format!("the paths of the archive version: {e}"),
            )
        };
        let taken = self
            .placer()
            .take_away_archive_paths(service.as_str())
            .map_err(archive)?;
        let activated = self
            .with(backend, service, |installer| {
                installer.activate(&order, dir)
            })
            .map_err(|failure| self.put_back(&taken, failure))?;
        if let Some(drift) = activated.drift {
            let mut repaired = self.repaired.lock().unwrap_or_else(PoisonError::into_inner);
            repaired.insert(service.to_string(), drift);
        }
        if !taken.units.is_empty() {
            let tools = LauncherTools::sharing(&*self.launcher, service.as_str(), &self.tool_runs);
            loaded::check_units(&tools, &dir.join(TREE), &taken.units, DRIFT_QUERY_SECS)?;
        }
        self.placer()
            .forget_archive_paths(service.as_str())
            .map_err(archive)?;
        self.placer()
            .switch_current(service, version)
            .map(drop)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))
    }

    /// `failure`, the installer's `activate` that failed or deferred after `taken` was taken
    /// away, once the links `taken` removed are put back
    /// ([`Placer::restore_archive_links`]): with no native version installed, the archive's
    /// unit is still the one systemd starts the service by. A put-back that fails is said in the
    /// failure's detail; its reason stays the installer's.
    fn put_back(&self, taken: &hostkeeper_placed::TakenAway, failure: Failure) -> Failure {
        match self.placer().restore_archive_links(taken) {
            Ok(_) => failure,
            Err(e) => Failure::new(
                failure.reason(),
                format!(
                    "{}; the archive's links were not put back: {e}",
                    failure.detail()
                ),
            ),
        }
    }

    /// The adoption of a package the host's setup installed with no record
    /// (docs/design/packages-native.md, section 10.4, "The bootstrap install is adopted"): under
    /// the installer lock, the version of `package` the package manager shows; when it is
    /// installed whole and `accept` takes its version, the package is held, then recorded as
    /// installed by `backend` for `service`, from no repository of the host (an empty
    /// `repository`), with no link. The version adopted; `None`, and nothing done, when the
    /// package is not installed, not whole, or at a version `accept` refuses.
    pub fn adopt(
        &self,
        backend: &dyn Backend,
        service: &ServiceName,
        package: &str,
        accept: &dyn Fn(&str) -> bool,
    ) -> Result<Option<String>, Failure> {
        let held = InstallerLock::new(&self.ports.lock)
            .hold()
            .map_err(|e| Failure::new(Reason::IoError, format!("the installer lock: {e}")))?;
        let tools = LauncherTools::sharing(&*self.launcher, service.as_str(), &self.tool_runs);
        let installed = backend.installed(&tools, package, DRIFT_QUERY_SECS)?;
        let Some(installed) = installed.filter(|found| found.complete & accept(&found.version))
        else {
            return Ok(None);
        };
        backend.hold(&tools, package, true, DRIFT_QUERY_SECS)?;
        let record = PackageRecord {
            installer: backend.installer().as_str().to_owned(),
            package: package.to_owned(),
            version: installed.version.clone(),
            repository: String::new(),
            links: Default::default(),
            pending: false,
        };
        self.records()
            .save(service, &record)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))?;
        drop(held);
        Ok(Some(installed.version))
    }

    /// The record of `service` written whole again when an `activate` left it `pending`: the
    /// `activate` of hostkeeper's own package failed, and the version it was to install, against
    /// the one the package manager still shows, is the drift the heartbeat reports
    /// (docs/design/packages-native.md, section 10.4, step 5). `false` when there was none.
    pub fn unpend(&self, service: &ServiceName) -> Result<bool, RecordError> {
        let records = self.records();
        let pending = records
            .placer()
            .store()
            .load()
            .map_err(record_error)?
            .installed
            .get(service.as_str())
            .is_some_and(|entry| entry.pending);
        if pending {
            records.change(|installed| {
                installed
                    .entry(service.to_string())
                    .and_modify(|entry| entry.pending = false);
            })?;
        }
        Ok(pending)
    }

    /// The `installed` entries of `placed.json` as the repository installer reads them.
    pub(crate) fn records(&self) -> PlacedRecords {
        PlacedRecords {
            roots: self.roots.clone(),
            fs: Arc::clone(&self.fs),
            units: Arc::clone(&self.units),
        }
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
        if native_of_entry(entry).is_none() {
            return Err(failed(no_backend(&entry.installer)));
        }
        self.remove(&service, entry).map_err(switch_error)?;
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
        if let Some(Native::MacosPkg) = native_of_entry(entry) {
            return self.remove_pkg(service, entry);
        }
        let remove_secs = entry
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.installer_bounds)
            .map_or(REMOVE_SECS_WITHOUT_BOUNDS, |bounds| bounds.remove_secs);
        let backend = backend_of_entry(entry)
            .ok_or_else(|| Failure::new(Reason::OrderInvalid, no_backend(&entry.installer)))?;
        self.with(backend, service, |installer| {
            installer.remove(service, remove_secs)
        })
        .map(drop)
    }

    /// The drift of `service`, whose `current` version holds `entry`, at this heartbeat: `None`
    /// when there is none, or when the installed version cannot be read (said by `log`).
    pub fn drift(&self, service: &ServiceName, entry: &Target) -> Result<Option<Drift>, Failure> {
        if let Some(Native::MacosPkg) = native_of_entry(entry) {
            return self.drift_pkg(service, entry);
        }
        let backend = backend_of_entry(entry)
            .ok_or_else(|| Failure::new(Reason::OrderInvalid, no_backend(&entry.installer)))?;
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

#[cfg(test)]
#[path = "repository_tests.rs"]
mod tests;
