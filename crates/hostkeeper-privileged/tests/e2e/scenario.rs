//! The states the end-to-end tests start from, each built by every test the same way, so that the
//! tests do not depend on each other or on their order.
//!
//! [`Scenario::start`] brings up the whole rig: the host root and the host's fixed paths, the
//! pinned `gh` and the fixture signer's trusted root, the release channel with the three fixture
//! versions published, the coordinator in process, the daemon under a `Type=notify` unit, and the
//! daemon's enrollment approved. [`Scenario::installed`] then has the coordinator install `1.0.0`
//! (an `install` order, the hooked path with no previous release), and [`Scenario::updated`] goes
//! on to `1.1.0` with the drain gate idle, which leaves `current` at `1.1.0`, `previous` at
//! `1.0.0`, both on disk with their seals, and `1.1.0` the coordinator's last good version.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::Value;

use super::channel::ReleaseChannel;
use super::coordinator::CoordinatorHandle;
use super::host_root::{DaemonUnit, HostRoot};
use super::package::{BuiltPackage, FixtureVersion, PackageBuilder};
use super::signer::FixtureSigner;
use super::{FIXTURE_BINARY, HOST_ID, ROLE, SERVICE, free_port, served_version, wait_for};

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

/// The rig of one test. The fields are dropped in this order: the daemon first (its unit stopped
/// and unlinked), then the coordinator and the channel, the host root last (every unit of the
/// fixture stopped and every path the test made on the host removed).
pub struct Scenario {
    pub daemon: DaemonUnit,
    pub coordinator: CoordinatorHandle,
    pub channel: ReleaseChannel,
    pub signer: FixtureSigner,
    pub packages: BTreeMap<&'static str, BuiltPackage>,
    pub host: HostRoot,
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

/// Distinct free loopback ports, two per version.
fn port_pairs(count: usize) -> Vec<[u16; 2]> {
    let mut seen = BTreeSet::new();
    let mut ports = Vec::new();
    while ports.len() < count * 2 {
        let port = free_port();
        if seen.insert(port) {
            ports.push(port);
        }
    }
    ports.chunks(2).map(|pair| [pair[0], pair[1]]).collect()
}

/// A failed test's view of the coordinator: its status and convergence of the service, printed
/// and kept with the host root's artifacts, while the coordinator still runs (this drop runs
/// before the fields' drops). It tells which of the coordinator's rules held an order back.
impl Drop for Scenario {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let ctl = |args: &[&str]| {
            let output = self.coordinator.ctl_output(args);
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        };
        let view = format!(
            "releases status:\n{}\nreleases converge:\n{}\nrelease channel requests:\n{:#?}\n\
             step record:\n{:#?}\n",
            ctl(&["releases", "status", "--service", SERVICE]),
            ctl(&["releases", "converge", "--service", SERVICE]),
            self.channel.requests(),
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
        host.install_gh(&pinned_gh());
        let signer = FixtureSigner::new();
        host.install_trusted_root(&signer.trusted_root_jsonl());
        let channel = ReleaseChannel::start();
        host.trust_ca(&channel.ca_pem());

        let builder =
            PackageBuilder::new(&host.packages(), &host.records(), Path::new(FIXTURE_BINARY));
        let built = host.base().join("built");
        fs::create_dir_all(&built).expect("the directory of the built packages");
        let versions = [(V1, false), (V2, false), (V3, true)];
        let mut packages = BTreeMap::new();
        for ((version, fail_verify), ports) in versions.into_iter().zip(port_pairs(versions.len()))
        {
            let fixture = FixtureVersion {
                version,
                ports,
                fail_verify,
            };
            let package = builder.build(&fixture, &built);
            let bundle = signer.bundle(&package.file_name, &package.sha256);
            channel.publish(
                SERVICE,
                version,
                &package.file_name,
                package.bytes.clone(),
                bundle,
            );
            packages.insert(version, package);
        }

        let coordinator = CoordinatorHandle::start(&host.base().join("coordinator"), SERVICE, ROLE);
        host.write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
            channel.base_url(),
            signer.trusted_signers_toml(),
        ));
        let daemon = host.start_daemon(&format!("hostkeeper-ci-{tag}-{}-daemon", process::id()));
        coordinator.approve_enrollment(HOST_ID, SERVICE);
        Scenario {
            daemon,
            coordinator,
            channel,
            signer,
            packages,
            host,
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

    /// Sets the desired version of the service to `version`, with its package's digest, through
    /// `hostkeeper ctl`.
    pub fn order(&self, version: &str) {
        let digest = self.package(version).sha256.clone();
        self.coordinator.set_desired(SERVICE, version, &digest);
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

    /// The seal of `version` as JSON (`{"schema", "tree_digest", "package_digest"}`).
    pub fn seal(&self, version: &str) -> Value {
        let path = self.host.service_dir().join(format!("{version}.seal"));
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_slice(&bytes).expect("a seal is JSON")
    }
}
