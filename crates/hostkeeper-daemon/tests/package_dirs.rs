//! The start's setting of the package directories' modes to the store's layout
//! ([`hostkeeper_daemon::package_dirs`]): the packages directory and each service directory 0711
//! (passed through by every user, listed by root alone), each version an installer published 0700
//! (root's alone), each version an earlier release unpacked from hostkeeper's own archive 0555
//! (the tree its service runs from, as its own user), and the apt work and mirror parents 0700;
//! what it set is said in one log line, and what is below a version (its tree digest) is left as
//! it was. The run units the archive enabled of a service whose archive version the start opened
//! are started again. A published version given its mode so is rolled back to as before, on Linux
//! (apt) and on a Mac (macos-pkg).

mod support;
mod support_apt;
mod support_macos_pkg;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_daemon::boot::{ALERT_START, alert_line};
use hostkeeper_daemon::package_dirs::{PackageDirs, set, set_dirs};
use hostkeeper_daemon::start::startup;
use hostkeeper_placed::testing::{Event, UnitCall};
use hostkeeper_steps::tree_digest;
use serde_json::{Value, json};
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

/// `path` with `bytes`, mode `mode`.
fn put(path: &Path, bytes: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    set_mode(path, mode);
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

/// The version `version` of `service` as an installer publishes it (a `target.json`, the
/// `artifact` and a `tree/` with a file), its seal 0444 and `current` naming it; its directory
/// mode `top`. Answers the version directory.
fn published(packages: &Path, service: &str, version: &str, top: u32) -> PathBuf {
    let dir = packages.join(service).join(version);
    put(&dir.join("target.json"), b"{}", 0o400);
    put(&dir.join("artifact"), b"the package's bytes", 0o400);
    put(&dir.join("tree/usr/bin/tool"), b"#!/bin/sh\n", 0o755);
    for sub in ["tree", "tree/usr", "tree/usr/bin"] {
        set_mode(&dir.join(sub), 0o755);
    }
    sealed(&dir, top)
}

/// The version `version` of `service` as hostkeeper's archive unpacked it: its `manifest.toml`,
/// `bin/` with the program and `units/` with its unit, every directory and the program 0555 and
/// every other file 0444; its seal 0444 and `current` naming it; its directory mode `top`.
/// Answers the version directory.
fn archive(packages: &Path, service: &str, version: &str, top: u32) -> PathBuf {
    let dir = packages.join(service).join(version);
    put(&dir.join("manifest.toml"), b"schema = 1\n", 0o444);
    put(&dir.join("bin").join(service), b"#!/bin/sh\n", 0o555);
    put(&dir.join("units/unit.service"), b"[Service]\n", 0o444);
    for sub in ["bin", "units"] {
        set_mode(&dir.join(sub), 0o555);
    }
    sealed(&dir, top)
}

/// Writes the seal of the version `dir` (0444) and `current` naming it, then sets the version
/// directory to `top`.
fn sealed(dir: &Path, top: u32) -> PathBuf {
    let service = dir.parent().expect("the service directory");
    let version = dir.file_name().expect("a version").to_string_lossy();
    put(&service.join(format!("{version}.seal")), b"{}", 0o444);
    if fs::symlink_metadata(service.join("current")).is_err() {
        symlink(&*version, service.join("current")).expect("current");
    }
    set_mode(dir, top);
    dir.to_path_buf()
}

/// Lays out what an earlier release left on a host: the service `web` with a published version
/// `1.0.0` and a publish under way (`.stage-left`), the service `hello` with an archive version
/// `0.1.92`, a file beside the services, and the apt work and mirror parents; every directory of
/// the store `dirs`, each published version and the stage `version`, each archive version
/// `archived`. Answers the versions `[web, hello]`.
fn left(rig: &Rig, dirs: u32, version: u32, archived: u32) -> [PathBuf; 2] {
    let web = published(&rig.packages, "web", "1.0.0", version);
    let stage = rig.packages.join("web/.stage-left");
    fs::create_dir_all(&stage).expect("a stage");
    set_mode(&stage, version);
    let hello = archive(&rig.packages, "hello", "0.1.92", archived);
    fs::write(rig.packages.join("not-a-service"), b"x").expect("a file");
    for dir in beside(rig) {
        fs::create_dir_all(dir.join("web")).expect("a directory beside");
        set_mode(&dir.join("web"), 0o755);
        set_mode(&dir, dirs);
    }
    for service in ["web", "hello"] {
        set_mode(&rig.packages.join(service), dirs);
    }
    set_mode(&rig.packages, dirs);
    [web, hello]
}

/// What the release before the closing left: everything open (0755, versions 0555).
fn left_open(rig: &Rig) -> [PathBuf; 2] {
    left(rig, 0o755, 0o555, 0o555)
}

/// What the release that closed the store to root left: every directory 0700, the archive
/// version too, as on the hosts whose service run as its own user could not start again.
fn left_closed(rig: &Rig) -> [PathBuf; 2] {
    left(rig, 0o700, 0o700, 0o700)
}

/// Makes every directory below its path writable by its owner again when dropped, so that the
/// scratch directory can be removed whether the test passed or failed: an archive version is
/// 0555 after the start, and its entries cannot be unlinked by a user that is not root.
struct Removable(PathBuf);

impl Drop for Removable {
    fn drop(&mut self) {
        fn open(dir: &Path) {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                if fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
                    open(&entry.path());
                }
            }
        }
        open(&self.0);
    }
}

/// [`Rig::new`], its packages directory made removable when the guard drops.
fn rig(name: &str) -> (Rig, Removable) {
    let rig = Rig::new(name);
    let removable = Removable(rig.packages.clone());
    (rig, removable)
}

/// Catches a start that leaves an earlier release's open package directories readable or
/// listable by other users, closes an archive version its service runs from, or changes what a
/// seal holds: after the start, the packages and service directories are 0711, the published
/// version and the publish under way 0700, the archive version is left 0555, and the apt work
/// and mirror parents are 0700, each listed in the report with its mode, outermost first; no
/// archive version is said opened; what is below each version keeps its modes, so the tree
/// digests are the same; `current`, the seals, a file beside the services and a directory below
/// a closed parent are left as they are; and a second start sets nothing.
#[test]
fn the_start_gives_the_directories_an_earlier_release_left_open_the_layouts_modes() {
    let (rig, _removable) = rig("modes-open");
    let [web, hello] = left_open(&rig);
    let digests = [&web, &hello].map(|v| tree_digest(v).expect("the digest"));
    let [work, mirror] = beside(&rig);

    let started = startup(&rig.settings(), rig.ports(), support::OWN).expect("the daemon starts");

    let p = &rig.packages;
    let want = PackageDirs {
        set: vec![
            (p.clone(), 0o711),
            (p.join("hello"), 0o711),
            (p.join("web"), 0o711),
            (p.join("web/.stage-left"), 0o700),
            (web.clone(), 0o700),
            (work.clone(), 0o700),
            (mirror.clone(), 0o700),
        ],
        opened: vec![],
    };
    assert_eq!(started.boot_report().package_dirs, want);
    assert_eq!(mode(&hello), 0o555);
    assert_eq!(
        started.boot_report().archive_units_started,
        Vec::<String>::new()
    );
    for (version, digest) in [&web, &hello].into_iter().zip(&digests) {
        assert_eq!(&tree_digest(version).expect("the digest"), digest);
    }
    assert_eq!(mode(&web.join("artifact")), 0o400);
    assert_eq!(mode(&hello.join("bin/hello")), 0o555);
    assert_eq!(mode(&p.join("web/1.0.0.seal")), 0o444);
    assert_eq!(
        fs::read_link(p.join("web/current")).expect("current"),
        Path::new("1.0.0")
    );
    assert_eq!(mode(&work.join("web")), 0o755, "below the closed parent");
    drop(started);

    let again = startup(&rig.settings(), rig.ports(), support::OWN).expect("it starts again");
    assert_eq!(again.boot_report().package_dirs, PackageDirs::default());
}

/// Catches a start that keeps an archive version closed to root, as a release before it left it,
/// so that the service run from it as its own user cannot start (`status=203/EXEC`): after the
/// start, the packages and service directories are 0711 and the archive version 0555, the
/// report says so and names its service opened; the published version, the publish under way and
/// the apt work and mirror parents stay 0700 (none is listed); the digests are the same;
/// and a second start sets nothing and opens nothing.
#[test]
fn the_start_opens_again_an_archive_version_a_release_closed_to_root() {
    let (rig, _removable) = rig("modes-closed");
    let [web, hello] = left_closed(&rig);
    let digests = [&web, &hello].map(|v| tree_digest(v).expect("the digest"));

    let started = startup(&rig.settings(), rig.ports(), support::OWN).expect("the daemon starts");

    let p = &rig.packages;
    let want = PackageDirs {
        set: vec![
            (p.clone(), 0o711),
            (p.join("hello"), 0o711),
            (hello.clone(), 0o555),
            (p.join("web"), 0o711),
        ],
        opened: vec!["hello".to_owned()],
    };
    assert_eq!(started.boot_report().package_dirs, want);
    assert_eq!(mode(&web), 0o700);
    for (version, digest) in [&web, &hello].into_iter().zip(&digests) {
        assert_eq!(&tree_digest(version).expect("the digest"), digest);
    }
    drop(started);

    let again = startup(&rig.settings(), rig.ports(), support::OWN).expect("it starts again");
    assert_eq!(again.boot_report().package_dirs, PackageDirs::default());
}

/// Catches a version taken for an archive version, and opened, on anything but the archive's
/// own shape (a regular `manifest.toml` and no `target.json`): a version with both, one whose
/// `manifest.toml` is a directory, one whose `manifest.toml` is a link to a file, and an empty
/// one are each made 0700 from 0555; a link and a file in the service directory are left as
/// they are; and nothing is said opened.
#[test]
fn only_the_archives_own_shape_is_an_archive_version() {
    let (rig, _removable) = rig("modes-kinds");
    let odd = rig.packages.join("odd");
    let both = archive(&rig.packages, "odd", "1.0.0", 0o755);
    put(&both.join("target.json"), b"{}", 0o400);
    fs::create_dir_all(odd.join("2.0.0/manifest.toml")).expect("a directory");
    put(&odd.join("3.0.0/real.toml"), b"schema = 1\n", 0o444);
    symlink("real.toml", odd.join("3.0.0/manifest.toml")).expect("a link");
    fs::create_dir_all(odd.join("4.0.0")).expect("an empty version");
    for version in ["1.0.0", "2.0.0", "3.0.0", "4.0.0"] {
        set_mode(&odd.join(version), 0o555);
    }
    set_mode(&odd, 0o711);
    set_mode(&rig.packages, 0o711);

    let done = set_dirs(&rig.packages, &[]);

    let set: Vec<(PathBuf, u32)> = ["1.0.0", "2.0.0", "3.0.0", "4.0.0"]
        .map(|v| (odd.join(v), 0o700))
        .to_vec();
    assert_eq!(done.set, set);
    assert_eq!(done.opened, Vec::<String>::new());
    assert!(done.failed.is_empty(), "{:?}", done.failed);
    assert_eq!(
        fs::read_link(odd.join("current")).expect("current"),
        Path::new("1.0.0")
    );
    assert_eq!(mode(&odd.join("1.0.0.seal")), 0o444);
}

/// Catches the setting passing unsaid, saying something else, or saying it twice: the log line
/// names every directory it set with its mode, in order, and a second setting logs nothing.
#[test]
fn the_start_says_what_it_set_in_one_line_and_a_repeat_says_nothing() {
    let (rig, _removable) = rig("modes-log");
    let [_, hello] = left_closed(&rig);
    let (daemon, log) = alerting(rig.daemon());

    let done = set(daemon.ctx());

    let p = &rig.packages;
    let named = [
        (p.clone(), "0711"),
        (p.join("hello"), "0711"),
        (hello.clone(), "0555"),
        (p.join("web"), "0711"),
    ];
    assert_eq!(done.opened, ["hello"]);
    let paths: Vec<String> = named
        .iter()
        .map(|(path, mode)| format!("{}={mode}", path.display()))
        .collect();
    assert_eq!(
        lines(&log),
        [format!(
            "hostkeeper daemon: level=info set the package directories an earlier release left \
             otherwise to the modes of the store's layout: {}",
            paths.join(" ")
        )]
    );

    assert_eq!(set(daemon.ctx()), PackageDirs::default());
    assert_eq!(lines(&log).len(), 1);
}

/// Catches a directory that cannot be given its mode passing unsaid or stopping the start's
/// setting: a version whose top cannot be looked at (mode 0600: listed, not searched) is named in
/// the one alert `start_incomplete`, with why, and left as it is, and the setting goes on to the
/// rest; a packages directory below a file is a failure too, and nothing more. A host with no
/// packages directory sets nothing and says nothing.
#[test]
fn a_package_directory_that_cannot_be_looked_into_is_an_alert_and_the_start_goes_on() {
    let (rig, _removable) = rig("modes-alert");
    let [web, _] = left_open(&rig);
    let unsearchable = rig.packages.join("db/1.0.0");
    put(&unsearchable.join("target.json"), b"{}", 0o400);
    set_mode(&unsearchable, 0o600);
    let (daemon, log) = alerting(rig.daemon());

    let done = set(daemon.ctx());

    set_mode(&unsearchable, 0o700);
    assert!(done.set.contains(&(web.clone(), 0o700)), "{:?}", done.set);
    assert!(!done.set.iter().any(|(path, _)| *path == unsearchable));
    let denied = "Permission denied (os error 13)";
    assert_eq!(
        lines(&log)[1..],
        [alert_line(
            ALERT_START,
            &format!(
                "a package directory was not given the mode of the store's layout: {}: {denied}",
                unsearchable.display(),
            )
        )]
    );

    let below_a_file = rig.packages.join("not-a-service/packages");
    let failed = set_dirs(&below_a_file, &[]);
    let failed: Vec<&Path> = failed.failed.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(failed, [below_a_file.as_path()]);

    let empty = Rig::new("modes-nothing");
    assert!(!empty.packages.exists(), "no packages directory");
    let (daemon, log) = alerting(empty.daemon());
    assert_eq!(set(daemon.ctx()), PackageDirs::default());
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
/// at 2.0.0 with 1.0.0 kept, its directories opened as an earlier release left them, is given the
/// layout's modes by the start (the kept version 0700), and a force to 1.0.0 then installs it
/// from its sealed copy, with nothing fetched, and the directories keep the layout's modes.
#[test]
fn an_apt_version_closed_at_the_start_is_rolled_back_to_from_its_sealed_copy() {
    let apt = AptRig::new("modes-apt-rollback");
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
    let set = &started.boot_report().package_dirs.set;
    assert!(set.contains(&(kept.clone(), 0o700)), "{set:?}");
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
    for (dir, want) in [
        (apt.rig.packages.clone(), 0o711),
        (apt.rig.packages.join(SERVICE), 0o711),
        (kept, 0o700),
    ] {
        assert_eq!(mode(&dir), want, "{}", dir.display());
    }
}

/// Catches a `.pkg` version closed by the start that can no longer be rolled back to: a Mac
/// service at 1.1.0 with 1.0.0 kept, its directories opened as an earlier release left them, is
/// given the layout's modes by the start (the kept version 0700), and a force to 1.0.0 then
/// installs the kept `.pkg` (its receipt names 1.0.0), with nothing fetched.
#[test]
fn a_pkg_version_closed_at_the_start_is_rolled_back_to_from_its_kept_pkg() {
    use support_macos_pkg::{IDENTIFIER, MacRig, force_of};
    let mac = MacRig::new("modes-pkg-rollback");
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
    let set = &started.boot_report().package_dirs.set;
    assert!(set.contains(&(mac.version_dir("1.0.0"), 0o700)), "{set:?}");
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
    assert_eq!(mode(&mac.rig.packages.join(service)), 0o711);
}

/// The host path of `service`'s archive unit `unit` in the rig's unit directory, and of its
/// `WantedBy=` link.
fn unit_paths(unit: &str) -> (String, String) {
    (
        format!("/units/{unit}"),
        format!("/units/multi-user.target.wants/{unit}"),
    )
}

/// One entry of the `paths` of `placed.json`, as the release before the native installers wrote
/// it.
fn path_entry(service: &str, kind: &str, link_target: &str) -> Value {
    json!({"service": service, "version": "0.1.92", "kind": kind, "state": "placed", "link_target": link_target})
}

/// Records in `placed.json`, and puts on the rig's host, what an archive version of `service`
/// placed for each of `enabled` (its unit, a link into `current/units/`, and its `WantedBy=`
/// link); for each of `listed`, the record's entries only. Answers the record's `paths`.
fn place_archive_units(rig: &Rig, service: &str, enabled: &[&str], listed: &[&str]) -> Value {
    let mut paths = serde_json::Map::new();
    for unit in enabled.iter().chain(listed) {
        let (at, wants) = unit_paths(unit);
        let target = format!("/packages/{service}/current/units/{unit}");
        paths.insert(at.clone(), path_entry(service, "unit", &target));
        paths.insert(wants.clone(), path_entry(service, "wants_link", &at));
        if enabled.contains(unit) {
            for (link, to) in [(&at, &target), (&wants, &at)] {
                let disk = rig.host(link);
                fs::create_dir_all(disk.parent().expect("a parent")).expect("the directory");
                symlink(to, disk).expect("the link");
            }
        }
    }
    Value::Object(paths)
}

/// Writes `paths` (merged from each service's [`place_archive_units`]) into `placed.json`.
fn seed_paths(rig: &Rig, paths: &[Value]) {
    let path = rig.placed_record_path();
    let mut all = serde_json::Map::new();
    for one in paths {
        all.extend(one.as_object().expect("an object").clone());
    }
    let record = json!({"schema": 1, "uninstalling": {}, "paths": all});
    fs::create_dir_all(path.parent().expect("a parent")).expect("the state dir");
    fs::write(&path, serde_json::to_vec_pretty(&record).expect("json")).expect("placed.json");
}

/// The starts of units the rig's service manager was asked for, in order.
fn starts(rig: &Rig) -> Vec<String> {
    rig.placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Unit(UnitCall::Start(unit)) => Some(unit),
            _ => None,
        })
        .collect()
}

/// Catches a host whose archive service, run as its own user, failed to start while its version
/// was closed (`status=203/EXEC`; systemd gave up) and stays failed after the start opened the
/// version: the start starts each unit the archive enabled that is enabled still (`hello@a`,
/// `hello@b`), and no unit whose `WantedBy=` link is gone (`hello@c`) or is not the archive's
/// (`hello@d`, an operator's link elsewhere), none of hostkeeper's own archive version (its unit
/// runs this daemon), none of an archive version that was not closed (`tierstore`, already
/// 0555: nothing failed), and none of a service whose `current` names a published version and
/// whose archive version, opened, is only kept (`moved`, which the move to its native package left
/// as it was); the report lists the units started; and a second start, which opens nothing,
/// starts nothing.
#[test]
fn the_start_starts_again_the_enabled_units_of_an_archive_version_it_opened() {
    let (rig, _removable) = rig("modes-units");
    left_closed(&rig);
    archive(&rig.packages, "hostkeeper", "0.1.200", 0o700);
    archive(&rig.packages, "tierstore", "0.2.0", 0o555);
    published(&rig.packages, "moved", "2.0.0", 0o700);
    let kept = archive(&rig.packages, "moved", "1.0.0", 0o700);
    for service in ["hostkeeper", "tierstore", "moved"] {
        set_mode(&rig.packages.join(service), 0o700);
    }
    let hello = place_archive_units(
        &rig,
        "hello",
        &["hello@a.service", "hello@b.service"],
        &["hello@c.service", "hello@d.service"],
    );
    let elsewhere = rig.host("/units/multi-user.target.wants/hello@d.service");
    symlink("/elsewhere/hello@d.service", elsewhere).expect("an operator's link");
    let own = place_archive_units(&rig, "hostkeeper", &["hostkeeper.service"], &[]);
    let other = place_archive_units(&rig, "tierstore", &["tierstore@a.service"], &[]);
    let moved = place_archive_units(&rig, "moved", &["moved.service"], &[]);
    seed_paths(&rig, &[hello, own, other, moved]);

    let started = startup(&rig.settings(), rig.ports(), support::OWN).expect("the daemon starts");

    let opened = &started.boot_report().package_dirs.opened;
    assert_eq!(opened, &["hello", "hostkeeper"]);
    assert_eq!(mode(&kept), 0o555, "the kept archive version is opened too");
    let enabled = ["hello@a.service", "hello@b.service"].map(str::to_owned);
    assert_eq!(started.boot_report().archive_units_started, enabled);
    assert_eq!(starts(&rig), enabled);
    drop(started);

    rig.placed_log.clear();
    let again = startup(&rig.settings(), rig.ports(), support::OWN).expect("it starts again");
    assert_eq!(
        again.boot_report().archive_units_started,
        Vec::<String>::new()
    );
    assert_eq!(starts(&rig), Vec::<String>::new());
}

/// Catches an archive unit that cannot be started stopping the start, or passing unsaid, and an
/// unreadable `placed.json` starting what it cannot know is enabled: a start that fails for
/// `hello@a` is an alert naming the service and the unit, and `hello@b` is started after it; with
/// `placed.json` not JSON, the alert names the service and nothing of it is started.
#[test]
fn an_archive_unit_that_cannot_be_started_is_an_alert_and_the_start_goes_on() {
    let (rig, _removable) = rig("modes-units-fail");
    left_closed(&rig);
    let hello = place_archive_units(&rig, "hello", &["hello@a.service", "hello@b.service"], &[]);
    seed_paths(&rig, &[hello]);
    rig.units.fail_start("hello@a.service");
    let (daemon, log) = alerting(rig.daemon());
    let mut report = hostkeeper_daemon::boot::BootReport {
        package_dirs: set(daemon.ctx()),
        ..Default::default()
    };

    daemon.start_run_units(&mut report);

    assert_eq!(report.archive_units_started, ["hello@b.service"]);
    let said = lines(&log).join("\n");
    assert!(
        said.contains("service=hello unit=hello@a.service: it could not be started"),
        "{said}"
    );

    fs::write(rig.placed_record_path(), b"not json").expect("a broken placed.json");
    rig.placed_log.clear();
    let mut report = hostkeeper_daemon::boot::BootReport {
        package_dirs: PackageDirs {
            set: vec![],
            opened: vec!["hello".to_owned()],
        },
        ..Default::default()
    };
    daemon.start_run_units(&mut report);
    assert_eq!(starts(&rig), Vec::<String>::new());
    let said = lines(&log).join("\n");
    assert!(
        said.contains(
            "service=hello: the units its archive version enabled cannot be read, so none is \
             started"
        ),
        "{said}"
    );
}
