//! A daemon on a Mac that announces `macos-pkg`: the `macos-pkg` installer's fake Mac
//! (`hostkeeper-install`'s, one copy for both crates' tests) is the installer's tools port, over
//! a scratch host root, and the package store serves each `.pkg` by its pin
//! ([`hostkeeper_daemon::testing::FakePackages::add_pinned`]). The host's signer is the leaf every
//! fixture package is signed by.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

#[path = "../../../hostkeeper-install/tests/macos_pkg/support/mod.rs"]
pub mod pkg;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::macos_pkg::{self, MacosPkgPorts};
use hostkeeper_daemon::repository::RepositoryPorts;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_install::host::{Host, Ids};
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::{Announcement, Tools};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, StepBounds, Target,
};
use hostkeeper_release::Installer;

use crate::support::{OWN, Rig, config, install, update};
pub use pkg::{FakeMac, FakePkg, fp, leaf, signed_output};

/// The service, its job's label (a unit name on the Linux the tests run on, and a label of the
/// service on a Mac) and its package's identifier.
pub const SERVICE: &str = "hello";
pub const LABEL: &str = "hello.service";
pub const IDENTIFIER: &str = "dev.hostkeeper.hello";

/// A rig whose daemon runs on a Mac with the signers file: `macos-pkg` announced, its tools the
/// fake Mac.
pub struct MacRig {
    pub rig: Rig,
    pub mac: Arc<FakeMac>,
    pub host: Host,
    pub root: PathBuf,
    pub signers: Signers,
}

impl MacRig {
    pub fn new(name: &str) -> MacRig {
        let rig = Rig::new(name);
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        let root = rig.scratch.path().join("mac");
        fs::create_dir(&root).expect("the Mac's root");
        let meta = fs::metadata(&root).expect("its owner");
        let ids = Ids {
            root: meta.uid(),
            wheel: meta.gid(),
            admin: meta.gid().wrapping_add(1),
        };
        let lock = rig.repository_ports(Default::default()).lock.clone();
        let mac = Arc::new(FakeMac::new(&root, &lock, ids, meta.uid(), meta.gid()));
        for tool in ["usr/sbin/installer", "usr/sbin/pkgutil"] {
            let path = root.join(tool);
            fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
            fs::write(&path, b"").expect("the tool");
        }
        let signers =
            Signers::new(vec![Fingerprint::parse(&leaf()).expect("a fingerprint")]).expect("one");
        MacRig {
            rig,
            mac,
            host: Host::new(&root, ids),
            root,
            signers,
        }
    }

    /// The installer's ports of the rig's Mac, as the daemon's start decides them.
    pub fn pkg_ports(&self) -> Option<MacosPkgPorts> {
        let tools: Arc<dyn Tools> = self.mac.clone();
        macos_pkg::ports(
            true,
            self.host.clone(),
            Some(self.signers.clone()),
            Some(tools),
        )
    }

    /// The daemon's ports: `macos-pkg` announced, its installer's ports held.
    pub fn ports(&self) -> Ports {
        let pkg = self.pkg_ports();
        let announcement = macos_pkg::announced(Announcement::default(), pkg.as_ref());
        let repository = RepositoryPorts::clone(&self.rig.repository_ports(Default::default()))
            .with_macos_pkg(pkg);
        Ports {
            announcement,
            repository: Arc::new(repository),
            ..self.rig.ports()
        }
    }

    pub fn daemon(&self) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.ports(), OWN)
    }

    /// Teaches the Mac `pkg`, the package of the service at `version`, served by its pin; its
    /// entry.
    pub fn publish(&self, version: &str, pkg: FakePkg) -> Target {
        let bytes = format!("fake-pkg:{SERVICE}-{version}");
        let digest = sha256_hex(bytes.as_bytes());
        self.rig
            .store
            .add_pinned(SERVICE, &digest, bytes.as_bytes());
        self.mac.add(pkg);
        entry(&digest, bytes.len() as u64)
    }

    /// The version directory of `version` of the service.
    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.rig.packages.join(SERVICE).join(version)
    }

    /// The file `logical` of the Mac.
    pub fn real(&self, logical: &str) -> PathBuf {
        self.root.join(logical.trim_start_matches('/'))
    }
}

/// The SHA-256 of `bytes`, in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The package of the service at `version`, signed by the host's signer, running [`LABEL`].
pub fn package(version: &str) -> FakePkg {
    FakePkg::standard(SERVICE, IDENTIFIER, version)
        .without(&format!("/Library/LaunchDaemons/{SERVICE}.plist"))
        .label(LABEL)
}

/// The manifest of an entry of the service: its job, the built-in steps, the installer's bounds.
pub fn manifest() -> Manifest {
    Manifest {
        min_hostkeeper: "0.1.0".to_owned(),
        package: Some(IDENTIFIER.to_owned()),
        run: vec![LABEL.to_owned()],
        hook: Some(Hook {
            timeout_secs: Some(StepBounds {
                prepare: 60,
                cutover: 60,
                verify: 60,
                rollback: 60,
                cleanup: 60,
                ..StepBounds::default()
            }),
            verify_deadline_secs: 300,
            ..Hook::default()
        }),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 60,
            remove_secs: 60,
        }),
        ..Manifest::default()
    }
}

/// The `macos-pkg` entry of a `.pkg` of `bytes` bytes pinned to `digest`, in the store `channel`.
pub fn entry(digest: &str, bytes: u64) -> Target {
    Target {
        installer: Installer::MacosPkg.as_str().to_owned(),
        locations: vec![hostkeeper_proto::hostkeeper::release::v1::Location {
            store: "channel".to_owned(),
            locator: format!("{SERVICE}/{SERVICE}.pkg"),
        }],
        digest: digest.to_owned(),
        bytes,
        manifest: Some(manifest()),
        ..Target::default()
    }
}

/// `order` with `entry` as its target.
pub fn with_entry(mut order: Order, entry: &Target) -> Order {
    use hostkeeper_proto::hostkeeper::agent::v1::order::Verb;
    match order.verb.as_mut() {
        Some(Verb::Install(a)) => a.target = Some(entry.clone()),
        Some(Verb::Update(a)) => a.target = Some(entry.clone()),
        Some(Verb::ForceRollback(a)) => a.target = Some(entry.clone()),
        Some(Verb::Force(a)) => a.target = Some(entry.clone()),
        other => panic!("no target to set on {other:?}"),
    }
    order
}

/// The order to install `version` from `entry`.
pub fn install_of(version: &str, entry: &Target, order_try: u64) -> Order {
    with_entry(install(SERVICE, version, &entry.digest, order_try), entry)
}

/// The order to update to `version` from `entry`.
pub fn update_of(version: &str, entry: &Target, order_try: u64) -> Order {
    with_entry(update(SERVICE, version, &entry.digest, order_try), entry)
}

/// Makes every directory under `dir` writable by its owner: an uninstall deletes the sealed,
/// read-only version directories, which root does through their mode on a host and the test's
/// own user cannot.
pub fn open_up(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    for entry in fs::read_dir(dir).expect("a directory").flatten() {
        open_up(&entry.path());
    }
}

/// The order to force `version` from `entry`, with no halt to lift.
pub fn force_of(version: &str, entry: &Target, order_try: u64) -> Order {
    with_entry(
        crate::support::force(SERVICE, version, &entry.digest, order_try, 0),
        entry,
    )
}
