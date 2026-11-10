//! The daemon of the apt device test: the shipped daemon's order path (`Daemon::order`) in this
//! process, as root, with the host's real step launcher (each tool a transient unit), `systemctl`
//! as its unit control, the host's announcement (its `os-release`, its apt and the rig's
//! repository entry), and the rig's repository configuration. Its packages, state and placed
//! record are the rig's own directories, never the host's service directories; what it installs
//! is the host's, as the rig's other tests install it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::adapters::host_systemctl;
use hostkeeper_daemon::executor::{Dirs, Placement};
use hostkeeper_daemon::packages::{Entry, PackageError, Packages, VersionDir};
use hostkeeper_daemon::prune::{LayoutPrune, NoLockPoint};
use hostkeeper_daemon::repository::RepositoryPorts;
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_daemon::{Daemon, Ports, repositories};
use hostkeeper_install::announce;
use hostkeeper_install::announce::OsHost;
use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{ObserveOptions, Observer};
use hostkeeper_placed::{RealFs, Roots, Systemctl};
use hostkeeper_proto::hostkeeper::agent::v1::{Install, Order, Uninstall, Update, order};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, StepBounds, Target,
};
use hostkeeper_steps::{ProcBootId, SystemClock, SystemdLauncher};
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::Owner;

use super::repo::PACKAGE;
use super::rig::{REPOSITORY, Rig};

/// The daemon's config: what every daemon reads, none of which an apt order uses.
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

/// A package store that has nothing: an apt order never asks it.
struct NoStore;

impl Packages for NoStore {
    fn version_dir(
        &self,
        _service: &ServiceName,
        _version: &Version,
        _digest: &PackageDigest,
        _index_digest: Option<&PackageDigest>,
        _entry: Option<&Entry>,
    ) -> Result<VersionDir, PackageError> {
        Err(PackageError::Other(
            "this daemon fetches no artifact".to_owned(),
        ))
    }
}

/// The root-owned directory the daemon's own directories are made in: a sealed version is
/// trusted only below directories that root owns (`Staged::check`), which the rig's scratch
/// directory under `target/` is not.
pub const BASE: &str = "/run/hostkeeper-ci";

/// The daemon on the rig's host, the installers the host announces, and the root-owned
/// directory its state, packages and placed record are in, kept until the test ends.
pub fn daemon(rig: &Rig) -> (Daemon, Vec<String>, Scratch) {
    daemon_with(rig, Arc::new(NoStore))
}

/// [`daemon`] with `store` as its package store: the archive versions of a service that moves to
/// apt are served by it.
pub fn daemon_with(rig: &Rig, store: Arc<dyn Packages>) -> (Daemon, Vec<String>, Scratch) {
    fs::create_dir_all(BASE).expect("the root-owned base");
    fs::set_permissions(BASE, fs::Permissions::from_mode(0o755)).expect("its mode");
    let scratch = Scratch::under(Path::new(BASE), "apt-daemon").expect("the daemon's directory");
    let base = scratch.path().to_path_buf();
    // Closed to others, as the rig's own work directory under the runner's home is: apt then
    // fetches and verifies as root, which reads the rig's key and repository there, and not as
    // `_apt`, which cannot (on a host the key and the work directory are readable to `_apt`).
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).expect("its mode");
    let (state, packages, placed) = (
        base.join("state"),
        base.join("packages"),
        base.join("placed"),
    );
    for dir in [&state, &packages, &placed] {
        fs::create_dir_all(dir).expect("a directory of the daemon");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    }
    let host = announce(OsHost {
        macos: false,
        root: Path::new("/"),
    });
    let announcement = repositories::announced(host, Path::new("/"), rig.repositories());
    let names = announcement.names().iter().map(|n| n.to_string()).collect();
    let observer = Observer::new(
        hostkeeper_observe::Ports {
            manager: Box::new(FakeServiceManager::default()),
            process: Box::new(FakeProcessProbe::default()),
            health: Box::new(FakeHealthClient::default()),
            expected: Box::new(FakeExpectedExe::default()),
        },
        ObserveOptions::default(),
    );
    let ports = Ports {
        launcher: Arc::new(SystemdLauncher::system()),
        packages: store,
        clock: Arc::new(SystemClock::new(Box::new(ProcBootId::host()))),
        deadman_poll_secs: None,
        units: Arc::new(Systemctl::new(host_systemctl())),
        placement: Placement {
            roots: Roots::new(&placed, Path::new("/etc/systemd/system"), &packages),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        },
        prune: Arc::new(LayoutPrune),
        force_lock_point: Arc::new(NoLockPoint),
        observer: Arc::new(observer),
        announcement,
        repository: Arc::new(RepositoryPorts::new(
            &packages,
            Path::new("/"),
            rig.repositories().clone(),
            Owner::ROOT,
        )),
    };
    let config = DaemonConfig::parse(CONFIG).expect("the config");
    let facts = packages.with_file_name("facts.json");
    let dirs = Dirs {
        state,
        packages,
        facts,
    };
    (Daemon::new(config, dirs, ports, OWN), names, scratch)
}

/// The version of the release of `package_version`: its upstream part, as the rig's orders name
/// it.
pub fn release(package_version: &str) -> String {
    format!(
        "{}.0",
        package_version.split('-').next().unwrap_or_default()
    )
}

/// The entry of the package at `package_version`, pinned to `pin`, from the rig's repository,
/// on the built-in steps.
pub fn entry(package_version: &str, pin: &str) -> Target {
    entry_running(package_version, pin, &[])
}

/// The program the hooked versions of the package install, and the arguments the manifest gives
/// it before the step's name.
pub const HOOK_COMMAND: [&str; 2] = ["/usr/bin/hkaptfix-hook", "update-hook"];

/// The entry of the package at `package_version`, pinned to `pin`, whose manifest names `command`
/// as its hook (empty: the built-in steps).
pub fn entry_running(package_version: &str, pin: &str, command: &[&str]) -> Target {
    let bounds = StepBounds {
        prepare: 120,
        cutover: 120,
        verify: 120,
        rollback: 120,
        cleanup: 120,
        ..StepBounds::default()
    };
    Target {
        installer: "apt".to_owned(),
        digest: pin.to_owned(),
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            package: Some(PACKAGE.to_owned()),
            run: vec!["hkaptfix.service".to_owned()],
            hook: Some(Hook {
                command: command.iter().map(|word| (*word).to_owned()).collect(),
                timeout_secs: Some(bounds),
                verify_deadline_secs: 600,
                ..Hook::default()
            }),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 120,
                activate_secs: 120,
                remove_secs: 120,
            }),
            ..Manifest::default()
        }),
        repository: Some(REPOSITORY.to_owned()),
        package_version: Some(package_version.to_owned()),
        ..Target::default()
    }
}

/// An install of the package at `package_version`, pinned to `pin`, as try `order_try`.
pub fn install(package_version: &str, pin: &str, order_try: u64) -> Order {
    install_of(entry(package_version, pin), package_version, pin, order_try)
}

/// An install of `target`, the entry of the package at `package_version` pinned to `pin`.
pub fn install_of(target: Target, package_version: &str, pin: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("install-{package_version}"),
        r#try: order_try,
        verb: Some(order::Verb::Install(Install {
            target: Some(target),
            service: PACKAGE.to_owned(),
            version: release(package_version),
            package_digest: pin.to_owned(),
            index_digest: String::new(),
        })),
    }
}

/// An update to the package at `package_version`, pinned to `pin`, as try `order_try`.
pub fn update(package_version: &str, pin: &str, order_try: u64) -> Order {
    update_of(entry(package_version, pin), package_version, pin, order_try)
}

/// An update to `target`, the entry of the package at `package_version` pinned to `pin`.
pub fn update_of(target: Target, package_version: &str, pin: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("update-{package_version}"),
        r#try: order_try,
        verb: Some(order::Verb::Update(Update {
            target: Some(target),
            service: PACKAGE.to_owned(),
            version: release(package_version),
            package_digest: pin.to_owned(),
            index_digest: String::new(),
        })),
    }
}

/// The uninstall of the service, as try `order_try`.
pub fn uninstall(order_try: u64) -> Order {
    Order {
        order_id: "uninstall".to_owned(),
        r#try: order_try,
        verb: Some(order::Verb::Uninstall(Uninstall {
            service: PACKAGE.to_owned(),
        })),
    }
}

/// What the daemon does at its start once its configuration is read, but for the work on
/// hostkeeper's own package: the sweep and reconciliation before the step records, the start pass
/// over them, and the work after it (which starts the run units no enablement link starts). The
/// rig's daemon is made in this process, so a start is these calls on it.
pub fn start(daemon: &Daemon) {
    let mut report = hostkeeper_daemon::BootReport::default();
    hostkeeper_daemon::start::before_records(daemon, &mut report);
    daemon
        .boot_pass_into(&mut report, &|_| false)
        .expect("the start pass");
    daemon.after_boot(&mut report, &hostkeeper_daemon::NoNotifier);
}
