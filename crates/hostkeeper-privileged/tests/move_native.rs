#![cfg(feature = "privileged")]
//! The move of a service from `hostkeeper-archive` to `apt` and its rollback
//! (docs/design/packages-native.md, section 10.6), as root, under the real systemd, apt and dpkg
//! of the runner, over the signed fixture repository of `install_apt.rs` (`install_apt/`). The
//! daemon is the shipped order path (`Daemon::order`) with the host's step launcher and
//! `systemctl`, as in the apt device test, and one more port: the package store serves the
//! archive version of the service (`hkaptfix` at 1.0.0), a sealed version directory the test
//! makes, whose unit is placed in `/etc/systemd/system` and whose hook restarts it. The fixture
//! package of the repository ships a unit of the same name in `/usr/lib/systemd/system`, the
//! situation of a service whose archive and whose package both carry a unit.
//!
//! What each test reads is what the host says: `systemctl show` (the file systemd loaded the unit
//! from, the command it runs), `systemctl is-active`, `dpkg-query`, `apt-mark showhold`, the
//! unit directory and the links of the service's package directory.
//!
//! Each test makes its own host (`install_apt/rig.rs` purges the package, releases its hold and
//! takes away its enablement link when it is made and again when it is dropped); the guard of
//! this file takes away the unit the archive version places. The names are the fixture's own.
//! No test asserts how long anything took.

#[allow(dead_code, reason = "each test file uses part of the apt device rig")]
#[path = "install_apt/daemon.rs"]
mod daemon;
#[allow(dead_code, reason = "each test file uses part of the apt device rig")]
#[path = "install_apt/repo.rs"]
mod repo;
#[allow(dead_code, reason = "each test file uses part of the apt device rig")]
#[path = "install_apt/rig.rs"]
mod rig;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::Reply;
use hostkeeper_daemon::packages::{Entry, PackageError, Packages, VersionDir};
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1::{ForceRollback, Install, Order, order};
use hostkeeper_steps::Staged;
use repo::PACKAGE;
use rig::{LINK, Rig, Trust, UNIT, dpkg_version, held, print};

/// The archive version the service runs before the move, and the apt version and package version
/// it moves to.
const ARCHIVE: &str = "1.0.0";
const MOVED: &str = "2.0.0";
const MOVED_PV: &str = "2.0-1";

/// The unit the archive version places, where systemd reads the units of the administrator.
const PLACED_UNIT: &str = "/etc/systemd/system/hkaptfix.service";
/// The units' name.
const NAME: &str = "hkaptfix.service";
/// Where Debian and Ubuntu with a merged `/usr` may report the package's unit.
const PACKAGE_UNITS: [&str; 2] = [UNIT, "/lib/systemd/system/hkaptfix.service"];

/// The archive version's manifest: the unit it runs and a hook, with every bound short.
fn manifest() -> String {
    format!(
        "schema = 1\nname = \"{PACKAGE}\"\nversion = \"{ARCHIVE}\"\nplatform = \"{}\"\n\
         min_hostkeeper = \"0.1.0\"\n\n[hook]\ncommand = [\"bin/hook\"]\n\
         verify_deadline_secs = 300\n\n[hook.timeout_secs]\nprepare = 60\ncutover = 60\n\
         verify = 60\nrollback = 60\ncleanup = 60\n\n[units]\nrun = [\"{NAME}\"]\n",
        hostkeeper_config::platform::Platform::current().as_str()
    )
}

/// The archive version's hook: `cutover` and `rollback` restart the unit, every step ends with
/// the last line of success.
const HOOK: &str = "#!/bin/sh\ncase \"$1\" in\ncutover|rollback) systemctl restart hkaptfix.service || exit 1 ;;\nesac\necho '{}'\n";

/// The archive version's unit: not the package's program, so that what runs tells the two apart.
const ARCHIVE_UNIT: &str = "[Unit]\nDescription=hkaptfix archive version\n\n[Service]\n\
    ExecStart=/bin/sh -c 'exec sleep infinity'\n\n[Install]\nWantedBy=multi-user.target\n";

/// Writes `text` at `path` with `mode`, making its directory.
fn put(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("a directory of the package");
    fs::write(path, text).expect("a file of the package");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// Makes the archive version's directory under `packages` (the daemon's packages directory).
fn make_archive_version(packages: &Path) {
    let service = packages.join(PACKAGE);
    let dir = service.join(ARCHIVE);
    put(&dir.join("manifest.toml"), &manifest(), 0o644);
    put(&dir.join("bin/hook"), HOOK, 0o755);
    put(&dir.join("units").join(NAME), ARCHIVE_UNIT, 0o644);
    for path in [
        packages.to_path_buf(),
        service,
        dir.clone(),
        dir.join("bin"),
        dir.join("units"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("a directory's mode");
    }
}

/// The package store of the archive: the sealed version directory the test made, and the
/// versions it was asked for.
struct ArchiveStore {
    root: PathBuf,
    asked: Mutex<Vec<String>>,
}

impl ArchiveStore {
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Packages for ArchiveStore {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        _digest: &PackageDigest,
        _index_digest: Option<&PackageDigest>,
        _entry: Option<&Entry>,
    ) -> Result<VersionDir, PackageError> {
        self.asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{service} {version}"));
        let dir = self.root.join(service.as_str()).join(version.as_str());
        Staged::seal(&dir)
            .map(VersionDir::new)
            .map_err(|e| PackageError::Other(format!("{}: {e}", dir.display())))
    }
}

/// Takes the unit the archive version places off the host when the test starts and when it ends,
/// however it ends. Declared after the rig, so that it is dropped first.
struct PlacedUnit;

impl PlacedUnit {
    fn take_away() {
        print("systemctl", &["stop", NAME]);
        let _ = fs::remove_file(PLACED_UNIT);
        print("systemctl", &["daemon-reload"]);
    }

    fn new() -> PlacedUnit {
        PlacedUnit::take_away();
        PlacedUnit
    }
}

impl Drop for PlacedUnit {
    fn drop(&mut self) {
        PlacedUnit::take_away();
    }
}

/// The host of one test: the rig, the guard of the archive's unit, the store and the daemon.
struct Host {
    daemon: Daemon,
    store: Arc<ArchiveStore>,
    // Dropped in the order declared: the guard first, the rig (which purges the package) last.
    _placed: PlacedUnit,
    rig: Rig,
}

impl Host {
    fn new(name: &str) -> Host {
        let rig = Rig::new(name, Trust::Entry);
        let placed = PlacedUnit::new();
        let packages = rig.scratch().join("packages");
        fs::create_dir_all(&packages).expect("the packages directory");
        make_archive_version(&packages);
        let store = Arc::new(ArchiveStore {
            root: packages,
            asked: Mutex::new(Vec::new()),
        });
        let (daemon, names) = daemon::daemon_with(&rig, store.clone());
        assert_eq!(names, ["hostkeeper-archive", "apt"]);
        Host {
            daemon,
            store,
            _placed: placed,
            rig,
        }
    }

    /// The verdict and the reason of the order's reply, which must have finished.
    fn finished(&self, order: &Order) -> (Verdict, Option<String>) {
        match self.daemon.order(order) {
            Reply::Finished {
                verdict, reason, ..
            } => (verdict, reason),
            other => panic!("the order did not finish: {other:?}"),
        }
    }

    fn succeeds(&self, order: &Order) {
        let succeeded = Verdict::Succeeded {
            promotion_pending: false,
        };
        assert_eq!(self.finished(order), (succeeded, None), "{order:?}");
    }

    /// The link `name` of the service's package directory: the version it names.
    fn linked(&self, name: &str) -> Option<String> {
        fs::read_link(self.rig.scratch().join("packages").join(PACKAGE).join(name))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The installer `placed.json` records for the service, and the paths it lists for it.
    fn recorded(&self) -> (Option<String>, Vec<PathBuf>) {
        let record = self
            .daemon
            .ctx()
            .placer()
            .store()
            .load()
            .expect("placed.json loads");
        let paths = record
            .paths
            .iter()
            .filter(|(_, entry)| entry.service == PACKAGE)
            .map(|(path, _)| path.clone())
            .collect();
        (record.installer_of(PACKAGE).map(str::to_owned), paths)
    }
}

/// What systemd says of the unit's property `name`.
fn shown(name: &str) -> String {
    print("systemctl", &["show", "--value", "-p", name, NAME])
        .text
        .trim()
        .to_owned()
}

fn is_active() -> bool {
    print("systemctl", &["is-active", NAME]).text.trim() == "active"
}

/// The install of the archive version, and what the host shows of it: the unit and its link
/// placed in the unit directory, the unit systemd loads that one and running its program, the
/// package not installed.
fn install_archive(host: &Host) {
    let digest = "ab".repeat(32);
    host.succeeds(&Order {
        order_id: "install-archive".to_owned(),
        r#try: 1,
        verb: Some(order::Verb::Install(Install {
            target: None,
            service: PACKAGE.to_owned(),
            version: ARCHIVE.to_owned(),
            package_digest: digest,
            index_digest: String::new(),
        })),
    });
    assert!(
        fs::symlink_metadata(PLACED_UNIT).is_ok(),
        "the archive's unit is placed"
    );
    assert!(
        fs::symlink_metadata(LINK).is_ok(),
        "the archive's WantedBy link is placed"
    );
    assert_eq!(shown("FragmentPath"), PLACED_UNIT);
    assert!(
        shown("ExecStart").contains("sleep"),
        "{}",
        shown("ExecStart")
    );
    assert!(is_active(), "the archive's unit runs");
    assert_eq!(dpkg_version(), None, "the package is not installed");
    assert_eq!(host.recorded().0, None, "a hostkeeper-archive service");
    assert_eq!(
        host.recorded().1.len(),
        2,
        "its unit and its link are placed"
    );
}

/// The move: the archive version updated to [`MOVED`] by the apt entry of the package.
fn move_to_apt(host: &Host) {
    install_archive(host);
    let pin = host.rig.pin(MOVED_PV);
    host.succeeds(&daemon::update(MOVED_PV, &pin, 2));
}

/// Proves the move on a real host: a service installed from `hostkeeper-archive` with its unit
/// placed in `/etc/systemd/system` is ordered an apt entry of its package and ends `Succeeded`.
/// The archive's placed unit and its link are gone from the unit directory and from `placed.json`;
/// the unit systemd loads is the package's, from `/usr/lib/systemd/system`, running the package's
/// program and active after the cutover; the package is installed and held, its enablement link
/// is the package unit's, `placed.json` names `apt`, `current` is the apt version and `previous`
/// the archive one, and the archive port was asked for nothing but the first install.
///
/// Catches: an archive unit left in `/etc/systemd/system` that hides the package's unit of the
/// same name, a move refused as `installer_changed`, and a unit left stopped by the move.
#[test]
fn a_service_installed_from_the_archive_moves_to_apt_and_its_unit_is_the_packages() {
    let host = Host::new("move-native-forward");
    move_to_apt(&host);

    assert!(
        fs::symlink_metadata(PLACED_UNIT).is_err(),
        "the archive's unit is gone from the unit directory"
    );
    assert_eq!(
        host.recorded(),
        (Some("apt".to_owned()), Vec::new()),
        "placed.json names apt and lists no archive path"
    );
    let fragment = shown("FragmentPath");
    assert!(
        PACKAGE_UNITS.contains(&fragment.as_str()),
        "systemd loaded the unit from {fragment}"
    );
    assert!(
        shown("ExecStart").contains("/usr/bin/hkaptfix"),
        "{}",
        shown("ExecStart")
    );
    assert!(is_active(), "the package's unit is active after cutover");
    assert_eq!(dpkg_version().as_deref(), Some(MOVED_PV));
    assert!(held().iter().any(|p| p == PACKAGE), "the package is held");
    assert_eq!(
        fs::read_link(LINK).expect("the enablement link"),
        Path::new(UNIT),
        "the package's unit is the one enabled"
    );
    assert_eq!(host.linked("current").as_deref(), Some(MOVED));
    assert_eq!(host.linked("previous").as_deref(), Some(ARCHIVE));
    assert_eq!(
        host.store.asked(),
        [format!("{PACKAGE} {ARCHIVE}")],
        "the archive port was asked for the first install alone"
    );
}

/// Proves the move's rollback on a real host: after the move settled, the repository is removed
/// from the disk and a `force_rollback` to the archive version kept as `previous` ends
/// `Succeeded`. The package is purged (dpkg knows none of it, its files, its enablement link and
/// its hold are gone), the archive's unit and link are placed again and are the unit systemd
/// loads, running the archive's program and active, `placed.json` records no installer and lists
/// the archive's two paths, `current` is the archive version, and the archive port was not asked
/// again.
///
/// Catches: a rollback that leaves the package installed beside the archive, one that leaves the
/// archive's unit unplaced or the unit stopped, and one that fetches what the host keeps.
#[test]
fn the_moves_rollback_purges_the_package_and_places_the_archive_unit_again() {
    let host = Host::new("move-native-rollback");
    move_to_apt(&host);
    fs::remove_dir_all(&host.rig.repo).expect("remove the repository from the disk");

    host.succeeds(&Order {
        order_id: "rollback-archive".to_owned(),
        r#try: 3,
        verb: Some(order::Verb::ForceRollback(ForceRollback {
            target: None,
            service: PACKAGE.to_owned(),
            version: ARCHIVE.to_owned(),
            package_digest: "ab".repeat(32),
            index_digest: String::new(),
        })),
    });

    assert_eq!(dpkg_version(), None, "the package is purged");
    assert!(!held().iter().any(|p| p == PACKAGE), "the hold is gone");
    assert!(
        fs::symlink_metadata(UNIT).is_err(),
        "the package's unit file is gone"
    );
    assert!(
        fs::symlink_metadata("/usr/bin/hkaptfix").is_err(),
        "the package's program is gone"
    );
    assert!(
        fs::symlink_metadata(PLACED_UNIT).is_ok(),
        "the archive's unit is placed again"
    );
    assert!(
        fs::symlink_metadata(LINK).is_ok(),
        "the archive's link is placed again"
    );
    assert_eq!(shown("FragmentPath"), PLACED_UNIT);
    assert!(
        shown("ExecStart").contains("sleep"),
        "{}",
        shown("ExecStart")
    );
    assert!(is_active(), "the archive's unit is active");
    let (installer, paths) = host.recorded();
    assert_eq!(installer, None, "a hostkeeper-archive service again");
    assert_eq!(paths.len(), 2, "the archive's unit and link: {paths:?}");
    assert_eq!(host.linked("current").as_deref(), Some(ARCHIVE));
    assert_eq!(
        host.store.asked(),
        [format!("{PACKAGE} {ARCHIVE}")],
        "nothing was fetched for the rollback"
    );
}
