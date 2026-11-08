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
//!   real ports, its version the name of the file it runs from) at versions newer than the pinned
//!   release ([`newer`]), and `hostkeeper-failing-daemon` for a version made to fail its start.
//! - **hostkeeper's package** of each version is built here as the release builder lays it out
//!   (`crates/xtask/src/release/package.rs`): `bin/hostkeeper`, `files/trusted_root.json`,
//!   `manifest.toml`, and on Linux `units/hostkeeper.service`; its manifest is the release's, the
//!   floor the release builder writes for the build the binary comes from ([`floor`]) and the
//!   no-op hook `bin/hostkeeper update-hook`. Two things differ from a
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
//!   it (`mac`). The daemon writes its fallback unit and drop-in, or its fallback job, into the
//!   host's real unit directory, so the released binary's own fallback is what puts it back.
//!
//! Each test starts from [`Rig::installed`]: the released daemon has installed its own version's
//! package, as a host the coordinator converged on the release is. Every wait is a bounded count
//! of tries of an explicit state ([`super::e2e::wait_for`]); nothing is asserted about time.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::platform::Platform;
use hostkeeper_privileged::across::{Pin, checked_binary, download_path};
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
/// This build's `hostkeeper`, its version the name of the file it runs from.
pub const NAMED: &str = env!("CARGO_BIN_EXE_hostkeeper-named-daemon");
/// This build's `hostkeeper` made to fail its start.
pub const FAILING: &str = env!("CARGO_BIN_EXE_hostkeeper-failing-daemon");
/// The floor of hostkeeper's package the release builder writes today (`OLDEST_READER` of
/// `hostkeeper-steps`, and `min_hostkeeper` the lower of it and the build's version, in
/// `crates/xtask/src/release/package.rs`): the daemon's `LEGACY_FLOOR`, so the released daemon
/// and this build both probe the binary of a package of the rig and run its hook's command.
pub const FLOOR: &str = "0.1.53";
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
/// of [`FLOOR`] and the version that build was built with. The released binary was built as the
/// pinned release. This build was built with the workspace's version (`0.1.0` in a pull request,
/// the release's own once `release set-version` ran), which is also the version its package
/// store judges a package's floor against (`hostkeeper_unpack::running_hostkeeper`), whatever
/// name the program runs under; a higher floor is `daemon_too_old` to it.
pub fn floor(build: Build, released: &str) -> String {
    let floor = HostkeeperVersion::parse(FLOOR).expect("the floor is a version");
    let built = match build {
        Build::Released => HostkeeperVersion::parse(released).expect("the release is a version"),
        Build::This | Build::Failing => hostkeeper_unpack::running_hostkeeper(),
    };
    floor.min(built).to_string()
}

/// The manifest of hostkeeper's package at `version` with the floor `floor`, as the release
/// builder writes it.
pub fn manifest(version: &str, floor: &str) -> String {
    let platform = Platform::current();
    let units = if cfg!(target_os = "linux") {
        "\n[units]\nrun = [\"hostkeeper.service\"]\n"
    } else {
        ""
    };
    format!(
        "schema = 1\n\
         name = \"{HK}\"\n\
         version = \"{version}\"\n\
         platform = \"{platform}\"\n\
         min_hostkeeper = \"{floor}\"\n\
         \n\
         [hook]\n\
         command = [\"bin/hostkeeper\", \"update-hook\"]\n\
         verify_deadline_secs = 600\n\
         \n\
         [hook.timeout_secs]\n\
         prepare = 300\n\
         cutover = 300\n\
         verify = 300\n\
         rollback = 300\n\
         cleanup = 300\n\
         {units}"
    )
}

/// A package of hostkeeper the rig built and published.
#[derive(Clone, Debug)]
pub struct Built {
    pub version: String,
    pub sha256: String,
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
        let ctl = |args: &[&str]| {
            let output = self.coordinator.ctl_output(args);
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        };
        eprintln!(
            "the coordinator's view when the test failed:\nreleases status:\n{}\n\
             releases converge:\n{}\nrelease channel requests:\n{:#?}\nstep record:\n{:#?}\n",
            ctl(&["releases", "status", "--service", HK]),
            ctl(&["releases", "converge", "--service", HK]),
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
        let manifest = manifest(version, &floor(build, &self.released));
        let root = self.signer.trusted_root_jsonl();
        let unit = Host::package_unit(self.host.root());
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
        self.channel.publish(HK, version, &file_name, bytes, bundle);
        Built {
            version: version.to_owned(),
            sha256,
        }
    }

    /// Sets the desired version of hostkeeper to the published package `built`, through
    /// `hostkeeper ctl`.
    pub fn order(&self, built: &Built) {
        self.coordinator
            .set_desired(HK, &built.version, &built.sha256);
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
