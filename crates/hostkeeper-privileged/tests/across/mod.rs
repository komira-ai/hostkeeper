//! The cross-release rig of `tests/self_update_across_releases.rs`: hostkeeper's update of itself
//! from a **published release** to this build, run end to end as a host runs it.
//!
//! - **The released daemon** is the `hostkeeper` binary of the release `previous-release.toml`
//!   pins (`hostkeeper_privileged::across`), downloaded and checked by a workflow step before the
//!   test, checked against the pin again here ([`released_binary`]), and staged as the host's
//!   first binary: `versions/<v>` of the binary directory, `current` naming it, as the deb leaves
//!   a host. A binary that is missing or differs from the pin fails the test by name; the test
//!   fetches nothing.
//! - **This build** is the program `hostkeeper-named-daemon` (the shipped daemon on the host's
//!   real ports, its hand-over of its own update acted on first, its version the name of the file
//!   it runs from) at versions newer than the pinned release ([`newer`]), and
//!   `hostkeeper-failing-daemon` for a version made to fail its start.
//! - **hostkeeper's package** of each version is its native `.deb` (`deb`): built around the
//!   binary of the version as `packaging/linux/nfpm.yaml` lays it out, in a signed apt repository
//!   on loopback; the coordinator in process orders it by targets alone, the one `apt` entry of
//!   `release targets` for the host's platform and no release index, as CI's set of a release
//!   does from A24 on, and the daemon installs it through its entry.
//! - **The rest is the end-to-end rig** (`tests/e2e/`): the real coordinator in process,
//!   managing the service `hostkeeper` for the test's host; the pinned `gh` and a Sigstore trusted
//!   root of the test signer at the fixed places where the published release looks for them
//!   (it asks nothing of them for a native entry, and its config names a release channel nothing
//!   serves, as every config does), so that the published release is pinned and no release is
//!   built for the test.
//! - **The host** is [`Host`]: the package `hostkeeper` in the host's dpkg database, its unit
//!   `hostkeeper.service` under systemd where the `.deb` installs it, the rig's repository entry
//!   in `/etc/hostkeeper/repositories/` (`linux`). The released daemon updates by the hand-over,
//!   so it writes no fallback unit or drop-in: when this build cannot start, the released binary
//!   is what runs again, after its three tries.
//!
//! The rig relies on a released daemon that can install `apt` entries, adopt the `.deb` of a host
//! set up from it and update itself through its `.deb` (A19): the release `previous-release.toml`
//! pins is the first that can.
//!
//! Each test starts from [`Rig::installed`]: the released daemon runs on a host set up from the
//! `.deb` of its own version (installed with `dpkg -i`, which runs the shipped `postinst`), and
//! its start adopted the package: it holds it and records it as installed by `apt`. Every wait is
//! a bounded count of tries of an explicit state ([`super::e2e::wait_for`]); nothing is asserted
//! about time.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::platform::Platform;
use hostkeeper_privileged::across::{Pin, checked_binary, download_path};
use hostkeeper_steps::record::{Loaded, Record, RecordStore, State};
use hostkeeper_steps::{BadVersions, Layout};
use serde_json::Value;

use super::e2e::coordinator::CoordinatorHandle;
use super::e2e::scenario::{COORDINATED, LOCAL, pinned_gh};
use super::e2e::signer::FixtureSigner;
use super::e2e::{HOST_ID, ROLE, wait_for};

mod deb;
pub use deb::Built;
pub mod linux;
pub use linux::Host;
/// The fixture repository's builder, which the rig's `.deb` repository is made with.
#[allow(
    dead_code,
    reason = "the rig uses part of the apt device test's repository builder"
)]
#[path = "../install_apt/repo.rs"]
mod repo;

/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// This build's `hostkeeper`, its version the name of the file it runs from.
pub const NAMED: &str = env!("CARGO_BIN_EXE_hostkeeper-named-daemon");
/// This build's `hostkeeper` made to fail its start.
pub const FAILING: &str = env!("CARGO_BIN_EXE_hostkeeper-failing-daemon");
/// The floor of hostkeeper's package the pinned release's builder wrote: its `OLDEST_READER`,
/// 0.1.194, R2 (`min_hostkeeper` is the lower of that and the release's version, in
/// `crates/xtask/src/release/package.rs`), above the daemon's `LEGACY_FLOOR`, so the released
/// daemon runs its built-in steps for its own package.
pub const FLOOR: &str = "0.1.194";
/// The reason of the verdict of a version whose new binary fell back.
pub const FALLBACK_REASON: &str = "self_update_fallback";

/// Which `hostkeeper` a package of the rig carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// The pinned release's binary.
    Released,
    /// This build ([`NAMED`]).
    This,
    /// This build made to fail its start ([`FAILING`]).
    Failing,
}

/// The pinned release, as `previous-release.toml` names it.
pub fn pin() -> Pin {
    Pin::parse(hostkeeper_privileged::across::PIN_TEXT)
        .unwrap_or_else(|e| panic!("previous-release.toml: {e}"))
}

/// The target directory: the parent of cargo's scratch directory for tests.
fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("the target directory")
        .to_path_buf()
}

/// The pinned release's daemon binary, from the tarball the workflow downloaded, checked against
/// the pin.
///
/// # Panics
/// The tarball is not downloaded, differs from the pin, or is not a release's binary tarball;
/// the message names which.
pub fn released_binary(pin: &Pin) -> Vec<u8> {
    let tarball = download_path(&target_dir());
    checked_binary(&tarball, pin.asset(Platform::current()))
        .unwrap_or_else(|e| panic!("the pinned release {}: {e}", pin.tag))
}

/// The version `step` patch releases after `version`: a name newer than the pinned release's,
/// as every release of this build will be.
pub fn newer(version: &str, step: u64) -> String {
    let parts: Vec<u64> = version
        .split('.')
        .map(|part| part.parse().expect("a version part"))
        .collect();
    let [major, minor, patch] = parts[..] else {
        panic!("{version} is not major.minor.patch");
    };
    format!("{major}.{minor}.{}", patch + step)
}

/// The `min_hostkeeper` the release builder writes into a package of `build`'s binary: the lower
/// of the `OLDEST_READER` of the build the binary comes from and the version that build was built
/// with. The released binary was built as the pinned release: [`FLOOR`] and the release's
/// version. This build's `OLDEST_READER` is `hostkeeper_steps::OLDEST_READER` (R2, 0.1.194) and
/// its version the workspace's (`0.1.0` in a pull request, the release's own once `release
/// set-version` ran), which is also the version `hostkeeper_unpack::running_hostkeeper` gives,
/// whatever name the program runs under. In a pull request the floor is therefore the
/// workspace's `0.1.0`, at or below the legacy floor (an entry has no hook command and runs the
/// built-in steps whatever the floor).
pub fn floor(build: Build, released: &str) -> String {
    let (oldest, built) = match build {
        Build::Released => (
            HostkeeperVersion::parse(FLOOR).expect("the floor is a version"),
            HostkeeperVersion::parse(released).expect("the release is a version"),
        ),
        Build::This | Build::Failing => (
            hostkeeper_steps::OLDEST_READER,
            hostkeeper_unpack::running_hostkeeper(),
        ),
    };
    oldest.min(built).to_string()
}

/// The rig of one test. The fields are dropped in this order: the coordinator, then the host (the
/// daemon stopped, every place the test took over put back).
pub struct Rig {
    pub coordinator: CoordinatorHandle,
    /// The apt repository of hostkeeper's `.deb`s and the host's entry of it.
    repository: deb::Repository,
    /// The pinned release's version.
    pub released: String,
    /// The pinned release's daemon binary.
    released_binary: Vec<u8>,
    pub host: Host,
}

/// A failed test's view of the coordinator, printed and kept while it still runs.
impl Drop for Rig {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        eprintln!(
            "the coordinator's view when the test failed:\nreleases status:\n{}\n\
             releases converge:\n{}\nstep record:\n{:#?}\n{}",
            self.coordinator.status_text(HK),
            self.coordinator.converge_text(HK),
            record_in(&self.host.root().state()),
            self.native_view(),
        );
    }
}

impl Rig {
    /// The whole rig up: the host, the pinned `gh`, the signer's trusted root, the coordinator
    /// managing hostkeeper, the daemon config, the apt repository and its entry, and the released
    /// daemon started as the host's daemon, on the `.deb` of its own version installed
    /// beforehand, and approved as [`HOST_ID`]; nothing ordered yet. `tag` (1 to 8 of `[a-z0-9]`)
    /// names the test's tree.
    pub fn start(tag: &str) -> Rig {
        let pin = pin();
        let released_binary = released_binary(&pin);
        let host = Host::new(tag);
        host.root().install_gh(&pinned_gh());
        let signer = FixtureSigner::new();
        host.root()
            .install_trusted_root(&signer.trusted_root_jsonl());
        let coordinator =
            CoordinatorHandle::start(&host.root().base().join("coordinator"), HK, ROLE);
        host.root().write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"https://releases.example\"\n\n{}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
            signer.trusted_signers_toml(),
        ));
        let released = pin.version.to_string();
        let first = host.root().base().join(format!("released-{released}"));
        fs::write(&first, &released_binary)
            .unwrap_or_else(|e| panic!("write {}: {e}", first.display()));
        let repository = deb::Repository::new(host.root().base());
        let rig = Rig {
            coordinator,
            repository,
            released,
            released_binary,
            host,
        };
        rig.start_released(&first);
        rig.coordinator.approve_enrollment(HOST_ID, HK);
        rig
    }

    /// [`Rig::start`], then the released version's package in place as the host the coordinator
    /// converged on the release is: the released daemon adopted the `.deb` the host was set up
    /// with at its start, and holds it and records it as installed by `apt`.
    pub fn installed(tag: &str) -> Rig {
        let rig = Rig::start(tag);
        rig.converge_on_released();
        rig
    }

    /// The bytes of the `hostkeeper` of `build`.
    fn binary(&self, build: Build) -> Vec<u8> {
        match build {
            Build::Released => self.released_binary.clone(),
            Build::This => fs::read(NAMED).unwrap_or_else(|e| panic!("read {NAMED}: {e}")),
            Build::Failing => fs::read(FAILING).unwrap_or_else(|e| panic!("read {FAILING}: {e}")),
        }
    }

    /// The step record of hostkeeper, when there is one this build reads.
    pub fn record(&self) -> Option<Record> {
        record_in(&self.host.root().state())
    }

    /// Waits until the step record of hostkeeper is a finished update to `version`, and gives
    /// it.
    pub fn settled(&self, version: &str) -> Record {
        let mut found = None;
        wait_for(
            &format!("the step record of hostkeeper settles its update to {version}"),
            LOCAL,
            || {
                found = self
                    .record()
                    .filter(|rec| rec.target_version == version && finished(rec));
                found.is_some()
            },
        );
        found.expect("a finished record")
    }

    /// The coordinator's status of hostkeeper.
    pub fn status(&self) -> Value {
        self.coordinator.status(HK)
    }

    /// The test host as the coordinator sees it, `Null` before its first heartbeat lists it.
    pub fn host_row(&self) -> Value {
        self.status()["hosts"]
            .as_array()
            .and_then(|hosts| hosts.iter().find(|host| host["nodeId"] == HOST_ID))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// What the host's last heartbeat said of its last update of hostkeeper.
    pub fn last_update(&self) -> Value {
        self.host_row()["beat"]["lastUpdate"].clone()
    }

    /// Whether the coordinator reports hostkeeper converged on `version`, the host running it.
    pub fn converged_on(&self, version: &str) -> bool {
        let convergence = self.coordinator.converge(HK);
        convergence["state"] == "CONVERGENCE_STATE_CONVERGED"
            && convergence["desired"] == version
            && self.host_row()["beat"]["version"] == version
    }

    /// Waits until [`Rig::converged_on`] `version`; fails at once, with the host's logs, when an
    /// update of hostkeeper to another version than an earlier one failed on the way.
    pub fn wait_converged(&self, version: &str) {
        wait_for(
            &format!("the coordinator reports the host converged on {version}"),
            COORDINATED,
            || {
                let update = self.last_update();
                assert!(
                    !(update["target"] == version && update["outcome"] == "OUTCOME_FAILED"),
                    "the update to {version} failed: {update}\n{}",
                    self.host.logs()
                );
                self.converged_on(version)
            },
        );
    }

    /// Waits until the host's last update of hostkeeper is of `version` and ended `outcome`
    /// (`OUTCOME_*`), and gives it.
    pub fn wait_reported(&self, version: &str, outcome: &str) -> Value {
        let mut found = Value::Null;
        wait_for(
            &format!("the host reports its update to {version} {outcome}"),
            COORDINATED,
            || {
                found = self.last_update();
                found["target"] == version && found["outcome"] == outcome
            },
        );
        found
    }

    /// The binary directory of the daemon.
    pub fn layout(&self) -> Layout {
        Layout::new(&hostkeeper_daemon::selfupdate::binary_dir(
            &self.host.root().state(),
        ))
    }

    /// The versions `current` and `previous` of the binary directory name.
    pub fn links(&self) -> (Option<String>, Option<String>) {
        let layout = self.layout();
        (
            layout.current().expect("read current"),
            layout.previous().expect("read previous"),
        )
    }

    /// Whether `version` is on the daemon's local list of bad versions of hostkeeper.
    pub fn is_bad(&self, version: &str) -> bool {
        BadVersions::load(&self.host.root().state().join(HK))
            .expect("the list of bad versions")
            .contains(version)
    }

    /// Waits until the host's last heartbeat as the coordinator holds it is from the agent
    /// `agent` and lists hostkeeper at `version`: the daemon that runs now has looked at the
    /// packages on the host and reported what it read.
    pub fn wait_host_reports(&self, agent: &str, version: &str) {
        wait_for(
            &format!("the host's heartbeat is of the agent {agent} and lists hostkeeper {version}"),
            COORDINATED,
            || {
                let heartbeat = self.coordinator.host(HOST_ID)["heartbeat"].clone();
                heartbeat["agent"]["version"] == agent
                    && heartbeat["services"].as_array().is_some_and(|services| {
                        services
                            .iter()
                            .any(|s| s["name"] == HK && s["version"] == version)
                    })
            },
        );
    }

    /// The file the rig staged the released binary from.
    pub fn released_file(&self) -> PathBuf {
        self.host
            .root()
            .base()
            .join(format!("released-{}", self.released))
    }

    /// Puts the host back on the released binary the way an operator does: the daemon stopped,
    /// the released binary staged again when a later update pruned it (the binary directory keeps
    /// `current` and `previous`), `current` pointed at it, and the daemon started again. The
    /// packages on the host are left as the daemon that ran last left them.
    ///
    /// # Panics
    /// The daemon does not stop or start, or the binary cannot be staged or switched to.
    pub fn go_back_to_released(&self) {
        self.host.stop();
        let layout = self.layout();
        if fs::symlink_metadata(layout.version_path(&self.released)).is_err() {
            layout
                .stage(&self.released, &self.released_file())
                .expect("stage the released binary again");
        }
        layout
            .switch(&self.released)
            .expect("switch to the released binary");
        self.host.start_again();
    }

    /// Waits until the daemon's process runs the binary directory's file of `version`.
    pub fn wait_running(&self, version: &str) {
        let want = fs::canonicalize(self.layout().version_path(version))
            .unwrap_or_else(|e| panic!("the staged binary of {version}: {e}"));
        wait_for(
            &format!("the daemon runs {}", want.display()),
            LOCAL,
            || self.host.running().as_deref() == Some(want.as_path()),
        );
    }
}

/// The step record of hostkeeper in the state directory `state`, when there is one this build
/// reads.
pub fn record_in(state: &Path) -> Option<Record> {
    match RecordStore::new(&state.join(HK)).load() {
        Ok(Loaded::Current(rec)) => Some(*rec),
        Ok(Loaded::Absent | Loaded::Newer { .. }) => None,
        Err(e) => panic!("the step record cannot be read: {e}"),
    }
}

/// Whether `rec` is finished.
pub fn finished(rec: &Record) -> bool {
    matches!(rec.state, State::Finished { .. })
}
