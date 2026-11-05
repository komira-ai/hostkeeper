//! The daemon's service observer (design section 5.8): what [`Daemon::status`] reports of how
//! each installed service runs, and the real adapters it looks through.
//!
//! - [`service_input`] reads what the observer needs of an installed service: the version
//!   `current` names, and the manifest of that version. A service with no `current`, or whose
//!   manifest does not load, has no input and is not observed: the daemon does not guess.
//! - [`observe`] makes one observation of such a service, with [`STATUS_DEADLINE`] for each
//!   probe. It takes no lock of the daemon's.
//! - [`SealedExe`] is the observer's [`ExpectedExe`] over the package tree: the digest of the
//!   executable the first `[units] run` unit of a version starts, found through its unit file.
//! - [`host_observer`] builds the observer of a host over `systemctl`, `/proc` and TCP.
//! - [`service_health`] is the heartbeat's health of a service's observation.
//!
//! [`Daemon::status`]: crate::executor::Daemon::status

use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, UnitName, Version};
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{
    Current, Deadline, ExpectedExe, HttpHealthClient, Observation, ObserveOptions, Observer,
    ProcProbe, ProcessRunner, ServiceInput, SystemdManager, TcpConnect, beat_health,
};
use hostkeeper_placed::lint::COMMAND_PREFIXES;
use hostkeeper_proto::hostkeeper::agent::v1::Health;

use crate::executor::ServiceStatus;
use crate::installed::{self, CURRENT};

/// How long each probe of a status read may wait, to connect and for each read after that.
pub const STATUS_DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(1),
};

/// How long one `systemctl show` may take.
const SYSTEMCTL_DEADLINE: Duration = Duration::from_secs(5);

/// How many bytes of a `systemctl show` answer are read.
const SYSTEMCTL_OUTPUT_CAP: usize = 64 * 1024;

/// The `/proc` tree of the host.
const PROC: &str = "/proc";

/// The part of `service` the observer reads: the version `current` names as the recorded
/// version, and the manifest of that version. `None` when there is no `current`, or its manifest
/// does not load for `own`.
pub fn service_input(
    packages: &Path,
    own: &HostkeeperVersion,
    service: &ServiceName,
) -> Option<ServiceInput> {
    let version = installed::linked_version(packages, service, CURRENT)?;
    let dir = packages.join(service.as_str()).join(version.as_str());
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
    };
    let manifest = Manifest::load(&dir, &expected, own).ok()?;
    Some(ServiceInput {
        service: service.to_string(),
        current: Some(Current::from_manifest(&manifest)),
        recorded_version: Some(version.to_string()),
    })
}

/// One observation of `service`, `None` when it has no input ([`service_input`]).
pub fn observe(
    observer: &Observer,
    packages: &Path,
    own: &HostkeeperVersion,
    service: &ServiceName,
) -> Option<Observation> {
    service_input(packages, own, service).map(|input| observer.observe(&input, STATUS_DEADLINE))
}

/// The health the heartbeat reports of the service `status` describes, and its version, as
/// [`beat_health`] maps its observation; `None` when it was not observed.
pub fn service_health(status: &ServiceStatus) -> Option<(Health, Option<String>)> {
    status.observation.as_ref().map(beat_health)
}

/// The digests of the executables sealed with each version, read from the package tree at
/// `packages`: the file the first `[units] run` unit of the version starts, named by the
/// `ExecStart=` of its unit file (the template's file for an instance), which must go through
/// `<packages>/<service>/current/` as the host spells the packages directory. The file is the
/// one at the same place under the version's own directory.
///
/// A version has no digest when it has no seal (`<version>.seal`), its manifest does not load,
/// it runs no unit, the command is not through `current` or has a `..`, or the file cannot be
/// read.
#[derive(Debug)]
pub struct SealedExe {
    packages: PathBuf,
    host_packages: PathBuf,
    own: HostkeeperVersion,
}

impl SealedExe {
    /// The sealed executables under `packages`, whose unit files spell that directory
    /// `host_packages`, for a daemon of version `own`.
    pub fn new(packages: &Path, host_packages: &Path, own: HostkeeperVersion) -> SealedExe {
        SealedExe {
            packages: packages.to_path_buf(),
            host_packages: host_packages.to_path_buf(),
            own,
        }
    }

    /// Where the executable of `version` of `service` is.
    fn exe_path(&self, service: &str, version: &str) -> Option<PathBuf> {
        let expected = Expected {
            name: ServiceName::new(service).ok()?,
            version: Version::new(version).ok()?,
        };
        let service_dir = self.packages.join(service);
        let sealed = fs::metadata(service_dir.join(format!("{version}.seal")))
            .is_ok_and(|seal| seal.is_file());
        sealed.then_some(())?;
        let dir = service_dir.join(version);
        let manifest = Manifest::load(&dir, &expected, &self.own).ok()?;
        let unit = manifest.run.first()?;
        let template = UnitName::of(service, unit, false)
            .ok()
            .and_then(|name| name.template());
        let text = std::iter::once(unit.clone())
            .chain(template)
            .find_map(|file| fs::read_to_string(dir.join("units").join(file)).ok());
        let word = text.as_deref().and_then(exec_start)?;
        let through = self.host_packages.join(service).join(CURRENT);
        let rest = Path::new(word)
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

/// The observer of a host: `systemctl show` for the units, `/proc` for the processes, TCP to
/// the loopback for health, and the executables sealed under `packages_dir` ([`SealedExe`], the
/// unit files spelling that directory as it is) to confirm the version, for a daemon of version
/// `own`. Making it reads nothing.
pub fn host_observer(packages_dir: &Path, own: HostkeeperVersion) -> Observer {
    Observer::new(
        hostkeeper_observe::Ports {
            manager: Box::new(SystemdManager::new(
                ProcessRunner::new(SYSTEMCTL_OUTPUT_CAP),
                SYSTEMCTL_DEADLINE,
            )),
            process: Box::new(ProcProbe::new(Path::new(PROC))),
            health: Box::new(HttpHealthClient::new(TcpConnect)),
            expected: Box::new(SealedExe::new(packages_dir, packages_dir, own)),
        },
        ObserveOptions::default(),
    )
}
