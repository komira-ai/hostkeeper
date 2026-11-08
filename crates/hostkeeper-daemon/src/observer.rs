//! The daemon's service observer (design section 5.8): what [`Daemon::status`] reports of how
//! each installed service runs, and the real adapters it looks through.
//!
//! - [`service_input`] reads what the observer needs of an installed service: the version
//!   `current` names, and the manifest of that version. A service with no `current`, or whose
//!   manifest does not load, has no input and is not observed: the daemon does not guess.
//! - [`observe`] makes one observation of such a service, with [`STATUS_DEADLINE`] for each
//!   probe. It takes no lock of the daemon's.
//! - [`SealedExe`] is the observer's [`ExpectedExe`] over the package tree: the digest of the
//!   executable the first `[units] run` unit of a version starts, found through its unit file
//!   (`ExecStart=`) or its launchd plist (`Program`, else the first `ProgramArguments` string);
//!   for the daemon's own service, `hostkeeper`, the file of the version in the binary directory
//!   its unit starts `current` of ([`SealedExe::with_binary_dir`]).
//! - [`OwnService`] is how the daemon itself runs on the host: the unit or launchd job it runs
//!   as, and its binary directory. The observer watches that unit as the service `hostkeeper`
//!   whenever the package's manifest names no `[units] run` (a Mac's package has none: the
//!   installer pkg places the daemon's job, `dev.hostkeeper.hostkeeper`), so the daemon reports
//!   its own service live, healthy and at the version it runs on every platform, as it reports
//!   `hostkeeper.service` on Linux.
//! - [`host_observer`] builds the observer of a host over `systemctl`, `/proc` and TCP.
//! - [`service_health`] is the heartbeat's health of a service's observation.
//!
//! [`Daemon::status`]: crate::executor::Daemon::status

use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{PLIST_SUFFIX, ServiceName, UNIT_SUFFIXES, UnitName, Version};
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{
    Current, Deadline, ExpectedExe, HttpHealthClient, Observation, ObserveOptions, Observer,
    ProcProbe, ProcessProbe, ProcessRunner, ServiceInput, ServiceManager, SystemdManager,
    TcpConnect, beat_health,
};
use hostkeeper_placed::lint::{COMMAND_PREFIXES, plist_program};
use hostkeeper_proto::hostkeeper::agent::v1::Health;

use hostkeeper_steps::Layout;

use crate::executor::ServiceStatus;
use crate::installed::{self, CURRENT};
use crate::verbs::force::SELF_SERVICE;

/// How long each probe of a status read may wait, to connect and for each read after that.
pub const STATUS_DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};

/// How long one `systemctl show`, or one `rc-service <service> status` on OpenRC, may take.
pub const SHOW_DEADLINE: Duration = Duration::from_secs(5);

/// How many bytes of a `systemctl show` (or an `rc-service` status) answer are read.
pub const SHOW_OUTPUT_CAP: usize = 64 * 1024;

/// The `/proc` tree of the host.
const PROC: &str = "/proc";

/// The part of `service` the observer reads: the version `current` names as the recorded
/// version, and the manifest of that version. `None` when there is no `current`, or its manifest
/// does not load for `own`.
///
/// For the daemon's own service, `hostkeeper`, the recorded version is the one `current` of
/// the binary directory `binaries` names, the binary its unit starts, when it names one: the
/// package's `current` names the release the last update placed, which is not the binary that
/// runs between the switch and the restart, nor after a fallback put the previous binary back.
pub fn service_input(
    packages: &Path,
    binaries: &Path,
    own: &HostkeeperVersion,
    service: &ServiceName,
) -> Option<ServiceInput> {
    let version = installed::linked_version(packages, service, CURRENT)?;
    let dir = packages.join(service.as_str()).join(version.as_str());
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
        platform: hostkeeper_config::platform::Platform::current(),
    };
    let manifest = Manifest::load(&dir, &expected, own).ok()?;
    let started = (service.as_str() == SELF_SERVICE)
        .then(|| Layout::new(binaries).current().ok().flatten())
        .flatten();
    Some(ServiceInput {
        service: service.to_string(),
        current: Some(Current::from_manifest(&manifest)),
        recorded_version: Some(started.unwrap_or_else(|| version.to_string())),
    })
}

/// One observation of `service`, `None` when it has no input ([`service_input`]).
pub fn observe(
    observer: &Observer,
    packages: &Path,
    binaries: &Path,
    own: &HostkeeperVersion,
    service: &ServiceName,
) -> Option<Observation> {
    service_input(packages, binaries, own, service)
        .map(|input| observer.observe(&input, STATUS_DEADLINE))
}

/// The health the heartbeat reports of the service `status` describes, and its version, as
/// [`beat_health`] maps its observation; `None` when it was not observed.
pub fn service_health(status: &ServiceStatus) -> Option<(Health, Option<String>)> {
    status.observation.as_ref().map(beat_health)
}

/// The digests of the executables sealed with each version, read from the package tree at
/// `packages`: the file the first `[units] run` unit of the version starts, named by the
/// `ExecStart=` of its unit file (the template's file for an instance) for a systemd unit, and
/// for a launchd label by the `Program` of `units/<label>.plist`, else its first
/// `ProgramArguments` string ([`plist_program`]). The path must go through
/// `<packages>/<service>/current/` as the host spells the packages directory, and the rest of it
/// be plain names. The file is the one at the same place under the version's own directory.
///
/// A version has no digest when it has no seal (`<version>.seal`), its manifest does not load,
/// it runs no unit, the command is not through `current` or has a `..`, or the file cannot be
/// read.
///
/// Given the daemon's binary directory ([`SealedExe::with_binary_dir`]), a version of the
/// service `hostkeeper` is instead the file `versions/<version>` of that directory: the copy of
/// the release's binary the self-update staged (or the first binary the deb or the pkg seeded),
/// which the daemon's unit starts through the directory's `current` on both platforms, whether
/// or not the package of that version is still installed. It has no digest when that file
/// cannot be read.
#[derive(Debug)]
pub struct SealedExe {
    packages: PathBuf,
    host_packages: PathBuf,
    own: HostkeeperVersion,
    binaries: Option<PathBuf>,
}

impl SealedExe {
    /// The sealed executables under `packages`, whose unit files spell that directory
    /// `host_packages`, for a daemon of version `own`.
    pub fn new(packages: &Path, host_packages: &Path, own: HostkeeperVersion) -> SealedExe {
        SealedExe {
            packages: packages.to_path_buf(),
            host_packages: host_packages.to_path_buf(),
            own,
            binaries: None,
        }
    }

    /// The same sealed executables, the service `hostkeeper`'s found in the binary directory
    /// `binary_dir` (`<state-dir>/hostkeeper/bin`, [`crate::selfupdate::binary_dir`]).
    #[must_use]
    pub fn with_binary_dir(mut self, binary_dir: &Path) -> SealedExe {
        self.binaries = Some(binary_dir.to_path_buf());
        self
    }

    /// Where the executable of `version` of `service` is.
    fn exe_path(&self, service: &str, version: &str) -> Option<PathBuf> {
        let expected = Expected {
            name: ServiceName::new(service).ok()?,
            version: Version::new(version).ok()?,
            platform: hostkeeper_config::platform::Platform::current(),
        };
        if let Some(binaries) = self.binaries.as_deref().filter(|_| service == SELF_SERVICE) {
            return Some(Layout::new(binaries).version_path(expected.version.as_str()));
        }
        let service_dir = self.packages.join(service);
        let sealed = fs::metadata(service_dir.join(format!("{version}.seal")))
            .is_ok_and(|seal| seal.is_file());
        sealed.then_some(())?;
        let dir = service_dir.join(version);
        let manifest = Manifest::load(&dir, &expected, &self.own).ok()?;
        let unit = manifest.run.first()?;
        let word = if UNIT_SUFFIXES.iter().any(|suffix| unit.ends_with(suffix)) {
            let template = UnitName::of(service, unit, false)
                .ok()
                .and_then(|name| name.template());
            let text = std::iter::once(unit.clone())
                .chain(template)
                .find_map(|file| fs::read_to_string(dir.join("units").join(file)).ok());
            text.as_deref().and_then(exec_start).map(str::to_owned)?
        } else {
            let file = dir.join("units").join(format!("{unit}{PLIST_SUFFIX}"));
            plist_program(&fs::read_to_string(file).ok()?)?
        };
        let through = self.host_packages.join(service).join(CURRENT);
        let rest = Path::new(&word)
            .strip_prefix(through)
            .ok()
            .filter(|rest| is_plain(rest))?;
        Some(dir.join(rest))
    }
}

/// The program of the first `ExecStart=` line of a systemd unit: its first word, without the
/// prefixes systemd reads before it.
fn exec_start(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))?
        .trim_start_matches(COMMAND_PREFIXES)
        .split_whitespace()
        .next()
}

/// Whether `path` is one or more plain names: no `..`, no `.`, nothing absolute.
fn is_plain(path: &Path) -> bool {
    let mut parts = path.components().peekable();
    parts.peek().is_some() & parts.all(|part| matches!(part, Component::Normal(_)))
}

impl ExpectedExe for SealedExe {
    fn digest(&self, service: &str, version: &str) -> Option<[u8; 32]> {
        let file = File::open(self.exe_path(service, version)?).ok()?;
        digest_open_file(&file).ok()
    }
}

/// How the daemon itself runs on its host: the unit (Linux) or launchd job (macOS) it runs as,
/// which the platform's adapters name (`crate::adapters::Adapters::own_unit`), and the binary
/// directory that unit starts `current` of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnService {
    /// The unit or job the daemon runs as: `hostkeeper.service`, or `dev.hostkeeper.hostkeeper`.
    pub unit: String,
    /// `<state-dir>/hostkeeper/bin` ([`crate::selfupdate::binary_dir`]).
    pub binary_dir: PathBuf,
}

/// The observer of a Linux host: `systemctl show` for the units, `/proc` for the processes, TCP
/// to the loopback for health, and the executables sealed under `packages_dir` ([`SealedExe`],
/// the unit files spelling that directory as it is) to confirm the version, for a daemon of
/// version `own` that runs as `own_service`. Making it reads nothing.
pub fn host_observer(
    packages_dir: &Path,
    own: HostkeeperVersion,
    own_service: &OwnService,
) -> Observer {
    observer_with(
        Box::new(systemd_manager()),
        Box::new(proc_probe()),
        packages_dir,
        own,
        own_service,
    )
}

/// The observer of a host whose units `manager` reads and whose processes `process` reads:
/// otherwise as [`host_observer`]. The service `hostkeeper` is observed through
/// `own_service`'s unit when its manifest names none, and its version confirmed against the
/// file in `own_service`'s binary directory. Making it reads nothing.
pub fn observer_with(
    manager: Box<dyn ServiceManager>,
    process: Box<dyn ProcessProbe>,
    packages_dir: &Path,
    own: HostkeeperVersion,
    own_service: &OwnService,
) -> Observer {
    let expected =
        SealedExe::new(packages_dir, packages_dir, own).with_binary_dir(&own_service.binary_dir);
    Observer::new(
        hostkeeper_observe::Ports {
            manager,
            process,
            health: Box::new(HttpHealthClient::new(TcpConnect)),
            expected: Box::new(expected),
        },
        ObserveOptions::default(),
    )
    .with_run_of(SELF_SERVICE, vec![own_service.unit.clone()])
}

/// The host's systemd: `systemctl show`, bounded in time and output.
pub fn systemd_manager() -> SystemdManager<ProcessRunner> {
    SystemdManager::new(ProcessRunner::new(SHOW_OUTPUT_CAP), SHOW_DEADLINE)
}

/// The host's `/proc`.
pub fn proc_probe() -> ProcProbe {
    ProcProbe::new(Path::new(PROC))
}
