//! The cross-release rig of `tests/self_update_across_releases.rs`: hostkeeper's update of itself
//! from a **published release** to this build, run end to end as a host runs it.
//!
//! - **The released daemon** is the `hostkeeper` binary of the release `previous-release.toml`
//!   pins (`hostkeeper_privileged::across`), downloaded and checked by a workflow step before the
//!   test, checked against the pin again here ([`released_binary`]), and staged as the host's
//!   first binary: `versions/<v>` of the binary directory, `current` naming it, as the deb, the
//!   rpm and the pkg leave a host. A binary that is missing or differs from the pin fails the
//!   test by name; the test fetches nothing.
//! - **This build** is the program `hostkeeper-named-daemon` (the shipped daemon on the host's
//!   real ports, its hand-over of its own update acted on first, its version the name of the file
//!   it runs from) at versions newer than the pinned release ([`newer`]), and
//!   `hostkeeper-failing-daemon` for a version made to fail its start.
//! - **hostkeeper's package** of each version is built here as the release builder lays it out
//!   (`crates/xtask/src/release/package.rs`): `bin/hostkeeper`, `files/trusted_root.json`,
//!   `manifest.toml`, and on Linux `units/hostkeeper.service`; its manifest is the release's, the
//!   floor the release builder of the build the binary comes from writes ([`floor`]) and its
//!   hook. The released version's hook is the release's own, `bin/hostkeeper update-hook`, which
//!   the released daemon does not run (it runs its built-in steps at that floor); this build's is
//!   the workload fixture `bin/hook` (ending every step 0), because a dev build's floor is at or
//!   below the daemon's legacy floor and a daemon then runs the hook's command, which this
//!   build's binary has no verb for. Two things differ from a
//!   release, both because the test is a host of its own: the packaged trusted root is the test
//!   signer's, the root the host's copy holds too (the daemon uses whichever of the two was
//!   written last, so a packaged production root would refuse every later package), and the
//!   unit names the test's directories (`linux::unit_text`). The released version's package is
//!   built the same way around the released binary.
//! - **The rest is the end-to-end rig** (`tests/e2e/`): the real coordinator in process,
//!   managing the service `hostkeeper` for the test's host; the release channel on loopback; the
//!   production attestation verifier (the pinned `gh`) against the test signer's trusted root,
//!   which reaches the released daemon as it reaches this build's: a fixed path of the host, not
//!   a key of the daemon config, so the published release is pinned and no release is built for
//!   the test.
//! - **The host** is the platform's ([`Host`]): on Linux the daemon's own unit
//!   `hostkeeper.service` under systemd, installed where the deb installs it (`linux`); on a Mac
//!   the daemon's own job `dev.hostkeeper.hostkeeper` under launchd, its plist where the pkg puts
//!   it (`mac`). The released daemon updates by the hand-over, so it writes no fallback unit,
//!   drop-in or job: when this build cannot start, the released binary is what runs again, after
//!   its three tries.
//!
//! Each test starts from [`Rig::installed`]: the released daemon has installed its own version's
//! package, as a host the coordinator converged on the release is. Every wait is a bounded count
//! of tries of an explicit state ([`super::e2e::wait_for`]); nothing is asserted about time.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::platform::Platform;
use hostkeeper_privileged::across::{Pin, checked_binary, download_path};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Manifest, Target, TargetList};
use hostkeeper_steps::record::{Loaded, Record, RecordStore, State};
use hostkeeper_steps::{BadVersions, Layout};
use serde_json::Value;

use super::e2e::channel::ReleaseChannel;
use super::e2e::coordinator::CoordinatorHandle;
use super::e2e::package::{ArchiveEntry, compressed_archive};
use super::e2e::scenario::{COORDINATED, LOCAL, pinned_gh};
use super::e2e::signer::FixtureSigner;
use super::e2e::{HOST_ID, ROLE, wait_for};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::Host;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "macos")]
pub use mac::Host;

/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The workload fixture, the hook of the packages of this build.
pub const HOOK: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");
/// This build's `hostkeeper`, its version the name of the file it runs from.
pub const NAMED: &str = env!("CARGO_BIN_EXE_hostkeeper-named-daemon");
/// This build's `hostkeeper` made to fail its start.
pub const FAILING: &str = env!("CARGO_BIN_EXE_hostkeeper-failing-daemon");
/// The floor of hostkeeper's package the pinned release's builder wrote: its `OLDEST_READER`,
/// 0.1.163 (`min_hostkeeper` is the lower of that and the release's version, in
/// `crates/xtask/src/release/package.rs`), above the daemon's `LEGACY_FLOOR`, so the released
/// daemon runs its built-in steps for its own package.
pub const FLOOR: &str = "0.1.163";
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
/// set-version` ran), which is also the version its package store judges a package's floor
/// against (`hostkeeper_unpack::running_hostkeeper`), whatever name the program runs under; a
/// higher floor is `daemon_too_old` to it. In a pull request the floor is therefore the
/// workspace's `0.1.0`, at or below the legacy floor, so a daemon runs the hook's command of
/// this build's package: the workload fixture ([`manifest`]).
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

/// The manifest of hostkeeper's package of `build` at `version` with the floor `floor`, as the
/// release builder writes it; this build's hook is the workload fixture `bin/hook` in mode `ok`
/// ([`HOOK`]) in place of `bin/hostkeeper update-hook`, which its binary has no verb for.
pub fn manifest(version: &str, floor: &str, build: Build) -> String {
    let platform = Platform::current();
    let units = if cfg!(target_os = "linux") {
        "\n[units]\nrun = [\"hostkeeper.service\"]\n"
    } else {
        ""
    };
    let (command, params) = match build {
        Build::Released => ("\"bin/hostkeeper\", \"update-hook\"", ""),
        Build::This | Build::Failing => ("\"bin/hook\"", "\n[params]\n\"fixture.mode\" = \"ok\"\n"),
    };
    format!(
        "schema = 1\n\
         name = \"{HK}\"\n\
         version = \"{version}\"\n\
         platform = \"{platform}\"\n\
         min_hostkeeper = \"{floor}\"\n\
         \n\
         [hook]\n\
         command = [{command}]\n\
         verify_deadline_secs = 600\n\
         \n\
         [hook.timeout_secs]\n\
         prepare = 300\n\
         cutover = 300\n\
         verify = 300\n\
         rollback = 300\n\
         cleanup = 300\n\
         {params}{units}"
    )
}

/// The store of the release channel, which the entries of the rig's sets name: the one a daemon
/// without a `stores.toml` has.
pub const CHANNEL_STORE: &str = "channel";

/// A package of hostkeeper the rig built and published.
#[derive(Clone, Debug)]
pub struct Built {
    pub version: String,
    /// The archive's SHA-256.
    pub sha256: String,
    /// The archive's size.
    pub bytes: u64,
    /// The archive's name on the channel, `<service>-<version>-<platform>.tar.zst`.
    pub file_name: String,
    /// The SHA-256 of the attestation bundle beside the archive.
    pub bundle_sha256: String,
    /// The bundle's size.
    pub bundle_bytes: u64,
    /// The `min_hostkeeper` of the package's manifest, and of its entry's.
    pub floor: String,
    /// The text of the version's `index.json` on the channel, and its SHA-256.
    pub index: String,
    pub index_sha256: String,
}

impl Built {
    /// The path of the version's release index on the channel, as a request for it is logged.
    pub fn index_path(&self) -> String {
        format!("/{HK}/{}/index.json", self.version)
    }

    /// The path of the archive in the channel's store, as a location names it: `S/V/S-V-platform.tar.zst`,
    /// below the base of the release channel (the form of `release targets`).
    pub fn archive_locator(&self) -> String {
        format!("{HK}/{}/{}", self.version, self.file_name)
    }

    /// The path of the bundle in the channel's store: the archive's, with `.sigstore.json`.
    pub fn bundle_locator(&self) -> String {
        format!("{}.sigstore.json", self.archive_locator())
    }

    /// The one `hostkeeper-archive` entry of this package for the host's platform, as CI's set
    /// writes it (`crates/xtask/src/release/targets.rs`) with the channel as the only store: the
    /// archive and its bundle at their locators, their digests and sizes, and the manifest's
    /// floor.
    pub fn entry(&self) -> Target {
        let at = |locator: String| Location {
            store: CHANNEL_STORE.to_owned(),
            locator,
        };
        Target {
            installer: "hostkeeper-archive".to_owned(),
            locations: vec![at(self.archive_locator())],
            digest: self.sha256.clone(),
            bytes: self.bytes,
            attestation: Some(Blob {
                digest: self.bundle_sha256.clone(),
                bytes: self.bundle_bytes,
                locations: vec![at(self.bundle_locator())],
            }),
            manifest: Some(Manifest {
                min_hostkeeper: self.floor.clone(),
                ..Manifest::default()
            }),
            repository: None,
            package_version: None,
        }
    }

    /// The `targets` of a set of this package: the entry for the host's platform.
    pub fn targets(&self) -> BTreeMap<String, TargetList> {
        BTreeMap::from([(
            Platform::current().to_string(),
            TargetList {
                entries: vec![self.entry()],
            },
        )])
    }
}

/// The rig of one test. The fields are dropped in this order: the coordinator and the channel,
/// then the host (the daemon stopped, every place the test took over put back).
pub struct Rig {
    pub coordinator: CoordinatorHandle,
    pub channel: ReleaseChannel,
    pub signer: FixtureSigner,
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
             releases converge:\n{}\nrelease channel requests:\n{:#?}\nstep record:\n{:#?}\n",
            self.coordinator.status_text(HK),
            self.coordinator.converge_text(HK),
            self.channel.requests(),
            record_in(&self.host.root().state()),
        );
    }
}

impl Rig {
    /// The whole rig up: the host, the pinned `gh`, the signer's trusted root, the release
    /// channel, the coordinator managing hostkeeper, the daemon config, and the released daemon
    /// started as the host's daemon and approved as [`HOST_ID`]; nothing ordered yet. `tag`
    /// (1 to 8 of `[a-z0-9]`) names the test's tree.
    pub fn start(tag: &str) -> Rig {
        let pin = pin();
        let released_binary = released_binary(&pin);
        let host = Host::new(tag);
        host.root().install_gh(&pinned_gh());
        let signer = FixtureSigner::new();
        host.root()
            .install_trusted_root(&signer.trusted_root_jsonl());
        let channel = ReleaseChannel::start();
        host.root().trust_ca(&channel.ca_pem());
        let coordinator =
            CoordinatorHandle::start(&host.root().base().join("coordinator"), HK, ROLE);
        host.root().write_config(&format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
            coordinator.heartbeat_url(),
            coordinator.ca_sha256(),
            channel.base_url(),
            signer.trusted_signers_toml(),
        ));
        let released = pin.version.to_string();
        let first = host.root().base().join(format!("released-{released}"));
        fs::write(&first, &released_binary)
            .unwrap_or_else(|e| panic!("write {}: {e}", first.display()));
        host.start(&first, &released);
        coordinator.approve_enrollment(HOST_ID, HK);
        Rig {
            coordinator,
            channel,
            signer,
            released,
            released_binary,
            host,
        }
    }

    /// [`Rig::start`], then the released version's package installed by the coordinator's
    /// order: the install of the version that runs, which the daemon takes without a restart.
    pub fn installed(tag: &str) -> Rig {
        let rig = Rig::start(tag);
        let released = rig.released.clone();
        let built = rig.publish(&released, Build::Released);
        rig.order(&built);
        rig.wait_converged(&released);
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

    /// Builds hostkeeper's package at `version` carrying the `hostkeeper` of `build`, signs it
    /// and publishes it on the release channel.
    pub fn publish(&self, version: &str, build: Build) -> Built {
        let binary = self.binary(build);
        let floor = floor(build, &self.released);
        let manifest = manifest(version, &floor, build);
        let root = self.signer.trusted_root_jsonl();
        let unit = Host::package_unit(self.host.root());
        let hook = (build != Build::Released)
            .then(|| fs::read(HOOK).unwrap_or_else(|e| panic!("read {HOOK}: {e}")));
        let mut entries = vec![
            ArchiveEntry {
                name: "bin/",
                mode: 0o555,
                data: b"",
            },
            ArchiveEntry {
                name: "bin/hostkeeper",
                mode: 0o555,
                data: &binary,
            },
            ArchiveEntry {
                name: "files/",
                mode: 0o555,
                data: b"",
            },
            ArchiveEntry {
                name: "files/trusted_root.json",
                mode: 0o444,
                data: root.as_bytes(),
            },
            ArchiveEntry {
                name: "manifest.toml",
                mode: 0o444,
                data: manifest.as_bytes(),
            },
        ];
        if let Some(hook) = &hook {
            entries.push(ArchiveEntry {
                name: "bin/hook",
                mode: 0o555,
                data: hook,
            });
        }
        if let Some(unit) = &unit {
            entries.push(ArchiveEntry {
                name: "units/",
                mode: 0o555,
                data: b"",
            });
            entries.push(ArchiveEntry {
                name: "units/hostkeeper.service",
                mode: 0o444,
                data: unit.as_bytes(),
            });
        }
        let bytes = compressed_archive(&entries);
        let sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
        let file_name = format!("{HK}-{version}-{}.tar.zst", Platform::current());
        let bundle = self.signer.bundle(&file_name, &sha256);
        let (archive_len, bundle_len) = (bytes.len() as u64, bundle.len() as u64);
        let bundle_sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bundle));
        self.channel.publish(HK, version, &file_name, bytes, bundle);
        let index = self
            .channel
            .object(&format!("/{HK}/{version}/index.json"))
            .expect("the channel serves the index it just published");
        Built {
            version: version.to_owned(),
            sha256,
            bytes: archive_len,
            file_name,
            bundle_sha256,
            bundle_bytes: bundle_len,
            floor,
            index_sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&index)),
            index: String::from_utf8(index).expect("the index is text"),
        }
    }

    /// Sets the desired version of hostkeeper to the published package `built` as CI's set does
    /// (design stage 2 and 3): by targets, the one `hostkeeper-archive` entry for the host's
    /// platform ([`Built::entry`]), and the version's release index with its digest, through the
    /// operator API. A daemon that announces no `target-v1` is ordered the package and the index
    /// digest of that platform and fetches the index from the channel; one that does is ordered
    /// the entry and fetches through it.
    pub fn order(&self, built: &Built) {
        self.coordinator.set_desired_targets(
            HK,
            &built.version,
            built.targets(),
            &built.index,
            &built.index_sha256,
        );
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
