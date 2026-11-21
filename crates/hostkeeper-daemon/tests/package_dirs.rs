//! The start's closing of the package directories an earlier release left open
//! ([`hostkeeper_daemon::package_dirs`]): what hostkeeper keeps of a package is root's alone, so
//! the start sets mode 0700 on the packages directory, each service and version directory in it,
//! and the apt work directories' and mirrors' parents, says what it closed in one log line, and
//! leaves what is below a version (its tree digest) as it was. A version closed so is rolled back
//! to as before, on Linux (apt) and on a Mac (macos-pkg).

mod support;
mod support_apt;
mod support_macos_pkg;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_daemon::boot::{ALERT_START, alert_line};
use hostkeeper_daemon::package_dirs::close;
use hostkeeper_daemon::start::startup;
use hostkeeper_steps::tree_digest;
use support::{Rig, alerting, force, lines};
use support_apt::seen::{held, install_of, linked, succeeded, update_of};
use support_apt::{AptRig, SERVICE, entry, package, with_entry};

/// The permission bits of `path`, not followed.
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// The apt work directories' parent and the mirrors' parent of `rig`, beside its packages
/// directory.
fn beside(rig: &Rig) -> [PathBuf; 2] {
    let state = rig
        .packages
        .parent()
        .expect("the packages directory's parent");
    [state.join("apt"), state.join("mirror")]
}

/// Lays out what an earlier release left on a host, open to every user: the packages directory
/// and the service directory `web` 0755, its version `1.0.0` 0555 with a file (0444) and a
/// directory (0555) in it, its seal 0444, `current` naming it, a file beside the service, and the
/// apt work and mirror directories 0755. Answers the version directory.
fn left_open(rig: &Rig) -> PathBuf {
    let service = rig.packages.join("web");
    let version = service.join("1.0.0");
    fs::create_dir_all(version.join("tree")).expect("the version");
    fs::write(version.join("artifact"), b"the package's bytes").expect("the artifact");
    set_mode(&version.join("artifact"), 0o444);
    set_mode(&version.join("tree"), 0o555);
    fs::write(service.join("1.0.0.seal"), b"{}").expect("a seal");
    set_mode(&service.join("1.0.0.seal"), 0o444);
    symlink("1.0.0", service.join("current")).expect("current");
    fs::write(rig.packages.join("not-a-service"), b"x").expect("a file");
    for dir in beside(rig) {
        fs::create_dir_all(dir.join("web")).expect("a directory beside");
        set_mode(&dir, 0o755);
    }
    set_mode(&version, 0o555);
    set_mode(&service, 0o755);
    set_mode(&rig.packages, 0o755);
    version
}

/// Catches a start that leaves an earlier release's package directories readable by other users,
/// or that closes them by changing what a seal holds: after the start, the packages directory,
/// the service directory, the version directory and the apt work and mirror parents are 0700,
/// and the start's report lists them, outermost first; the file and the directory in the
/// version keep their modes, so the tree digest is the same; `current`, the seal and a file
/// beside the service are left as they are; and a second start closes nothing.
#[test]
fn the_start_closes_the_package_directories_an_earlier_release_left_open() {
    let rig = Rig::new("close-at-start");
    let version = left_open(&rig);
    let digest = tree_digest(&version).expect("the digest");
    let [work, mirror] = beside(&rig);

    let started = startup(&rig.settings(), rig.ports(), support::OWN).expect("the daemon starts");

    let service = rig.packages.join("web");
    let closed = [
        rig.packages.clone(),
        service.clone(),
        version.clone(),
        work.clone(),
        mirror.clone(),
    ];
    assert_eq!(started.boot_report().closed_package_dirs, closed);
    for dir in &closed {
        assert_eq!(mode(dir), 0o700, "{}", dir.display());
    }
    assert_eq!(mode(&version.join("artifact")), 0o444);
    assert_eq!(mode(&version.join("tree")), 0o555);
    assert_eq!(tree_digest(&version).expect("the digest"), digest);
    assert_eq!(mode(&service.join("1.0.0.seal")), 0o444);
    assert_eq!(
        fs::read_link(service.join("current")).expect("current"),
        Path::new("1.0.0")
    );
    assert_eq!(mode(&work.join("web")), 0o755, "below the closed parent");
    drop(started);

    let again = startup(&rig.settings(), rig.ports(), support::OWN).expect("it starts again");
    assert_eq!(
        again.boot_report().closed_package_dirs,
        Vec::<PathBuf>::new()
    );
}

/// Catches the close passing unsaid, saying something else, or saying it twice: the log line
/// names every directory it closed, in order, and a second close logs nothing.
#[test]
fn the_close_says_what_it_closed_in_one_line_and_a_repeat_says_nothing() {
    let rig = Rig::new("close-log");
    let version = left_open(&rig);
    let [work, mirror] = beside(&rig);
    let (daemon, log) = alerting(rig.daemon());

    let closed = close(daemon.ctx());
    let service = rig.packages.join("web");
    let named = [&rig.packages, &service, &version, &work, &mirror];
    assert_eq!(closed, named.map(|p| p.to_path_buf()));
    let paths: Vec<String> = named.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(
        lines(&log),
        [format!(
            "hostkeeper daemon: level=info closed to every user but root the package \
             directories an earlier release left open: {}",
            paths.join(" ")
        )]
    );

    assert_eq!(close(daemon.ctx()), Vec::<PathBuf>::new());
    assert_eq!(lines(&log).len(), 1);
}

/// Catches a directory that cannot be closed passing unsaid or stopping the close: a service
/// directory whose entries cannot be looked at (mode 0600: listed, not searched) and one that
/// cannot be listed (0300) are each named in the one alert `start_incomplete`, with why, and the
/// close goes on to the rest. A host with nothing open and no packages directory closes nothing
/// and says nothing.
#[test]
fn a_package_directory_that_cannot_be_looked_into_is_an_alert_and_the_close_goes_on() {
    let rig = Rig::new("close-alert");
    left_open(&rig);
    let unsearchable = rig.packages.join("db");
    let unlistable = rig.packages.join("mq");
    for (dir, closed_mode) in [(&unsearchable, 0o600), (&unlistable, 0o300)] {
        fs::create_dir_all(dir.join("1.0.0")).expect("a version");
        set_mode(dir, closed_mode);
    }
    let (daemon, log) = alerting(rig.daemon());

    let closed = close(daemon.ctx());

    set_mode(&unsearchable, 0o700);
    set_mode(&unlistable, 0o700);
    assert!(
        closed.contains(&rig.packages.join("web/1.0.0")),
        "{closed:?}"
    );
    let denied = "Permission denied (os error 13)";
    assert_eq!(
        lines(&log)[1..],
        [alert_line(
            ALERT_START,
            &format!(
                "a package directory was not closed to other users: {}: {denied}; {}: {denied}",
                unsearchable.join("1.0.0").display(),
                unlistable.display(),
            )
        )]
    );

    let empty = Rig::new("close-nothing");
    assert!(!empty.packages.exists(), "no packages directory");
    let (daemon, log) = alerting(empty.daemon());
    assert_eq!(close(daemon.ctx()), Vec::<PathBuf>::new());
    assert_eq!(lines(&log), Vec::<String>::new());
}

/// Opens the packages directory, the service directory and each version directory of `service`
/// as an earlier release left them (0755, 0755, 0555).
fn open_as_before(packages: &Path, service: &str) {
    let dir = packages.join(service);
    for entry in fs::read_dir(&dir).expect("the service directory").flatten() {
        let path = entry.path();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            set_mode(&path, 0o555);
        }
    }
    set_mode(&dir, 0o755);
    set_mode(packages, 0o755);
}

/// Catches a version closed by the start that can no longer be rolled back to: an apt service
/// at 2.0.0 with 1.0.0 kept, its directories opened as an earlier release left them, is closed
/// by the start, and a force to 1.0.0 then installs it from its sealed copy, with nothing
/// fetched, and the directories stay closed.
#[test]
fn an_apt_version_closed_at_the_start_is_rolled_back_to_from_its_sealed_copy() {
    let apt = AptRig::new("close-apt-rollback");
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    let daemon = apt.daemon();
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        succeeded()
    );
    drop(daemon);
    open_as_before(&apt.rig.packages, SERVICE);

    let started =
        startup(&apt.rig.settings(), apt.ports(), support::OWN).expect("the daemon starts");
    let kept = apt.rig.packages.join(SERVICE).join("1.0.0");
    assert!(
        started.boot_report().closed_package_dirs.contains(&kept),
        "{:?}",
        started.boot_report().closed_package_dirs
    );
    apt.host.empty_repositories();
    let calls = apt.host.calls().len();
    let order = with_entry(force(SERVICE, "1.0.0", &one, 3, 0), &entry("1.0.0-1", &one));

    assert_eq!(started.daemon().order(&order), succeeded());

    let fetched = apt.host.calls()[calls..]
        .iter()
        .any(|c| c.label == "apt-get update" || c.label == "apt-get download");
    assert!(!fetched, "nothing was fetched");
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    for dir in [
        apt.rig.packages.clone(),
        apt.rig.packages.join(SERVICE),
        kept,
    ] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
}

/// Catches a `.pkg` version closed by the start that can no longer be rolled back to: a Mac
/// service at 1.1.0 with 1.0.0 kept, its directories opened as an earlier release left them, is
/// closed by the start, and a force to 1.0.0 then installs the kept `.pkg` (its receipt names
/// 1.0.0), with nothing fetched.
#[test]
fn a_pkg_version_closed_at_the_start_is_rolled_back_to_from_its_kept_pkg() {
    use support_macos_pkg::{IDENTIFIER, MacRig, force_of};
    let mac = MacRig::new("close-pkg-rollback");
    let one = mac.publish("1.0.0", support_macos_pkg::package("1.0.0"));
    let two = mac.publish("1.1.0", support_macos_pkg::package("1.1.0"));
    let daemon = mac.daemon();
    assert_eq!(
        daemon.order(&support_macos_pkg::install_of("1.0.0", &one, 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&support_macos_pkg::update_of("1.1.0", &two, 2)),
        succeeded()
    );
    drop(daemon);
    let service = support_macos_pkg::SERVICE;
    assert_eq!(mode(&mac.version_dir("1.0.0").join("artifact")), 0o400);
    open_as_before(&mac.rig.packages, service);

    let started =
        startup(&mac.rig.settings(), mac.ports(), support::OWN).expect("the daemon starts");
    assert!(
        started
            .boot_report()
            .closed_package_dirs
            .contains(&mac.version_dir("1.0.0")),
        "{:?}",
        started.boot_report().closed_package_dirs
    );
    let fetched = mac.rig.pins.asked().len();

    assert_eq!(
        started.daemon().order(&force_of("1.0.0", &one, 3)),
        succeeded()
    );

    assert_eq!(
        mac.mac.receipt(IDENTIFIER).map(|receipt| receipt.version),
        Some("1.0.0".to_owned())
    );
    assert_eq!(mac.rig.pins.asked().len(), fetched, "nothing was fetched");
    assert_eq!(mode(&mac.version_dir("1.0.0")), 0o700);
}
