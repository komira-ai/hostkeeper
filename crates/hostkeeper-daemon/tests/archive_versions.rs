//! The archive version a native service left as `previous` (docs/design/packages-native.md,
//! section 10.7): at its start a daemon deletes the version directory, the seal and the link of
//! a `hostkeeper-archive` version that `previous` names for a service it runs natively, and
//! nothing else. Each case runs on the repository installer's fake of apt: the native versions
//! are installed by orders, the archive version is what an earlier daemon left.

mod support;
mod support_apt;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::archive_versions::{ArchiveVersion, sweep};
use hostkeeper_daemon::boot::{ALERT_START, alert_line};
use hostkeeper_daemon::locks::Holder;
use hostkeeper_daemon::prune::LayoutPrune;
use hostkeeper_daemon::start::startup;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::InstalledEntry;
use hostkeeper_steps::record::{PendingActivate, RECORD_FILE, State};
use hostkeeper_steps::{Seal, tree_digest};
use support::selfupdate::HK;
use support::{OWN, alerting, d, lines, name};
use support_apt::seen::{install_of, update_of};
use support_apt::{AptRig, SERVICE, package};
use support_native::hk_entry;

mod support_native;

/// The version of `web` the host runs.
const NATIVE: &str = "2.0.0";
/// The archive version an earlier daemon left as `previous`.
const ARCHIVE: &str = "1.0.0";

/// An archive version of `service` as `hostkeeper-archive` staged it: no `target.json`, its own
/// `manifest.toml` and program, sealed.
fn archive_version(apt: &AptRig, service: &str, version: &str) -> PathBuf {
    let dir = apt.rig.packages.join(service).join(version);
    fs::create_dir_all(dir.join("bin")).expect("the version");
    fs::write(dir.join("manifest.toml"), "schema = 1\n").expect("the manifest");
    fs::write(dir.join("bin/hook"), "#!hook\n").expect("the program");
    for dir in [dir.join("bin"), dir.clone()] {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    }
    let tree = tree_digest(&dir).expect("the tree digest");
    let seal = Seal::new(&tree, &d()).expect("a seal");
    fs::write(
        apt.rig
            .packages
            .join(service)
            .join(format!("{version}.seal")),
        seal.to_json(),
    )
    .expect("the seal");
    dir
}

/// Points `previous` of `service` at `version`.
fn previous(apt: &AptRig, service: &str, version: &str) {
    let link = apt.rig.packages.join(service).join("previous");
    drop(fs::remove_file(&link));
    symlink(version, link).expect("the link");
}

/// A host that runs `web` at [`NATIVE`], installed by an order through `apt`, with the archive
/// version [`ARCHIVE`] its `previous`, as the move left it.
fn moved(name: &str) -> AptRig {
    let apt = AptRig::new(name);
    let pin = apt.publish(package(NATIVE));
    let daemon = apt.daemon();
    let reply = daemon.order(&install_of(NATIVE, NATIVE, &pin, 1));
    assert_eq!(
        reply,
        support_apt::seen::succeeded(),
        "installed through apt"
    );
    archive_version(&apt, SERVICE, ARCHIVE);
    previous(&apt, SERVICE, ARCHIVE);
    apt
}

/// What `web`'s package directory holds, by name.
fn names(apt: &AptRig) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(apt.rig.packages.join(SERVICE))
        .expect("the package directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn swept(version: &str, deleted: Result<(), String>) -> ArchiveVersion {
    ArchiveVersion {
        service: SERVICE.to_owned(),
        version: version.to_owned(),
        deleted,
    }
}

/// Catches an archive `previous` that stays on the host, or a deletion that takes more than the
/// archive version: the link, the directory and the seal of the archive version are gone, the
/// native version, its seal and `current` are as they were, and the start's report says so.
#[test]
fn the_archive_previous_of_a_native_service_is_deleted_with_its_seal_and_link() {
    let apt = moved("archive-deleted");
    let before = names(&apt);
    assert!(before.contains(&format!("{ARCHIVE}.seal")), "{before:?}");
    let started = startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts");
    assert_eq!(
        started.boot_report().archive_versions,
        [swept(ARCHIVE, Ok(()))]
    );
    assert_eq!(
        names(&apt),
        ["2.0.0", "2.0.0.seal", "current"],
        "only the archive version went"
    );
    assert_eq!(
        support_apt::seen::linked(&apt, "current").as_deref(),
        Some(NATIVE)
    );
    assert_eq!(
        support_apt::seen::recorded(started.daemon())
            .expect("recorded")
            .installer,
        "apt"
    );
    assert_eq!(started.boot_report().ready, Some(Ok(())));
}

/// Catches a second start deleting again, or failing on what the first deleted: a repeat finds
/// nothing, and changes nothing.
#[test]
fn a_repeat_deletes_nothing() {
    let apt = moved("archive-repeat");
    let daemon = apt.daemon();
    assert_eq!(sweep(daemon.ctx()), [swept(ARCHIVE, Ok(()))]);
    let after = names(&apt);
    assert_eq!(sweep(daemon.ctx()), []);
    assert_eq!(names(&apt), after);
}

/// Catches a native `previous` deleted: two native versions leave `previous` where it is, with
/// its seal.
#[test]
fn a_native_previous_is_kept() {
    let apt = AptRig::new("archive-native-previous");
    let one = apt.publish(package("1.0.0"));
    let two = apt.publish(package(NATIVE));
    let daemon = apt.daemon();
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0", &one, 1)),
        support_apt::seen::succeeded()
    );
    assert_eq!(
        daemon.order(&update_of(NATIVE, NATIVE, &two, 2)),
        support_apt::seen::succeeded()
    );
    let before = names(&apt);
    assert_eq!(sweep(daemon.ctx()), []);
    assert_eq!(names(&apt), before);
    assert_eq!(
        support_apt::seen::linked(&apt, "previous").as_deref(),
        Some("1.0.0")
    );
}

/// Catches a service the host does not record as native losing a version: with no `installed`
/// entry of the service (an archive service, or one a record never named), nothing is deleted;
/// and an installer that is not a native one keeps its `previous` too.
#[test]
fn a_service_not_recorded_native_is_untouched() {
    let apt = moved("archive-not-native");
    let daemon = apt.daemon();
    let placer = daemon.ctx().placer();
    let lock = placer.lock().expect("the lock");
    let mut record = placer.store().load().expect("placed.json");
    let kept = record.installed.remove(SERVICE).expect("recorded");
    placer.store().save(&record, &lock).expect("saved");
    drop(lock);
    let before = names(&apt);
    assert_eq!(sweep(daemon.ctx()), []);
    assert_eq!(names(&apt), before);

    for installer in ["hostkeeper-archive", "tarball"] {
        let lock = placer.lock().expect("the lock");
        let mut record = placer.store().load().expect("placed.json");
        record
            .installed
            .insert(SERVICE.to_owned(), InstalledEntry::new(installer));
        placer.store().save(&record, &lock).expect("saved");
        drop(lock);
        assert_eq!(sweep(daemon.ctx()), [], "{installer}");
        assert_eq!(names(&apt), before, "{installer}");
    }
    drop(kept);
}

/// Catches an archive `previous` deleted while something still depends on it: an unfinished step
/// record, a record that cannot be read, an `activate` still pending, a tombstone, an install
/// still `pending`, an order holding the service's lock, and a `current` that is itself an archive
/// version each keep it, and once none of them holds it is deleted.
#[test]
fn an_unfinished_record_a_pending_activate_a_tombstone_or_a_held_lock_keeps_it() {
    let apt = moved("archive-kept");
    let daemon = apt.daemon();
    let before = names(&apt);
    let store = daemon.ctx().store(&name(SERVICE));
    let mut record = *match store.load().expect("the record") {
        hostkeeper_steps::record::Loaded::Current(record) => record,
        other => panic!("a record: {other:?}"),
    };
    let ended = record.clone();

    // An unfinished record.
    record.state = State::Pending;
    store.save(&record).expect("saved");
    assert_eq!(sweep(daemon.ctx()), []);
    // One that cannot be read.
    fs::write(store.dir().join(RECORD_FILE), "not a record").expect("a corrupt record");
    assert_eq!(sweep(daemon.ctx()), []);
    // An `activate` pending, the update ended.
    let mut pending = ended.clone();
    pending.progress.activate = Some(PendingActivate {
        version: NATIVE.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
    });
    store.save(&pending).expect("saved");
    assert_eq!(sweep(daemon.ctx()), []);
    store.save(&ended).expect("the ended record again");

    // A tombstone, then an install still pending.
    let placer = daemon.ctx().placer();
    let lock = placer.lock().expect("the lock");
    let mut placed = placer.store().load().expect("placed.json");
    placed
        .uninstalling
        .insert(SERVICE.to_owned(), hostkeeper_placed::Tombstone::default());
    placer.store().save(&placed, &lock).expect("saved");
    assert_eq!(sweep(daemon.ctx()), []);
    placed.uninstalling.clear();
    placed.installed.get_mut(SERVICE).expect("recorded").pending = true;
    placer.store().save(&placed, &lock).expect("saved");
    assert_eq!(sweep(daemon.ctx()), []);
    placed.installed.get_mut(SERVICE).expect("recorded").pending = false;
    placer.store().save(&placed, &lock).expect("saved");
    drop(lock);

    // An order holding the lock.
    let held = daemon
        .ctx()
        .locks
        .take_for(&name(SERVICE), Holder::Uninstall)
        .expect("the lock");
    assert_eq!(sweep(daemon.ctx()), []);
    drop(held);
    assert_eq!(names(&apt), before, "nothing was deleted");

    // Nothing holds it any more.
    assert_eq!(sweep(daemon.ctx()), [swept(ARCHIVE, Ok(()))]);
}

/// Catches a `current` that is an archive version swept as though the move were over: with
/// `current` and `previous` both archive versions nothing is deleted, and with `previous` naming
/// the version `current` names, or a version that is not there, nothing is either.
#[test]
fn a_current_without_target_json_or_a_previous_that_is_not_another_version_is_kept() {
    let apt = moved("archive-current-archive");
    let daemon = apt.daemon();
    // `current` is an archive version too.
    archive_version(&apt, SERVICE, "1.5.0");
    let link = apt.rig.packages.join(SERVICE).join("current");
    fs::remove_file(&link).expect("current");
    symlink("1.5.0", &link).expect("current");
    assert_eq!(sweep(daemon.ctx()), []);
    // `previous` names `current`.
    fs::remove_file(&link).expect("current");
    symlink(NATIVE, &link).expect("current");
    previous(&apt, SERVICE, NATIVE);
    assert_eq!(sweep(daemon.ctx()), []);
    // `previous` names a version that is not there.
    previous(&apt, SERVICE, "0.9.0");
    assert_eq!(sweep(daemon.ctx()), []);
    // A `previous` whose `target.json` cannot be read is not an archive version.
    previous(&apt, SERVICE, "1.5.0");
    fs::write(
        apt.rig.packages.join(SERVICE).join("1.5.0/target.json"),
        "{",
    )
    .expect("garbage");
    assert_eq!(sweep(daemon.ctx()), []);
    assert!(apt.rig.packages.join(SERVICE).join("1.5.0").is_dir());
}

/// Catches hostkeeper's own service treated unlike the others, or its archive version deleted
/// before its native one is the version that runs: with its native package `current` and its
/// record ended the archive `previous` goes; while the `activate` of the package waits, or while
/// `current` is still the archive version, it stays.
#[test]
fn hostkeepers_own_service_is_swept_by_the_same_rules() {
    let apt = AptRig::new("archive-hostkeeper");
    let rig = &apt.rig;
    let dir = rig
        .store
        .add_entry(HK, NATIVE, &hk_entry(NATIVE, &d()), false);
    drop(dir);
    rig.store.seal(HK, NATIVE, &d());
    rig.store.link(HK, "current", NATIVE);
    let daemon = apt.daemon();
    let placer = daemon.ctx().placer();
    let lock = placer.lock().expect("the lock");
    let mut placed = placer.store().load().expect("placed.json");
    placed
        .installed
        .insert(HK.to_owned(), InstalledEntry::new("apt"));
    placer.store().save(&placed, &lock).expect("saved");
    drop(lock);
    archive_version(&apt, HK, ARCHIVE);
    previous(&apt, HK, ARCHIVE);
    let ended = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    // The `activate` of its package waits: the archive version stays.
    let mut waiting = support::selfupdate::record_of(HK, ended.clone());
    waiting.progress.activate = Some(PendingActivate {
        version: NATIVE.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
    });
    support::selfupdate::put_record(rig, HK, &waiting);
    assert_eq!(sweep(daemon.ctx()), []);
    support::selfupdate::put_record(rig, HK, &support::selfupdate::record_of(HK, ended));
    // `current` is still the archive version: nothing to delete.
    let link = rig.packages.join(HK).join("current");
    fs::remove_file(&link).expect("current");
    symlink(ARCHIVE, &link).expect("current");
    previous(&apt, HK, NATIVE);
    assert_eq!(sweep(daemon.ctx()), []);
    // The package is the version that runs: the archive version goes.
    fs::remove_file(&link).expect("current");
    symlink(NATIVE, &link).expect("current");
    previous(&apt, HK, ARCHIVE);
    assert_eq!(
        sweep(daemon.ctx()),
        [ArchiveVersion {
            service: HK.to_owned(),
            version: ARCHIVE.to_owned(),
            deleted: Ok(()),
        }]
    );
}

/// Catches a failed deletion stopping the start, or passing in silence: a seal that cannot be
/// removed is one alert naming the service and the version and the result `Err`, the link is
/// already gone, and the start goes on to its readiness.
#[test]
fn a_delete_failure_is_an_alert_and_the_start_goes_on() {
    let apt = moved("archive-delete-fails");
    // A directory where the seal is: removing it as a file fails.
    let seal = apt
        .rig
        .packages
        .join(SERVICE)
        .join(format!("{ARCHIVE}.seal"));
    fs::remove_file(&seal).expect("the seal");
    fs::create_dir(&seal).expect("a directory in its place");
    let (daemon, log) = alerting(Daemon::new(
        support::config(),
        apt.rig.dirs(),
        apt.ports(),
        OWN,
    ));
    let swept_now = sweep(daemon.ctx());
    assert_eq!(swept_now.len(), 1, "{swept_now:?}");
    assert_eq!(
        (swept_now[0].service.as_str(), swept_now[0].version.as_str()),
        (SERVICE, ARCHIVE)
    );
    let why = swept_now[0]
        .deleted
        .clone()
        .expect_err("the deletion failed");
    assert_eq!(
        lines(&log),
        [alert_line(
            ALERT_START,
            &format!("service={SERVICE}: the archive version {ARCHIVE} was not deleted: {why}")
        )]
    );
    assert!(
        !apt.rig.packages.join(SERVICE).join("previous").exists(),
        "the link was removed first"
    );

    // The same, at the start: it goes on and says it is ready.
    let apt = moved("archive-delete-fails-start");
    let seal = apt
        .rig
        .packages
        .join(SERVICE)
        .join(format!("{ARCHIVE}.seal"));
    fs::remove_file(&seal).expect("the seal");
    fs::create_dir(&seal).expect("a directory in its place");
    let started = startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts");
    assert!(started.boot_report().archive_versions[0].deleted.is_err());
    assert_eq!(started.boot_report().ready, Some(Ok(())));

    // The unlink fails first when the link is not one: nothing else is touched.
    let apt = moved("archive-unlink-fails");
    let link = apt.rig.packages.join(SERVICE).join("previous");
    fs::remove_file(&link).expect("the link");
    symlink(ARCHIVE, &link).expect("the link");
    let service = apt.rig.packages.join(SERVICE);
    fs::set_permissions(&service, fs::Permissions::from_mode(0o555)).expect("read-only");
    let swept_now = sweep(apt.daemon().ctx());
    fs::set_permissions(&service, fs::Permissions::from_mode(0o755)).expect("writable");
    assert!(swept_now[0].deleted.is_err(), "{swept_now:?}");
    assert!(
        service.join(ARCHIVE).is_dir(),
        "the version was not touched"
    );
}

/// Catches a deletion a crash cut short that the next start does not finish: after the link is
/// gone and the tree renamed to its temporary name, the start's sweep of leftovers removes the
/// renamed tree and the seal, and nothing more is deleted; after the link is gone with the
/// version whole, the next confirmed switch's prune deletes it.
#[test]
fn a_deletion_a_crash_cut_short_is_finished_by_the_next_start() {
    let apt = moved("archive-crash");
    let service = apt.rig.packages.join(SERVICE);
    // The crash after the rename: the link gone, the tree under its temporary name, the seal.
    fs::remove_file(service.join("previous")).expect("the link");
    fs::rename(
        service.join(ARCHIVE),
        service.join(".stage-prune-0123456789abcdef"),
    )
    .expect("the renamed tree");
    let started = startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts");
    assert!(started.boot_report().archive_versions.is_empty());
    assert_eq!(names(&apt), ["2.0.0", "2.0.0.seal", "current"]);
    drop(started);

    // The crash before the rename: the link gone, the version whole, no start deletes it, and the
    // next confirmed switch does.
    let apt = moved("archive-crash-early");
    let service = apt.rig.packages.join(SERVICE);
    fs::remove_file(service.join("previous")).expect("the link");
    let started = startup(&apt.rig.settings(), apt.ports(), OWN).expect("the daemon starts");
    assert!(started.boot_report().archive_versions.is_empty());
    assert!(
        service.join(ARCHIVE).is_dir(),
        "a start does not look past `previous`"
    );
    drop(started);
    let three = apt.publish(package("3.0.0"));
    let ports = Ports {
        prune: Arc::new(LayoutPrune),
        ..apt.ports()
    };
    let daemon = Daemon::new(support::config(), apt.rig.dirs(), ports, OWN);
    assert_eq!(
        daemon.order(&update_of("3.0.0", "3.0.0", &three, 2)),
        support_apt::seen::succeeded()
    );
    assert!(!service.join(ARCHIVE).exists(), "the prune deleted it");
    assert!(!service.join(format!("{ARCHIVE}.seal")).exists());
}

/// Catches a sweep that goes on past what it cannot read: a `placed.json` that cannot be read
/// and a packages directory that cannot be listed are each one alert and nothing deleted.
#[test]
fn a_record_or_a_directory_that_cannot_be_read_is_an_alert_and_nothing_is_swept() {
    let apt = moved("archive-unreadable");
    let (daemon, log) = alerting(apt.daemon());
    fs::write(apt.rig.placed_record_path(), "not json").expect("a corrupt placed.json");
    assert_eq!(sweep(daemon.ctx()), []);
    let seen = lines(&log);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&alert_line(
            ALERT_START,
            "placed.json cannot be read, so no archive version is deleted: "
        )),
        "{seen:?}"
    );

    let apt = moved("archive-unlistable");
    let (daemon, log) = alerting(apt.daemon());
    support_apt::open_up(&apt.rig.packages);
    fs::remove_dir_all(&apt.rig.packages).expect("the packages");
    fs::write(&apt.rig.packages, "x").expect("a file where the packages go");
    assert_eq!(sweep(daemon.ctx()), []);
    let seen = lines(&log);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&alert_line(
            ALERT_START,
            "the package directory cannot be listed, so no archive version is deleted: "
        )),
        "{seen:?}"
    );
}
