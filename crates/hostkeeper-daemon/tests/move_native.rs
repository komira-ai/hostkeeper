//! The move of a service from `hostkeeper-archive` to `apt`, and its rollback
//! (docs/design/packages-native.md, section 10.6; packages-tests.md, "The move (A19)"): the one
//! installer change a daemon accepts is from the archive to a native installer; the move takes
//! the archive's placed paths away before the package's units are enabled, keeps the archive
//! version as `previous`, and its rollback, before or after it settled, purges the package and
//! places the archive version again from its sealed copy. apt and dpkg are the repository
//! installer's fake, answering the daemon's own step launcher.

mod support;
mod support_apt;

use std::path::PathBuf;

use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::target;
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_install::{Announcement, Family};
use hostkeeper_placed::PathState;
use hostkeeper_placed::testing::UnitCall;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Manifest, Target};
use hostkeeper_release::Installer;
use support::{OWN, config, d, force_rollback, install, update};
use support_apt::seen::{failed, held, linked, recorded, succeeded, unit_calls, update_of};
use support_apt::{AptRig, SERVICE, UNIT, entry, package, with_entry};

/// The archive version the service runs before the move.
const ARCHIVE: &str = "1.0.0";
/// The apt version it moves to, and its package version.
const MOVED: &str = "2.0.0";
const MOVED_PV: &str = "2.0.0-1";

/// The link the package's unit is enabled by, as the repository installer writes it.
const ENABLED: &str = "/etc/systemd/system/multi-user.target.wants/web.service";

/// The paths the archive version places: its unit, the unit's `WantedBy=` link and its file.
fn archive_paths(apt: &AptRig) -> [PathBuf; 3] {
    apt.rig.placed_paths(SERVICE)
}

/// The placed paths `placed.json` lists for the service, with their state.
fn placed(apt: &AptRig) -> Vec<(PathBuf, PathState)> {
    apt.rig
        .placed_record()
        .paths
        .into_iter()
        .filter(|(_, entry)| entry.service == SERVICE)
        .map(|(path, entry)| (path, entry.state))
        .collect()
}

/// The archive's paths, each placed and confirmed.
fn archive_placed(apt: &AptRig) -> Vec<(PathBuf, PathState)> {
    let mut paths: Vec<(PathBuf, PathState)> = archive_paths(apt)
        .into_iter()
        .map(|path| (path, PathState::Placed))
        .collect();
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    paths
}

/// The service installed from archives at [`ARCHIVE`], placing its unit, its unit's link and its
/// file.
fn on_archives(apt: &AptRig, daemon: &Daemon) {
    apt.rig.placed(SERVICE, ARCHIVE, None);
    assert_eq!(
        daemon.order(&install(SERVICE, ARCHIVE, &d(), 1)),
        succeeded()
    );
    for path in archive_paths(apt) {
        assert!(apt.rig.there(&path), "{} is placed", path.display());
    }
    assert_eq!(placed(apt), archive_placed(apt));
    assert_eq!(recorded(daemon), None);
}

/// The service moved: on archives, then updated to [`MOVED`] by its apt entry.
fn moved(apt: &AptRig, daemon: &Daemon) -> String {
    on_archives(apt, daemon);
    let pin = apt.publish(package(MOVED_PV));
    assert_eq!(
        daemon.order(&update_of(MOVED, MOVED_PV, &pin, 2)),
        succeeded()
    );
    pin
}

/// The labels of the fake's tool runs from call number `from` on.
fn labels_since(apt: &AptRig, from: usize) -> Vec<String> {
    apt.host.calls()[from..]
        .iter()
        .map(|call| call.label.clone())
        .collect()
}

/// Whether a label fetches: the index's update or a download.
fn fetches(label: &str) -> bool {
    (label == "apt-get update") | (label == "apt-get download")
}

/// Asserts the service is back on its archive version after a rollback of the move: the package
/// purged and its enable link gone, the archive's paths placed and confirmed, no installer
/// recorded, `current` the archive version.
fn back_on_archives(apt: &AptRig, daemon: &Daemon) {
    assert_eq!(apt.host.installed(SERVICE), None, "the package is purged");
    assert!(!apt.rig.there(ENABLED), "the package's enable link is gone");
    for path in archive_paths(apt) {
        assert!(apt.rig.there(&path), "{} is placed again", path.display());
    }
    assert_eq!(placed(apt), archive_placed(apt));
    assert_eq!(recorded(daemon), None, "it is a hostkeeper-archive service");
    assert_eq!(linked(apt, "current").as_deref(), Some(ARCHIVE));
}

/// Proves the move: a service that runs from archives, its unit, its unit's `WantedBy=` link and
/// its file placed, ordered an apt entry, ends `Succeeded`; the archive's paths are gone from the
/// host and from `placed.json`, the package is installed and held, its unit is the one enabled
/// (by the repository installer's link), the record names `apt`, `current` is the apt version
/// and `previous` the archive one, and the unit is stopped and started once, by `cutover`.
///
/// Catches: a move refused as `installer_changed`, an archive unit left in the unit directory
/// that would hide the package's unit of the same name, or a unit stopped before the package is
/// installed.
#[test]
fn a_service_on_archives_moves_to_apt_and_its_placed_paths_go() {
    let apt = AptRig::new("move-forward");
    let daemon = apt.daemon();
    on_archives(&apt, &daemon);
    let pin = apt.publish(package(MOVED_PV));
    apt.rig.placed_log.clear();
    let asked = apt.rig.store.asked().len();

    assert_eq!(
        daemon.order(&update_of(MOVED, MOVED_PV, &pin, 2)),
        succeeded()
    );

    for path in archive_paths(&apt) {
        assert!(!apt.rig.there(&path), "{} is taken away", path.display());
    }
    assert_eq!(placed(&apt), Vec::new());
    assert_eq!(apt.host.installed(SERVICE), held(MOVED_PV));
    assert!(
        apt.rig.host(ENABLED).is_symlink(),
        "the package's unit is enabled"
    );
    let record = recorded(&daemon).expect("an installed entry");
    assert_eq!(record.installer, "apt");
    assert_eq!(record.version.as_deref(), Some(MOVED_PV));
    assert!(record.links.contains(ENABLED));
    assert_eq!(linked(&apt, "current").as_deref(), Some(MOVED));
    assert_eq!(linked(&apt, "previous").as_deref(), Some(ARCHIVE));
    assert_eq!(
        unit_calls(&apt),
        [
            UnitCall::Stop(UNIT.to_owned()),
            UnitCall::Start(UNIT.to_owned())
        ],
        "only cutover restarts the unit"
    );
    assert_eq!(
        apt.rig.store.asked().len(),
        asked,
        "the archive port fetched nothing"
    );
}

/// An entry of `installer` for the service at `version`: a `macos-pkg` artifact entry with one
/// location, or the apt entry.
fn entry_of(installer: Installer, version: &str, pin: &str) -> Target {
    match installer {
        Installer::Apt => entry(&format!("{version}-1"), pin),
        _ => Target {
            installer: installer.as_str().to_owned(),
            digest: pin.to_owned(),
            bytes: 100,
            locations: vec![Location {
                store: "komira-stable".to_owned(),
                locator: format!("web/{version}/web-{version}.pkg"),
            }],
            manifest: Some(support_apt::manifest()),
            ..Target::default()
        },
    }
}

/// The release's archive entry for the service at `version`: a `hostkeeper-archive` entry,
/// whose manifest holds its floor alone (the rest is in the archive's own `manifest.toml`).
fn archive_entry(version: &str) -> Target {
    Target {
        installer: Installer::HostkeeperArchive.as_str().to_owned(),
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            ..Manifest::default()
        }),
        ..entry_of(Installer::MacosPkg, version, &d())
    }
}

/// A daemon on `apt`'s host that announces `apt` and `macos-pkg`.
fn both(apt: &AptRig) -> Daemon {
    let ports = hostkeeper_daemon::Ports {
        announcement: Announcement {
            installers: vec![
                Installer::HostkeeperArchive,
                Installer::Apt,
                Installer::MacosPkg,
            ],
            family: Some(Family::Deb),
        },
        ..apt.ports()
    };
    Daemon::new(config(), apt.rig.dirs(), ports, OWN)
}

/// Proves every other installer change stays `installer_changed` (`Failed`, release, nothing
/// fetched, nothing installed, `current` kept): an apt service ordered back to an archive
/// version that is not the `previous` the move kept, by an archive order and by its archive
/// entry; an apt service ordered a `macos-pkg` entry; and a service whose `current` was staged by
/// `macos-pkg` ordered an apt entry.
///
/// Catches: a move's exception that lets any archive version replace a native one, or that lets a
/// service change between two native installers.
#[test]
fn every_other_installer_change_stays_installer_changed() {
    let apt = AptRig::new("move-other-changes");
    let daemon = both(&apt);
    moved(&apt, &daemon);
    let later = apt.rig.stage(SERVICE, "3.0.0");
    assert!(later.is_dir());
    let calls = apt.host.calls().len();
    let asked = apt.rig.store.asked().len();
    let orders = [
        update(SERVICE, "3.0.0", &d(), 3),
        force_rollback(SERVICE, "0.5.0", &d(), 4),
        with_entry(
            force_rollback(SERVICE, "0.5.0", &d(), 5),
            &archive_entry("0.5.0"),
        ),
        with_entry(
            update(SERVICE, "3.0.0", &d(), 6),
            &entry_of(Installer::MacosPkg, "3.0.0", &d()),
        ),
    ];
    for order in orders {
        assert_eq!(
            daemon.order(&order),
            failed(Class::Release, "installer_changed"),
            "{order:?}"
        );
    }
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.rig.store.asked().len(), asked, "nothing was fetched");
    assert_eq!(apt.host.installed(SERVICE), held(MOVED_PV));
    assert_eq!(linked(&apt, "current").as_deref(), Some(MOVED));

    // A service whose `current` a `macos-pkg` staged, ordered an apt entry.
    let other = AptRig::new("move-pkg-to-apt");
    let daemon = both(&other);
    let dir = other.rig.packages.join(SERVICE).join(ARCHIVE);
    std::fs::create_dir_all(&dir).expect("its version directory");
    target::write(&dir, &entry_of(Installer::MacosPkg, ARCHIVE, &d())).expect("its target.json");
    other.rig.store.link(SERVICE, "current", ARCHIVE);
    let pin = other.publish(package(MOVED_PV));
    assert_eq!(
        daemon.order(&update_of(MOVED, MOVED_PV, &pin, 1)),
        failed(Class::Release, "installer_changed")
    );
    assert_eq!(other.host.calls().len(), 0, "no tool ran");
    assert_eq!(other.host.installed(SERVICE), None);
}

/// Proves the move's rollback before it settled: the move's `activate` fails in the package's
/// script, so the runner rolls back; the package is purged, the archive's paths are placed again
/// and confirmed, no installer is recorded, `current` is the archive version, `rollback`
/// restarts the archive's unit, and nothing is fetched after the move's stage: no archive from
/// the package port, no index update or download by apt.
///
/// Catches: a rollback of a move that looks for a `target.json` in the archive version, leaves
/// the package installed beside the archive, or fetches what is on the host.
#[test]
fn the_moves_rollback_before_it_settled_puts_the_archive_back_and_fetches_nothing() {
    let apt = AptRig::new("move-rollback-unsettled");
    let daemon = apt.daemon();
    on_archives(&apt, &daemon);
    let pin = apt.publish(package(MOVED_PV));
    apt.host
        .edit(|state| state.faults.script_fails_for = Some(MOVED_PV.to_owned()));
    let asked = apt.rig.store.asked().len();
    let calls = apt.host.calls().len();
    apt.rig.placed_log.clear();

    let ended = daemon.order(&update_of(MOVED, MOVED_PV, &pin, 2));

    assert!(
        matches!(
            &ended,
            hostkeeper_daemon::Reply::Finished {
                verdict: Verdict::Failed { class: Class::Release, .. },
                reason: Some(reason),
                ..
            } if reason == "dpkg_error"
        ),
        "{ended:?}"
    );
    back_on_archives(&apt, &daemon);
    assert_eq!(linked(&apt, "previous"), None);
    assert_eq!(
        apt.rig.store.asked().len(),
        asked,
        "the archive port fetched nothing"
    );
    let labels = labels_since(&apt, calls);
    let install = labels
        .iter()
        .position(|label| label == "apt-get install")
        .expect("the move's install");
    assert!(
        !labels[install..].iter().any(|label| fetches(label)),
        "the rollback fetched nothing: {labels:?}"
    );
    assert!(labels.contains(&"dpkg --purge".to_owned()), "{labels:?}");
    assert_eq!(
        unit_calls(&apt).last(),
        Some(&UnitCall::Start(UNIT.to_owned())),
        "rollback started the archive's unit"
    );
    assert!(apt.rig.units.active().contains(UNIT));
}

/// Proves the move's rollback after it settled: a `force_rollback` to the archive version kept as
/// `previous`, by an order with no entry and by one with the release's archive entry, ends
/// `Succeeded`; the package is purged and its hold gone with it, the archive's paths are placed
/// again, no installer is recorded, and apt fetched nothing.
///
/// Catches: a settled move that cannot be undone, or one undone by fetching the archive again.
#[test]
fn the_moves_rollback_after_it_settled_purges_the_package_and_places_the_archive() {
    for (n, with_archive_entry) in [false, true].into_iter().enumerate() {
        let apt = AptRig::new(&format!("move-rollback-settled-{n}"));
        let daemon = apt.daemon();
        moved(&apt, &daemon);
        let calls = apt.host.calls().len();
        let order = force_rollback(SERVICE, ARCHIVE, &d(), 3);
        let order = match with_archive_entry {
            true => with_entry(order, &archive_entry(ARCHIVE)),
            false => order,
        };

        assert_eq!(
            daemon.order(&order),
            succeeded(),
            "entry: {with_archive_entry}"
        );

        back_on_archives(&apt, &daemon);
        let labels = labels_since(&apt, calls);
        assert!(labels.contains(&"dpkg --purge".to_owned()), "{labels:?}");
        assert!(!labels.iter().any(|label| fetches(label)), "{labels:?}");
        assert!(
            apt.rig.launcher.steps(SERVICE).contains(&Step::Cutover),
            "the archive's own steps ran"
        );
    }
}

/// Proves the move cannot be undone once the archive version is no longer the one kept: after
/// the next apt update, a `force_rollback` to the archive version is `Failed`, release,
/// `installer_changed`, with nothing fetched and the package as it was.
///
/// Catches: a rollback to an archive version the host no longer keeps as `previous`, which would
/// fetch it again.
#[test]
fn after_the_next_update_the_move_cannot_be_undone() {
    let apt = AptRig::new("move-retired");
    let daemon = apt.daemon();
    moved(&apt, &daemon);
    let three = apt.publish(package("3.0.0-1"));
    assert_eq!(
        daemon.order(&update_of("3.0.0", "3.0.0-1", &three, 3)),
        succeeded()
    );
    assert_eq!(linked(&apt, "previous").as_deref(), Some(MOVED));
    let calls = apt.host.calls().len();
    let asked = apt.rig.store.asked().len();

    assert_eq!(
        daemon.order(&force_rollback(SERVICE, ARCHIVE, &d(), 4)),
        failed(Class::Release, "installer_changed")
    );
    assert_eq!(labels_since(&apt, calls), Vec::<String>::new());
    assert_eq!(apt.rig.store.asked().len(), asked);
    assert_eq!(apt.host.installed(SERVICE), held("3.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("3.0.0"));
}

/// Proves a `force_rollback` to the archive version whose `cutover` fails is rolled back to the
/// apt version it replaced: the archive's paths are taken away again, the package is installed
/// and held at its version again from its sealed copy (no index update, no download), its unit
/// is enabled, the record names `apt` again and `current` is the apt version.
///
/// Catches: a rollback of a move's rollback that leaves the archive's unit hiding the package's,
/// or one that fetches the apt version again.
#[test]
fn a_failed_rollback_to_the_archive_goes_back_to_the_package() {
    let apt = AptRig::new("move-rollback-fails");
    let daemon = apt.daemon();
    moved(&apt, &daemon);
    apt.rig.launcher.exits(SERVICE, Step::Cutover, &[1]);
    let calls = apt.host.calls().len();

    let ended = daemon.order(&force_rollback(SERVICE, ARCHIVE, &d(), 3));

    assert!(
        matches!(
            &ended,
            hostkeeper_daemon::Reply::Finished {
                verdict: Verdict::Failed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    for path in archive_paths(&apt) {
        assert!(
            !apt.rig.there(&path),
            "{} is taken away again",
            path.display()
        );
    }
    assert_eq!(placed(&apt), Vec::new());
    assert_eq!(apt.host.installed(SERVICE), held(MOVED_PV));
    assert!(apt.rig.host(ENABLED).is_symlink());
    let record = recorded(&daemon).expect("an installed entry");
    assert_eq!(record.installer, "apt");
    assert_eq!(record.version.as_deref(), Some(MOVED_PV));
    assert_eq!(linked(&apt, "current").as_deref(), Some(MOVED));
    let labels = labels_since(&apt, calls);
    assert!(labels.contains(&"apt-get install".to_owned()), "{labels:?}");
    assert!(!labels.iter().any(|label| fetches(label)), "{labels:?}");
}
