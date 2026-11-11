//! What the tests of hostkeeper's own native package share (docs/design/packages-native.md,
//! sections 10.4 and 10.6): a host whose daemon announces apt, with the repository installer's
//! fake of apt and dpkg answering its tool runs ([`crate::support_apt`]), hostkeeper's binary
//! directory with [`V1`] staged and `current`, and hostkeeper's `.deb` of each version published
//! in the host's repository.
//!
//! Its versions are above every row of the compiled rollback table
//! (`hostkeeper_daemon::rollback_floor`), so that a rollback between them is not refused for its
//! version: [`V1`] runs, [`V2`] is ordered, [`V3`] comes later.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start::{Started, startup};
use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_install::{Announcement, Family};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::{Manifest, Target};
use hostkeeper_release::Installer;
use hostkeeper_steps::record::{Loaded, State};
use hostkeeper_steps::{Layout, Record, RecordStore};

use crate::support::selfupdate::{HK, own};
use crate::support::{Rig, config, d, force_rollback, install, update};
use crate::support_apt::{Deb, Events, FakeHost, REPO, URI, bridge, repositories};

/// The version the host runs.
pub const V1: &str = "1.0.0";
/// The version it is ordered to.
pub const V2: &str = "1.1.0";
/// A later version.
pub const V3: &str = "1.2.0";
/// The package version of a release in the repository.
pub fn pv(version: &str) -> String {
    format!("{version}-1")
}

/// The unit hostkeeper's `.deb` installs, and its path in the package.
pub const DEB_UNIT_NAME: &str = "hostkeeper.service";
pub const DEB_UNIT_PATH: &str = "usr/lib/systemd/system/hostkeeper.service";

/// A host that runs hostkeeper at [`V1`], set up from its `.deb`: its binary directory with
/// [`V1`] `current`, as the packaged start's seed leaves it, and nothing under the packages
/// directory.
pub struct NativeRig {
    pub rig: Rig,
    pub host: Arc<FakeHost>,
    pub events: Events,
}

impl NativeRig {
    pub fn new(name: &str) -> NativeRig {
        let rig = Rig::new(name);
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        let events = Events::default();
        let lock = rig.scratch.path().join("installer.lock");
        let host = Arc::new(FakeHost::new(&lock, Arc::clone(&events)));
        rig.launcher.answer_tools(bridge(Arc::clone(&host)));
        host.watch_units(&rig.host("/units"), "/units");
        let layout = Layout::new(&binary_dir(&rig.state));
        let old = rig.scratch.path().join("v1-binary");
        fs::write(&old, binary_text(V1)).expect("the binary of V1");
        layout.stage(V1, &old).expect("stage it");
        layout.switch(V1).expect("switch to it");
        NativeRig { rig, host, events }
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
    pub fn ports(&self) -> Ports {
        let keys = self.rig.scratch.path().join("keys");
        Ports {
            announcement: Announcement {
                installers: vec![Installer::Apt],
                family: Some(Family::Deb),
            },
            repository: self.rig.repository_ports(repositories(&keys)),
            ..self.rig.ports()
        }
    }

    /// The ports, placement going through `fs` (a filesystem that fails where the test says).
    pub fn ports_with_fs(&self, fs: Arc<hostkeeper_placed::testing::FaultFs>) -> Ports {
        Ports {
            placement: hostkeeper_daemon::Placement {
                roots: crate::support::roots(),
                fs,
            },
            ..self.ports()
        }
    }

    /// The scratch directory as `/`, for a [`hostkeeper_placed::testing::FaultFs`].
    pub fn root(&self) -> PathBuf {
        self.rig.scratch.path().to_path_buf()
    }

    /// A daemon of `version` whose placement goes through `fs`.
    pub fn daemon_with_fs(
        &self,
        version: &str,
        fs: Arc<hostkeeper_placed::testing::FaultFs>,
    ) -> Daemon {
        Daemon::new(
            config(),
            self.rig.dirs(),
            self.ports_with_fs(fs),
            own(version),
        )
    }

    /// A daemon of `version` on the host, the way an order reaches it (no start).
    pub fn daemon_at(&self, version: &str) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.ports(), own(version))
    }

    /// The start of the daemon of `version` on the host: settlement, the `activate` of its own
    /// package, the adoption, the boot pass.
    pub fn start(&self, version: &str) -> Started {
        fs::create_dir_all(&self.rig.state).expect("the state dir");
        fs::set_permissions(&self.rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
        startup(&self.rig.settings(), self.ports(), own(version)).expect("the daemon starts")
    }

    /// Publishes hostkeeper's `.deb` of `version` in the repository, answers its pin.
    pub fn publish(&self, version: &str) -> String {
        self.host.publish(URI, hk_deb(version))
    }

    /// The version directory of `version` of hostkeeper.
    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.rig.packages.join(HK).join(version)
    }

    /// The version a link of hostkeeper's packages directory (`current`, `previous`) names.
    pub fn linked(&self, link: &str) -> Option<String> {
        fs::read_link(self.rig.packages.join(HK).join(link))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The binary directory.
    pub fn layout(&self) -> Layout {
        Layout::new(&binary_dir(&self.rig.state))
    }

    /// hostkeeper's step record.
    pub fn record(&self) -> Option<Record> {
        match RecordStore::new(&self.rig.state.join(HK)).load() {
            Ok(Loaded::Current(rec)) => Some(*rec),
            _ => None,
        }
    }

    /// hostkeeper's `installed` entry of `placed.json`.
    pub fn installed(&self) -> Option<hostkeeper_placed::InstalledEntry> {
        self.rig.placed_record().installed.get(HK).cloned()
    }

    /// The labels of the fake's tool runs from call number `from` on.
    pub fn labels_since(&self, from: usize) -> Vec<String> {
        self.host.calls()[from..]
            .iter()
            .map(|call| call.label.clone())
            .collect()
    }

    /// The number of tool runs so far.
    pub fn calls(&self) -> usize {
        self.host.calls().len()
    }
}

/// The text of hostkeeper's binary of `version` in the tests.
pub fn binary_text(version: &str) -> String {
    format!("#!hostkeeper {version}\n")
}

/// hostkeeper's `.deb` of `version`: its binary and its unit, with the maintainer scripts its own
/// package keeps.
pub fn hk_deb(version: &str) -> Deb {
    Deb::new(HK, &pv(version), "amd64")
        .file("usr/bin/hostkeeper", 0o755, &binary_text(version))
        .file(
            DEB_UNIT_PATH,
            0o644,
            "[Unit]\nDescription=hostkeeper\n[Service]\nExecStart=/usr/bin/hostkeeper daemon\n[Install]\nWantedBy=multi-user.target\n",
        )
        .member("postinst")
        .member("prerm")
}

/// The manifest of hostkeeper's native entry: its unit, no hook, the installer's bounds.
pub fn hk_manifest() -> Manifest {
    Manifest {
        package: Some(HK.to_owned()),
        run: vec![DEB_UNIT_NAME.to_owned()],
        ..crate::support_apt::manifest()
    }
}

/// hostkeeper's apt entry of the release `version`, pinned to `pin`.
pub fn hk_entry(version: &str, pin: &str) -> Target {
    Target {
        manifest: Some(hk_manifest()),
        ..crate::support_apt::entry(&pv(version), pin)
    }
}

/// The `update` of hostkeeper to `version` through its `.deb`, pinned to `pin`.
pub fn update_hk(version: &str, pin: &str, order_try: u64) -> Order {
    crate::support_apt::with_entry(update(HK, version, pin, order_try), &hk_entry(version, pin))
}

/// The `install` of hostkeeper at `version` through its `.deb`, pinned to `pin`.
pub fn install_hk(version: &str, pin: &str, order_try: u64) -> Order {
    crate::support_apt::with_entry(
        install(HK, version, pin, order_try),
        &hk_entry(version, pin),
    )
}

/// The `force_rollback` of hostkeeper to `version` through its `.deb`, pinned to `pin`.
pub fn rollback_hk(version: &str, pin: &str, order_try: u64) -> Order {
    crate::support_apt::with_entry(
        force_rollback(HK, version, pin, order_try),
        &hk_entry(version, pin),
    )
}

/// A reply that ran to `Succeeded` now, with no reason.
pub fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The finished state of a record: `Succeeded` with no reason.
pub fn succeeded_state() -> State {
    State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    }
}

/// The finished state of a record whose package's `activate` failed for `reason`.
pub fn activate_failed(reason: &str) -> State {
    State::Finished {
        verdict: Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        reason: Some(reason.to_owned()),
    }
}
