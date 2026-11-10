//! The daemon of the `macos-pkg` device test: the shipped daemon's order path (`Daemon::order`)
//! in this process, as root, with the Mac's own adapters (`launchctl`, the launchd step
//! launcher), the host's announcement decided as a daemon's start decides it (the signers file
//! the test names, loaded by the daemon's loader, and the host's `installer` and `pkgutil`), and
//! the `macos-pkg` installer over the Mac's real tools but for the one answered call of
//! [`super::Signed`]: `pkgutil --check-signature` of a file whose SHA-256 the test names
//! ([`SignedBytes`]). Its packages, state and placed record are the test's own directories under
//! a root-owned base, never the host's service directories; what it installs is the host's.
//! Its package store serves each `.pkg` by its pin from the test's files ([`PkgStore`]).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::adapters::{Adapters, host_clock};
use hostkeeper_daemon::executor::{Dirs, Placement};
use hostkeeper_daemon::macos_pkg;
use hostkeeper_daemon::packages::{PackageError, Packages, Pinned};
use hostkeeper_daemon::prune::{LayoutPrune, NoLockPoint};
use hostkeeper_daemon::repository::RepositoryPorts;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_install::announce::{OsHost, announce};
use hostkeeper_install::host::Host;
use hostkeeper_install::tools::{ToolError, ToolOutput};
use hostkeeper_install::version::Owner;
use hostkeeper_install::{HostTools, Tool, Tools};
use hostkeeper_launchd::Platform;
use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{ObserveOptions, Observer};
use hostkeeper_placed::{RealFs, Roots};
use hostkeeper_proto::hostkeeper::agent::v1::{Install, Order, Uninstall, Update, order};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Location, Manifest, StepBounds, Target,
};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

use super::{IDENTIFIER, SERVICE, signed_answer};

/// The daemon's config: what every daemon reads, none of which a `macos-pkg` order uses.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The daemon's version.
const OWN: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

/// The root-owned directory the daemon's own directories are made in: a sealed version is
/// trusted only below directories that root owns (`Staged::check`), which the test's scratch
/// directory under `target/` is not. `/private/var` is `/var` with no link on the way.
const BASE: &str = "/private/var";

/// The SHA-256 of the file at `path`, in hex.
pub fn sha256_of(path: &Path) -> String {
    let bytes = fs::read(path).expect("read the package");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The Mac's tools, but for `pkgutil --check-signature` of a file whose SHA-256 is one the test
/// named ([`SignedBytes::sign`]), answered as signed by the host's signer with a chain the Mac
/// trusts. The daemon names the `.pkg` by the path of its own copy, so the answer goes by the
/// bytes. Every other call is the Mac's.
#[derive(Default)]
pub struct SignedBytes {
    digests: Mutex<Vec<String>>,
}

impl SignedBytes {
    fn sign(&self, digest: &str) {
        self.digests
            .lock()
            .expect("digests")
            .push(digest.to_owned());
    }
}

impl Tools for SignedBytes {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        if tool == Tool::Pkgutil && args.first().map(String::as_str) == Some("--check-signature") {
            let package = args.get(1).cloned().unwrap_or_default();
            let signed = fs::metadata(&package).is_ok_and(|meta| meta.is_file())
                && self
                    .digests
                    .lock()
                    .expect("digests")
                    .contains(&sha256_of(Path::new(&package)));
            if signed {
                return Ok(ToolOutput {
                    code: Some(0),
                    stdout: signed_answer(&package).into_bytes(),
                    stderr: Vec::new(),
                });
            }
        }
        HostTools.run(tool, args, deadline)
    }
}

/// A package store of the test's `.pkg` files, each served by its pin into a file of the
/// service's package directory, removed when the daemon drops it. It stages no artifact.
pub struct PkgStore {
    /// The daemon's packages directory.
    packages: PathBuf,
    files: Mutex<BTreeMap<String, PathBuf>>,
    asked: Mutex<Vec<String>>,
}

impl PkgStore {
    /// The digests asked for, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked").clone()
    }
}

/// A file the store wrote, removed when dropped.
struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Packages for PkgStore {
    fn pinned(&self, service: &ServiceName, entry: &Target) -> Result<Pinned, PackageError> {
        self.asked.lock().expect("asked").push(entry.digest.clone());
        let from = self
            .files
            .lock()
            .expect("files")
            .get(&entry.digest)
            .cloned()
            .ok_or_else(|| PackageError::NotFound(entry.digest.clone()))?;
        let copy = Copy(
            self.packages
                .join(service.as_str())
                .join(format!(".fetch-test-{}", &entry.digest[..12])),
        );
        fs::create_dir_all(copy.0.parent().expect("the service's directory"))
            .expect("the service's package directory");
        fs::copy(&from, &copy.0).expect("the fetched copy");
        let path = copy.0.clone();
        Ok(Pinned::new(&path, Box::new(copy)))
    }
}

/// The daemon of the test, its store and its tools, the installers it announces, and the
/// root-owned directory its state, packages and placed record are in, kept until the test ends.
pub struct MacDaemon {
    pub daemon: Daemon,
    pub store: Arc<PkgStore>,
    pub tools: Arc<SignedBytes>,
    pub names: Vec<String>,
    pub packages: PathBuf,
    _base: Scratch,
}

impl MacDaemon {
    /// Serves `pkg`, a `.pkg` of the test at `version`, by its pin, and answers its signature as
    /// the host's signer's when `signed`; its entry.
    pub fn publish(&self, version: &str, pkg: &Path, signed: bool) -> Target {
        let digest = sha256_of(pkg);
        self.store
            .files
            .lock()
            .expect("files")
            .insert(digest.clone(), pkg.to_path_buf());
        if signed {
            self.tools.sign(&digest);
        }
        let bytes = fs::metadata(pkg).expect("the package").len();
        entry(version, &digest, bytes)
    }
}

/// The daemon on this Mac whose signers file is `signers_file` (absent: none), as the daemon's
/// start loads it, owned by root.
pub fn daemon(signers_file: &Path) -> MacDaemon {
    let base = Scratch::under(Path::new(BASE), "hk-pkg-daemon").expect("the daemon's directory");
    fs::set_permissions(base.path(), fs::Permissions::from_mode(0o755)).expect("its mode");
    let (state, packages, placed) = (
        base.path().join("state"),
        base.path().join("packages"),
        base.path().join("placed"),
    );
    for dir in [&state, &packages, &placed] {
        fs::create_dir_all(dir).expect("a directory of the daemon");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    }
    let signers = hostkeeper_install::macos_pkg::signers::load(signers_file, 0)
        .expect("the daemon's loader takes the signers file");
    let tools = Arc::new(SignedBytes::default());
    let pkg = macos_pkg::ports(
        true,
        Host::mac(),
        signers,
        Some(Arc::clone(&tools) as Arc<dyn Tools>),
    );
    let host = announce(OsHost {
        macos: true,
        root: Path::new("/"),
    });
    let announcement = macos_pkg::announced(host, pkg.as_ref());
    let names = announcement.names();
    let adapters = Adapters::for_platform(&Platform::macos()).expect("the Mac's adapters");
    let observer = Observer::new(
        hostkeeper_observe::Ports {
            manager: Box::new(FakeServiceManager::default()),
            process: Box::new(FakeProcessProbe::default()),
            health: Box::new(FakeHealthClient::default()),
            expected: Box::new(FakeExpectedExe::default()),
        },
        ObserveOptions::default(),
    );
    let store = Arc::new(PkgStore {
        packages: packages.clone(),
        files: Mutex::default(),
        asked: Mutex::default(),
    });
    let ports = Ports {
        launcher: adapters.launcher,
        packages: Arc::clone(&store) as Arc<dyn Packages>,
        clock: Arc::new(host_clock()),
        deadman_poll_secs: None,
        units: adapters.units,
        placement: Placement {
            roots: Roots::new(&placed, &adapters.unit_dir, &packages),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        },
        prune: Arc::new(LayoutPrune),
        force_lock_point: Arc::new(NoLockPoint),
        observer: Arc::new(observer),
        announcement,
        repository: Arc::new(
            RepositoryPorts::new(&packages, Path::new("/"), Default::default(), Owner::ROOT)
                .with_macos_pkg(pkg),
        ),
    };
    let config = DaemonConfig::parse(CONFIG).expect("the config");
    let dirs = Dirs {
        state,
        packages: packages.clone(),
        facts: packages.with_file_name("facts.json"),
    };
    MacDaemon {
        daemon: Daemon::new(config, dirs, ports, OWN),
        store,
        tools,
        names,
        packages,
        _base: base,
    }
}

/// The `macos-pkg` entry of the fixture at `version`, pinned to `digest` of `bytes` bytes, with
/// no job and the built-in steps.
pub fn entry(version: &str, digest: &str, bytes: u64) -> Target {
    let bounds = StepBounds {
        prepare: 300,
        cutover: 300,
        verify: 300,
        rollback: 300,
        cleanup: 300,
        ..StepBounds::default()
    };
    Target {
        installer: "macos-pkg".to_owned(),
        locations: vec![Location {
            store: "channel".to_owned(),
            locator: format!("{SERVICE}/{version}/{SERVICE}-{version}.pkg"),
        }],
        digest: digest.to_owned(),
        bytes,
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            package: Some(IDENTIFIER.to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(bounds),
                verify_deadline_secs: 600,
                ..Hook::default()
            }),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 300,
                activate_secs: 300,
                remove_secs: 300,
            }),
            ..Manifest::default()
        }),
        ..Target::default()
    }
}

/// An install of `version` from `entry`, as try `order_try`.
pub fn install(version: &str, entry: &Target, order_try: u64) -> Order {
    Order {
        order_id: format!("install-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::Install(Install {
            target: Some(entry.clone()),
            service: SERVICE.to_owned(),
            version: version.to_owned(),
            package_digest: entry.digest.clone(),
            index_digest: String::new(),
        })),
    }
}

/// An update to `version` from `entry`, as try `order_try`.
pub fn update(version: &str, entry: &Target, order_try: u64) -> Order {
    Order {
        order_id: format!("update-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::Update(Update {
            target: Some(entry.clone()),
            service: SERVICE.to_owned(),
            version: version.to_owned(),
            package_digest: entry.digest.clone(),
            index_digest: String::new(),
        })),
    }
}

/// An uninstall of the service, as try `order_try`.
pub fn uninstall(order_try: u64) -> Order {
    Order {
        order_id: "uninstall".to_owned(),
        r#try: order_try,
        verb: Some(order::Verb::Uninstall(Uninstall {
            service: SERVICE.to_owned(),
        })),
    }
}
