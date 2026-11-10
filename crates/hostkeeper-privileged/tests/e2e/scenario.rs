//! The states the end-to-end tests start from, each built by every test the same way, so that the
//! tests do not depend on each other or on their order.
//!
//! [`Scenario::start`] brings up the whole rig: the host root and the host's fixed paths, the
//! apt repository with the three fixture versions published and the host's entry of it, the
//! coordinator in process, the daemon under a `Type=notify` unit, and the daemon's enrollment
//! approved. [`Scenario::installed`] then has the coordinator install `1.0.0` (an `install`
//! order, the hooked path with no previous release), and [`Scenario::updated`] goes on to `1.1.0`
//! with the drain gate idle, which leaves `current` at `1.1.0`, `previous` at `1.0.0`, both on
//! disk with their seals, and `1.1.0` the coordinator's last good version.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::Value;

use super::aptrepo::AptRepo;
use super::coordinator::CoordinatorHandle;
use super::host_root::{DaemonUnit, HostRoot};
use super::package::{BuiltPackage, FixtureVersion, PackageBuilder};
use super::{FIXTURE_BINARY, HOST_ID, HeldPort, ROLE, SERVICE, served_version, wait_for};

/// The first release.
pub const V1: &str = "1.0.0";
/// A good update.
pub const V2: &str = "1.1.0";
/// An update whose `verify` fails.
pub const V3: &str = "1.2.0";

/// Tries for a state reached through the coordinator: its quiet start (30 seconds, a floor), the
/// enrollment and a heartbeat per order or report, at a pause of half a second per try.
pub const COORDINATED: u32 = 600;
/// Tries for a state the host reaches on its own once ordered (a step, a switch, a unit).
pub const LOCAL: u32 = 240;

/// The config of a daemon that no order of this rig makes read a key of: the loader requires the
/// release channel and the trusted signers, and the daemon reads neither.
const CONFIG_TAIL: &str = "release_channel = \"https://releases.example\"\n\n\
     [trusted_signers]\norganisation = \"example-org\"\nworkflow = \".github/workflows/release.yml\"\n\
     refs = [\"refs/tags/*\"]\nhosted_runner = true\n";

/// The rig of one test. The fields are dropped in this order: the daemon first (its unit stopped
/// and unlinked), then the coordinator and the repository, the host root (every unit of the
/// fixture stopped, its package purged and every path the test made on the host removed), and the
/// held ports last, once no unit of the fixture can bind them again.
pub struct Scenario {
    pub daemon: DaemonUnit,
    pub coordinator: CoordinatorHandle,
    pub repo: AptRepo,
    pub packages: BTreeMap<&'static str, BuiltPackage>,
    pub host: HostRoot,
    /// The ports of the fixture's instances, two per version, held from before the coordinator
    /// binds its listeners until the test ends: a port only chosen and let go could be the one
    /// such a listener is given, and the instance on it would fail to start.
    pub ports: Vec<HeldPort>,
}

/// The pinned `gh` the workflow installed (its step `Install gh`), where the test finds it: next
/// to cargo's scratch directory for tests, in the target directory.
pub fn pinned_gh() -> PathBuf {
    let gh = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("the target directory")
        .join("privileged-tools")
        .join("gh");
    assert!(
        gh.is_file(),
        "the pinned gh is not at {}: install it as privileged.yml's step `Install gh` does",
        gh.display()
    );
    gh
}

/// Held loopback ports, two per version (distinct, since the system gives a held port to no
/// other socket), and their numbers in pairs.
fn held_pairs(count: usize) -> (Vec<HeldPort>, Vec<[u16; 2]>) {
    let held: Vec<HeldPort> = (0..count * 2).map(|_| HeldPort::new()).collect();
    let pairs = held
        .chunks(2)
        .map(|pair| [pair[0].port(), pair[1].port()])
        .collect();
    (held, pairs)
}

/// A failed test's view of the coordinator: its status and convergence of the service, printed
/// and kept with the host root's artifacts, while the coordinator still runs (this drop runs
/// before the fields' drops). It tells which of the coordinator's rules held an order back.
impl Drop for Scenario {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let view = format!(
            "releases status:\n{}\nreleases converge:\n{}\nstep record:\n{:#?}\n",
            self.coordinator.status_text(SERVICE),
            self.coordinator.converge_text(SERVICE),
            self.host.step_record(),
        );
        eprintln!("the coordinator's view when the test failed:\n{view}");
        let dir = self.host.artifact_dir();
        if fs::create_dir_all(&dir).is_ok() {
            drop(fs::write(dir.join("coordinator.txt"), view));
        }
    }
}

impl Scenario {
    /// The whole rig up, the daemon enrolled and approved as [`HOST_ID`], nothing installed.
    /// `tag` (a few lowercase letters) names the test's host root and daemon unit.
    pub fn start(tag: &str) -> Scenario {
        let host = HostRoot::new(tag);
        let repo = AptRepo::new(host.base(), "tierstore-fixture");
        host.install_repository(&repo);

        let builder = PackageBuilder::new(&host.records(), Path::new(FIXTURE_BINARY));
        let versions = [(V1, false), (V2, false), (V3, true)];
        let mut packages = BTreeMap::new();
        let (ports, pairs) = held_pairs(versions.len());
        for ((version, fail_verify), ports) in versions.into_iter().zip(pairs) {
            let fixture = FixtureVersion {
                version,
                ports,
                fail_verify,
            };
            packages.insert(version, builder.build(&fixture, &repo));
        }

        let coordinator = CoordinatorHandle::start(&host.base().join("coordinator"), SERVICE, ROLE);
        host.write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\n{CONFIG_TAIL}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
        ));
        let daemon = host.start_daemon(&format!("hostkeeper-ci-{tag}-{}-daemon", process::id()));
        coordinator.approve_enrollment(HOST_ID, SERVICE);
        Scenario {
            daemon,
            coordinator,
            repo,
            packages,
            host,
            ports,
        }
    }

    /// [`Scenario::start`], then `1.0.0` installed by the coordinator's `install` order: the
    /// coordinator reports the host converged on it and both its instances serve it.
    pub fn installed(tag: &str) -> Scenario {
        let scenario = Scenario::start(tag);
        scenario.order(V1);
        scenario.wait_converged(V1);
        scenario.wait_serving(V1);
        scenario
    }

    /// [`Scenario::installed`], then the update to `1.1.0` with the drain gate idle: the host
    /// converged on it, its instances serving it, and `1.1.0` the coordinator's last good version.
    pub fn updated(tag: &str) -> Scenario {
        let scenario = Scenario::installed(tag);
        scenario.host.set_gate(false);
        scenario.order(V2);
        scenario.wait_converged(V2);
        scenario.wait_serving(V2);
        wait_for(
            "the coordinator records 1.1.0 as last good",
            COORDINATED,
            || scenario.status()["marks"]["lastGood"] == V2,
        );
        scenario
    }

    /// The package of `version`.
    pub fn package(&self, version: &str) -> &BuiltPackage {
        self.packages.get(version).expect("a fixture version")
    }

    /// Sets the desired version of the service to `version` by targets alone, as CI's set of a
    /// release whose entries are all native does: the one `apt` entry of its package for the host's
    /// platform, through the operator API.
    pub fn order(&self, version: &str) {
        let targets = self.package(version).targets();
        self.coordinator
            .set_desired_targets_alone(SERVICE, version, targets);
    }

    /// The coordinator's status of the service (`releases status`).
    pub fn status(&self) -> Value {
        self.coordinator.status(SERVICE)
    }

    /// The coordinator's convergence of the service (`releases converge`).
    pub fn convergence(&self) -> Value {
        self.coordinator.converge(SERVICE)
    }

    /// The test host as the service sees it, `Null` before its first heartbeat lists it.
    pub fn host_row(&self) -> Value {
        self.status()["hosts"]
            .as_array()
            .and_then(|hosts| hosts.iter().find(|host| host["nodeId"] == HOST_ID))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// What the host's last heartbeat said of its last update of the service.
    pub fn last_update(&self) -> Value {
        self.host_row()["beat"]["lastUpdate"].clone()
    }

    /// Whether the coordinator reports the service converged on `version`, the host running it.
    pub fn converged_on(&self, version: &str) -> bool {
        let convergence = self.convergence();
        convergence["state"] == "CONVERGENCE_STATE_CONVERGED"
            && convergence["desired"] == version
            && self.host_row()["beat"]["version"] == version
    }

    /// Waits until [`Scenario::converged_on`] `version`.
    pub fn wait_converged(&self, version: &str) {
        wait_for(
            &format!("the coordinator reports the host converged on {version}"),
            COORDINATED,
            || self.converged_on(version),
        );
    }

    /// Waits until both instances of `version` answer `/healthz` with `version=<version>`.
    pub fn wait_serving(&self, version: &str) {
        let ports = self.package(version).ports;
        wait_for(
            &format!("both instances of {version} serve it"),
            LOCAL,
            || {
                ports
                    .iter()
                    .all(|port| served_version(*port).as_deref() == Some(version))
            },
        );
    }

    /// The target of the link `name` (`current`, `previous`) of the service's package directory.
    pub fn link(&self, name: &str) -> Option<String> {
        fs::read_link(self.host.service_dir().join(name))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The version directories of the service on disk: every real directory in its package
    /// directory but the in-flight ones (a leading `.`), links and seals left out.
    pub fn version_dirs(&self) -> BTreeSet<String> {
        fs::read_dir(self.host.service_dir())
            .expect("the service's package directory")
            .map(|entry| entry.expect("an entry"))
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect()
    }

    /// The seal of `version` as JSON (`{"schema", "tree_digest", "package_digest"}`, the last the
    /// package's pin).
    pub fn seal(&self, version: &str) -> Value {
        let path = self.host.service_dir().join(format!("{version}.seal"));
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_slice(&bytes).expect("a seal is JSON")
    }
}
