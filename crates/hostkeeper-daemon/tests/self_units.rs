//! The fallback files of the releases before the hand-over, which the daemon removes at its start
//! (`hostkeeper_daemon::selfunits`): on Linux the drop-in of its own unit and the fallback unit, on
//! macOS the fallback timer job; each removed where it is found, followed by one word to the
//! service manager, and nothing at all on a start that finds none; and the daemon's own update
//! held off while one could not be removed.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::Stage;
use hostkeeper_daemon::selfunits::{
    DROPIN_DIR, DROPIN_FILE, FALLBACK_UNIT, Removed, SelfUnitsError, fallback_label,
};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, UnitCall};
use hostkeeper_steps::Layout;
use hostkeeper_steps::selfupdate::{
    START_LIMIT_BURST, fallback_unit, launchd_fallback_job, main_dropin,
};
use support::{OWN, Rig};

/// The rig's unit directory, as its host names it.
fn units() -> PathBuf {
    PathBuf::from("/units")
}

fn dropin_dir() -> PathBuf {
    units().join(DROPIN_DIR)
}

fn dropin() -> PathBuf {
    dropin_dir().join(DROPIN_FILE)
}

fn fallback_unit_path() -> PathBuf {
    units().join(FALLBACK_UNIT)
}

fn job() -> PathBuf {
    units().join(format!("{}.plist", fallback_label()))
}

/// Writes `text` at the rig's host path `path`, its directories made.
fn put(rig: &Rig, path: &Path, text: &str) {
    let at = rig.host(path);
    fs::create_dir_all(at.parent().expect("a directory")).expect("the directory");
    fs::write(at, text).expect("the file");
}

/// What a release before the hand-over left on a Linux host: the drop-in and the fallback unit,
/// with the texts it wrote.
fn armed_linux(rig: &Rig) {
    let layout = Layout::new(&binary_dir(&rig.state));
    put(
        rig,
        &dropin(),
        &main_dropin(FALLBACK_UNIT, START_LIMIT_BURST),
    );
    put(
        rig,
        &fallback_unit_path(),
        &fallback_unit(&layout, "hostkeeper.service"),
    );
}

/// What a release before the hand-over left on a Mac: the fallback job's plist.
fn armed_macos(rig: &Rig) {
    let layout = Layout::new(&binary_dir(&rig.state));
    put(
        rig,
        &job(),
        &launchd_fallback_job(&layout, "dev.hostkeeper.hostkeeper", 60),
    );
}

/// The events of removing the file at `path` durably: the removal and the sync of its directory.
fn gone(path: &Path) -> Vec<Event> {
    vec![
        Event::Fs(FsWrite::Remove(path.to_path_buf())),
        Event::Fs(FsWrite::SyncDir(
            path.parent().expect("a directory").to_path_buf(),
        )),
    ]
}

/// What removing the units of a Linux host that has all of them writes, in order: the drop-in, its
/// emptied directory, one `daemon-reload`, then the fallback unit.
fn linux_removal() -> Vec<Event> {
    let mut wanted = gone(&dropin());
    wanted.push(Event::Fs(FsWrite::RemoveDir(dropin_dir())));
    wanted.push(Event::Fs(FsWrite::SyncDir(units())));
    wanted.push(Event::Unit(UnitCall::DaemonReload));
    wanted.extend(gone(&fallback_unit_path()));
    wanted
}

/// The design's named test for the daemon side of the fallback files. Catches a start that leaves
/// the fallback unit or its drop-in standing beside a hand-over, one that removes the unit before
/// the drop-in whose `OnFailure=` names it, one that removes the unit before systemd is told (so a
/// failed reload would leave nothing for the next start to retry), one that does not tell systemd,
/// or one that tells it more than once: on Linux the drop-in goes first, then its emptied
/// directory, each durably, then one `daemon-reload`, then the fallback unit, durably; the second
/// start finds nothing and does nothing.
#[test]
fn on_linux_the_dropin_goes_then_systemd_reloads_once_then_the_fallback_unit_goes() {
    let rig = Rig::new("self-units-linux");
    armed_linux(&rig);
    let daemon = rig.daemon();
    let first = daemon.remove_self_units(PlatformKind::Linux);
    assert_eq!(
        first.expect("removed"),
        Removed::Gone(vec![dropin(), dropin_dir(), fallback_unit_path()])
    );
    assert_eq!(rig.placed_log.events(), linux_removal());
    assert!(!rig.there(dropin()) & !rig.there(dropin_dir()) & !rig.there(fallback_unit_path()));

    rig.placed_log.clear();
    let again = daemon.remove_self_units(PlatformKind::Linux);
    assert_eq!(again.expect("removed"), Removed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a start that removes what is not its own, or leaves its own when the other is missing:
/// a drop-in directory that holds another drop-in stays, with only `self-update.conf` removed; a
/// host with the fallback unit and no drop-in, or the drop-in and no fallback unit, has the one it
/// has removed and the manager told.
#[test]
fn only_the_files_that_are_there_are_removed_and_a_directory_with_another_dropin_stays() {
    let rig = Rig::new("self-units-partial");
    armed_linux(&rig);
    let other = dropin_dir().join("other.conf");
    put(&rig, &other, "[Service]\nNice=1\n");
    let removed = rig.daemon().remove_self_units(PlatformKind::Linux);
    assert_eq!(
        removed.expect("removed"),
        Removed::Gone(vec![dropin(), fallback_unit_path()])
    );
    assert!(rig.there(&other) & !rig.there(dropin()) & !rig.there(fallback_unit_path()));
    assert!(
        !rig.placed_log
            .events()
            .contains(&Event::Fs(FsWrite::RemoveDir(dropin_dir())))
    );

    let rig = Rig::new("self-units-no-dropin");
    armed_linux(&rig);
    fs::remove_file(rig.host(dropin())).expect("no drop-in");
    fs::remove_dir(rig.host(dropin_dir())).expect("no directory");
    let removed = rig.daemon().remove_self_units(PlatformKind::Linux);
    assert_eq!(
        removed.expect("removed"),
        Removed::Gone(vec![fallback_unit_path()])
    );
    let mut wanted = vec![Event::Unit(UnitCall::DaemonReload)];
    wanted.extend(gone(&fallback_unit_path()));
    assert_eq!(rig.placed_log.events(), wanted);

    let rig = Rig::new("self-units-no-fallback-unit");
    armed_linux(&rig);
    fs::remove_file(rig.host(fallback_unit_path())).expect("no fallback unit");
    let removed = rig.daemon().remove_self_units(PlatformKind::Linux);
    assert_eq!(
        removed.expect("removed"),
        Removed::Gone(vec![dropin(), dropin_dir()])
    );
    assert_eq!(
        rig.placed_log.events().last(),
        Some(&Event::Unit(UnitCall::DaemonReload))
    );
}

/// Catches a Mac left with a fallback job that fires beside a hand-over, or a plist removed under a
/// job launchd still holds: the job is booted out first, then its plist removed durably; no systemd
/// file is touched; a second start does nothing.
#[test]
fn on_macos_the_fallback_job_is_booted_out_and_its_plist_removed() {
    let rig = Rig::new("self-units-macos");
    armed_macos(&rig);
    let daemon = rig.daemon();
    let label = fallback_label();
    assert_eq!(label, "dev.hostkeeper.hostkeeper.fallback");
    let first = daemon.remove_self_units(PlatformKind::Macos);
    assert_eq!(first.expect("removed"), Removed::Gone(vec![job()]));
    let mut wanted = vec![Event::Unit(UnitCall::Stop(label))];
    wanted.extend(gone(&job()));
    assert_eq!(rig.placed_log.events(), wanted);
    assert!(!rig.there(job()));

    rig.placed_log.clear();
    let again = daemon.remove_self_units(PlatformKind::Macos);
    assert_eq!(again.expect("removed"), Removed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a Mac whose plist is removed while its job still runs, or a failure hidden: a bootout
/// that fails is the error naming no file removed, and the plist stays for the next start.
#[test]
fn on_macos_a_bootout_that_fails_is_the_error_and_the_plist_stays() {
    let rig = Rig::new("self-units-macos-stop-fails");
    armed_macos(&rig);
    rig.units.fail_stop(&fallback_label());
    let error = rig
        .daemon()
        .remove_self_units(PlatformKind::Macos)
        .expect_err("the stop fails");
    assert!(
        matches!(&error, SelfUnitsError::NotTold { removed, .. } if removed.is_empty()),
        "{error:?}"
    );
    assert!(rig.there(job()));
    assert!(
        !rig.placed_log
            .events()
            .contains(&Event::Fs(FsWrite::Remove(rig.host(job()))))
    );
}

/// Catches a failed reload being hidden, or lost: the error names the files removed (the drop-in
/// and its directory) and the refusal; those stay removed; the fallback unit is still there, so
/// the next start reloads before it removes it; and the start after that, finding none, removes
/// nothing and does not reload.
#[test]
fn a_failed_reload_is_the_error_and_the_next_start_reloads_again() {
    let rig = Rig::new("self-units-reload-fails");
    armed_linux(&rig);
    rig.units.fail_reload();
    let error = rig
        .daemon()
        .remove_self_units(PlatformKind::Linux)
        .expect_err("the reload fails");
    assert_eq!(
        error.to_string(),
        format!(
            "{:?} were removed, and the service manager could not be told: \
             DaemonReload failed: the fake fails this call",
            [dropin(), dropin_dir()]
        )
    );
    assert!(!rig.there(dropin()) & rig.there(fallback_unit_path()));

    rig.placed_log.clear();
    let working = Arc::new(FakeUnitControl::new(rig.placed_log.clone()));
    let daemon = rig.daemon_with(rig.placed_fs.clone(), working);
    let again = daemon.remove_self_units(PlatformKind::Linux);
    assert_eq!(
        again.expect("removed"),
        Removed::Gone(vec![fallback_unit_path()])
    );
    let mut wanted = vec![Event::Unit(UnitCall::DaemonReload)];
    wanted.extend(gone(&fallback_unit_path()));
    assert_eq!(rig.placed_log.events(), wanted);
    assert!(!rig.there(fallback_unit_path()));

    rig.placed_log.clear();
    let third = daemon.remove_self_units(PlatformKind::Linux);
    assert_eq!(third.expect("removed"), Removed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a start that tells the service manager of files it could not remove, goes on to the next
/// file after a failure, or reads a failure as nothing there: a removal that fails is the error
/// naming its file, the fallback unit is left standing, and nothing is reloaded; a read that fails
/// is the error too, before anything is removed; a directory where the drop-in should be a file is
/// refused; and the next start removes what is left.
#[test]
fn a_removal_or_read_that_fails_is_the_error_and_nothing_is_reloaded() {
    let rig = Rig::new("self-units-remove-fails");
    armed_linux(&rig);
    // The first write the removal makes is the drop-in's removal.
    let fs_fails = Arc::new(FaultFs::failing_at(
        rig.scratch.path(),
        1,
        rig.placed_log.clone(),
    ));
    let error = rig
        .daemon_with(fs_fails, rig.units.clone())
        .remove_self_units(PlatformKind::Linux)
        .expect_err("the removal fails");
    assert_eq!(
        error.to_string(),
        format!("{}: injected failure of write 1", dropin().display())
    );
    assert!(rig.there(dropin()) & rig.there(fallback_unit_path()));
    assert!(
        !rig.placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );

    // The first read is the drop-in's lookup.
    rig.placed_log.clear();
    let read_fails =
        Arc::new(FaultFs::new(rig.scratch.path(), rig.placed_log.clone()).failing_read(1, false));
    let error = rig
        .daemon_with(read_fails, rig.units.clone())
        .remove_self_units(PlatformKind::Linux)
        .expect_err("the first read fails");
    assert!(
        matches!(&error, SelfUnitsError::Io { path, .. } if *path == dropin()),
        "{error:?}"
    );
    assert_eq!(rig.placed_log.events(), []);

    let retried = rig.daemon().remove_self_units(PlatformKind::Linux);
    assert_eq!(
        retried.expect("removed"),
        Removed::Gone(vec![dropin(), dropin_dir(), fallback_unit_path()])
    );

    let rig = Rig::new("self-units-dropin-is-a-directory");
    fs::create_dir_all(rig.host(dropin())).expect("a directory in its place");
    let error = rig
        .daemon()
        .remove_self_units(PlatformKind::Linux)
        .expect_err("a directory is not removed");
    assert!(
        matches!(&error, SelfUnitsError::Io { path, .. } if *path == dropin()),
        "{error:?}"
    );
    assert!(rig.there(dropin()));

    // The directory of the drop-in cannot be listed once the drop-in is removed.
    let rig = Rig::new("self-units-list-fails");
    armed_linux(&rig);
    // Reads, in order: the drop-in's lookup, then the listing of its directory.
    let list_fails =
        Arc::new(FaultFs::new(rig.scratch.path(), rig.placed_log.clone()).failing_read(2, false));
    let error = rig
        .daemon_with(list_fails, rig.units.clone())
        .remove_self_units(PlatformKind::Linux)
        .expect_err("the listing fails");
    assert!(
        matches!(&error, SelfUnitsError::Io { path, .. } if *path == dropin_dir()),
        "{error:?}"
    );
    assert!(!rig.there(dropin()) & rig.there(fallback_unit_path()));
    assert!(
        !rig.placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );

    // The fallback unit cannot be looked up: the error names it, and nothing is reloaded.
    let rig_unit = Rig::new("self-units-unit-read-fails");
    armed_linux(&rig_unit);
    // Reads, in order: the drop-in's lookup, the listing of its directory, the fallback unit's.
    let unit_fails = Arc::new(
        FaultFs::new(rig_unit.scratch.path(), rig_unit.placed_log.clone()).failing_read(3, false),
    );
    let error = rig_unit
        .daemon_with(unit_fails, rig_unit.units.clone())
        .remove_self_units(PlatformKind::Linux)
        .expect_err("the lookup fails");
    assert!(
        matches!(&error, SelfUnitsError::Io { path, .. } if *path == fallback_unit_path()),
        "{error:?}"
    );
    assert!(!rig_unit.there(dropin()) & rig_unit.there(fallback_unit_path()));
    assert!(
        !rig_unit
            .placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );

    // The next start finds the fallback unit, so it reloads before it removes it.
    rig.placed_log.clear();
    let retried = rig.daemon().remove_self_units(PlatformKind::Linux);
    assert_eq!(
        retried.expect("removed"),
        Removed::Gone(vec![fallback_unit_path()])
    );
    let mut wanted = vec![Event::Unit(UnitCall::DaemonReload)];
    wanted.extend(gone(&fallback_unit_path()));
    assert_eq!(rig.placed_log.events(), wanted);
}

/// Catches a failure of any one durable step of a removal being taken for success, or the removals
/// after it going on: on Linux the six writes of a full removal (the drop-in and its directory's
/// sync, the directory and the unit directory's sync, the fallback unit and its sync) each fail in
/// turn, and each is the error naming the path it was working on; a failure among the first four
/// comes before the reload and reloads nothing, one of the last two comes after the one reload;
/// on macOS the two writes of the plist's removal do the same.
#[test]
fn each_write_of_a_removal_that_fails_is_the_error_naming_its_path() {
    let linux = [
        dropin(),
        dropin(),
        dropin_dir(),
        dropin_dir(),
        fallback_unit_path(),
        fallback_unit_path(),
    ];
    for (n, path) in (1..).zip(linux) {
        let rig = Rig::new(&format!("self-units-write-{n}"));
        armed_linux(&rig);
        let fails = Arc::new(FaultFs::failing_at(
            rig.scratch.path(),
            n,
            rig.placed_log.clone(),
        ));
        let error = rig
            .daemon_with(fails, rig.units.clone())
            .remove_self_units(PlatformKind::Linux)
            .expect_err("a write fails");
        assert!(
            matches!(&error, SelfUnitsError::Io { path: at, .. } if *at == path),
            "write {n}: {error:?}"
        );
        let reloads = rig
            .placed_log
            .events()
            .iter()
            .filter(|event| **event == Event::Unit(UnitCall::DaemonReload))
            .count();
        assert_eq!(reloads, usize::from(n > 4), "write {n}");
    }
    for n in 1..=2 {
        let rig = Rig::new(&format!("self-units-macos-write-{n}"));
        armed_macos(&rig);
        let fails = Arc::new(FaultFs::failing_at(
            rig.scratch.path(),
            n,
            rig.placed_log.clone(),
        ));
        let error = rig
            .daemon_with(fails, rig.units.clone())
            .remove_self_units(PlatformKind::Macos)
            .expect_err("a write fails");
        assert!(
            matches!(&error, SelfUnitsError::Io { path, .. } if *path == job()),
            "macOS write {n}: {error:?}"
        );
    }
}

/// The files a start on this host removes, in order: on Linux the drop-in, its directory and the
/// fallback unit, on macOS the plist (`Platform::host` decides which, as the daemon's start does).
fn host_removed() -> Vec<PathBuf> {
    match cfg!(target_os = "macos") {
        true => vec![job()],
        false => vec![dropin(), dropin_dir(), fallback_unit_path()],
    }
}

/// Arms this host as a release before the hand-over left it.
fn arm_host(rig: &Rig) {
    match cfg!(target_os = "macos") {
        true => armed_macos(rig),
        false => armed_linux(rig),
    }
}

/// Catches the daemon's start not removing the files, or removing them before the boot work is
/// over: a start removes them after the readiness notice and says so in its report, the daemon's
/// own update is not held; a second start of the same host finds none and tells the service
/// manager nothing.
#[test]
fn a_start_removes_the_fallback_files_once_ready_and_a_second_start_finds_none() {
    let rig = Rig::new("units-start");
    arm_host(&rig);
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(
        report.stages[report.stages.len() - 2..],
        [Stage::Ready, Stage::SelfUnits]
    );
    assert_eq!(report.self_units, Some(Ok(Removed::Gone(host_removed()))));
    assert!(!started.daemon().ctx().self_updates_held());
    for path in host_removed() {
        assert!(!rig.there(path));
    }
    drop(started);

    rig.placed_log.clear();
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("it starts again");
    assert_eq!(
        started.boot_report().self_units,
        Some(Ok(Removed::Unchanged))
    );
    let touched_units = rig
        .placed_log
        .events()
        .into_iter()
        .any(|event| match event {
            Event::Fs(write) => format!("{write:?}").contains("/units"),
            Event::Unit(_) => true,
            Event::Mark(_) => false,
        });
    assert!(!touched_units, "{:?}", rig.placed_log.events());
}

/// Catches a start that stops when the files cannot be removed, hides why, or goes on to hand
/// over beside them: the start goes on (the daemon serves), its report holds the error, the alert
/// `self_update_units` is raised, and the daemon's own update is answered `busy`.
#[test]
fn a_start_whose_files_cannot_be_removed_goes_on_reports_why_and_holds_the_update() {
    let rig = Rig::new("units-fail");
    arm_host(&rig);
    // The call that tells the service manager: the reload on Linux, the bootout of the job on macOS.
    let refusal = match cfg!(target_os = "macos") {
        true => {
            rig.units.fail_stop(&fallback_label());
            format!(
                "Stop({:?}) failed: the fake fails this call",
                fallback_label()
            )
        }
        false => {
            rig.units.fail_reload();
            "DaemonReload failed: the fake fails this call".to_owned()
        }
    };
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(report.stages.last(), Some(&Stage::SelfUnits));
    let removed = report.self_units.clone().expect("tried");
    let error = removed.expect_err("the service manager refuses");
    assert!(
        error.ends_with(&format!("the service manager could not be told: {refusal}")),
        "{error}"
    );
    assert!(started.daemon().ctx().self_updates_held());
}
