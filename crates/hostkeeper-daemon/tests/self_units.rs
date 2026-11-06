//! The units of hostkeeper's own update that the daemon writes at its start
//! (`hostkeeper_daemon::selfunits`): on Linux the fallback unit and the drop-in of its own unit,
//! on macOS the fallback timer job; each written only where it differs, followed by one word to
//! the service manager, and nothing at all on a start that finds them in place.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::Stage;
use hostkeeper_daemon::selfunits::{
    DROPIN_DIR, DROPIN_FILE, FALLBACK_UNIT, Installed, SelfUnitsError, fallback_label,
};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_placed::placer::temporary_name;
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, UnitCall};
use support::{OWN, Rig};

/// The drop-in, as `hostkeeper_steps::selfupdate::main_dropin` pins it.
const DROPIN_TEXT: &str = "[Unit]\nOnFailure=hostkeeper-fallback.service\nStartLimitIntervalSec=300\nStartLimitBurst=3\n\n[Service]\nRestart=always\nRestartSec=1\nRestartMode=direct\n";

/// The fallback unit of a host whose binary directory is `dir`, as
/// `hostkeeper_steps::selfupdate::fallback_unit` pins it.
fn fallback_text(dir: &Path) -> String {
    let dir = dir.display();
    format!(
        "[Unit]\nDescription=Put the previous updater binary back after the new one failed to start\nStartLimitIntervalSec=0\n\n[Service]\nType=oneshot\nExecStart=-{dir}/previous self-update-fallback --dir {dir}\nExecStartPost=/usr/bin/systemctl reset-failed hostkeeper.service\nExecStartPost=/usr/bin/systemctl start --no-block hostkeeper.service\n"
    )
}

/// The fallback job of a Mac whose binary directory is `dir`, as
/// `hostkeeper_steps::selfupdate::launchd_fallback_job` pins it.
fn job_text(dir: &Path) -> String {
    let dir = dir.display();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>dev.hostkeeper.hostkeeper.fallback</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{dir}/previous</string>\n    <string>self-update-fallback</string>\n    <string>--dir</string>\n    <string>{dir}</string>\n  </array>\n  <key>RunAtLoad</key>\n  <false/>\n  <key>StartInterval</key>\n  <integer>60</integer>\n</dict>\n</plist>\n"
    )
}

/// The rig's unit directory, as its host names it.
fn units() -> PathBuf {
    PathBuf::from("/units")
}

fn dropin() -> PathBuf {
    units().join(DROPIN_DIR).join(DROPIN_FILE)
}

fn fallback_unit() -> PathBuf {
    units().join(FALLBACK_UNIT)
}

fn job() -> PathBuf {
    units().join(format!("{}.plist", fallback_label()))
}

/// The writes that put a file at `dir/name` durably: its directory, the placer's temporary name
/// for it written and synced, the rename over it, the directory synced.
fn put(dir: &Path, name: &str) -> Vec<Event> {
    let tmp = dir.join(temporary_name(name));
    [
        FsWrite::CreateDirs(dir.to_path_buf()),
        FsWrite::Write(tmp.clone()),
        FsWrite::SyncFile(tmp.clone()),
        FsWrite::Rename {
            from: tmp,
            to: dir.join(name),
        },
        FsWrite::SyncDir(dir.to_path_buf()),
    ]
    .into_iter()
    .map(Event::Fs)
    .collect()
}

/// The text at the rig's host path `path`, `None` when nothing is there.
fn text(rig: &Rig, path: &Path) -> Option<String> {
    fs::read_to_string(rig.host(path)).ok()
}

/// The design's named test for the daemon side. Catches the generators having no caller, a
/// start that writes the files and does not tell systemd, one that tells it more than once, one
/// that writes the drop-in before the unit its `OnFailure=` names, and one that rewrites or
/// reloads what is already in place: the first install on Linux writes the fallback unit and then
/// the drop-in, each with its pinned text and durably, then one `daemon-reload`; the second
/// writes nothing and calls nothing.
#[test]
fn the_first_install_writes_the_fallback_unit_and_the_dropin_then_reloads_once() {
    let rig = Rig::new("self-units-linux");
    let daemon = rig.daemon();
    let first = daemon.install_self_units(PlatformKind::Linux);
    assert_eq!(
        first.expect("installed"),
        Installed::Written(vec![fallback_unit(), dropin()])
    );
    let mut wanted = put(&units(), FALLBACK_UNIT);
    wanted.extend(put(&units().join(DROPIN_DIR), DROPIN_FILE));
    wanted.push(Event::Unit(UnitCall::DaemonReload));
    assert_eq!(rig.placed_log.events(), wanted);
    assert_eq!(text(&rig, &dropin()).as_deref(), Some(DROPIN_TEXT));
    let dir = binary_dir(&rig.state);
    assert_eq!(text(&rig, &fallback_unit()), Some(fallback_text(&dir)));

    rig.placed_log.clear();
    let again = daemon.install_self_units(PlatformKind::Linux);
    assert_eq!(again.expect("installed"), Installed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a host whose units drifted (an older release's text, or a hand edit) being left so: a
/// file that holds other text is written again, the one already right is not, and the service
/// manager is told once.
#[test]
fn a_file_with_other_text_is_written_again_and_the_one_in_place_is_left() {
    let rig = Rig::new("self-units-drift");
    let daemon = rig.daemon();
    daemon
        .install_self_units(PlatformKind::Linux)
        .expect("installed");
    fs::write(rig.host(dropin()), "[Service]\nRestart=on-failure\n").expect("an older drop-in");
    rig.placed_log.clear();
    let again = daemon.install_self_units(PlatformKind::Linux);
    assert_eq!(
        again.expect("installed"),
        Installed::Written(vec![dropin()])
    );
    let mut wanted = put(&units().join(DROPIN_DIR), DROPIN_FILE);
    wanted.push(Event::Unit(UnitCall::DaemonReload));
    assert_eq!(rig.placed_log.events(), wanted);
    assert_eq!(text(&rig, &dropin()).as_deref(), Some(DROPIN_TEXT));
}

/// Catches the daemon's start not installing the units, or installing them before the boot work
/// is over: a start writes them after the readiness notice and says so in its report; a second
/// start of the same host finds them in place, writes nothing under the unit directory and does
/// not reload.
#[test]
fn a_start_installs_the_units_once_ready_and_a_second_start_leaves_them() {
    let rig = Rig::new("units-start");
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(
        report.stages[report.stages.len() - 2..],
        [Stage::Ready, Stage::SelfUnits]
    );
    assert_eq!(
        report.self_units,
        Some(Ok(Installed::Written(vec![fallback_unit(), dropin()])))
    );
    assert_eq!(text(&rig, &dropin()).as_deref(), Some(DROPIN_TEXT));
    drop(started);

    rig.placed_log.clear();
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("it starts again");
    assert_eq!(
        started.boot_report().self_units,
        Some(Ok(Installed::Unchanged))
    );
    let touched_units = rig
        .placed_log
        .events()
        .into_iter()
        .any(|event| match event {
            Event::Fs(write) => format!("{write:?}").contains("/units"),
            Event::Unit(call) => call == UnitCall::DaemonReload,
            Event::Mark(_) => false,
        });
    assert!(!touched_units, "{:?}", rig.placed_log.events());
}

/// Catches a Mac left with no fallback job, or with one launchd never loads: on macOS the job's
/// plist is written with its pinned text, then the job is booted out and bootstrapped from it
/// (the unit control's reload looks only at placed links); no systemd file is written; a second
/// install does nothing.
#[test]
fn on_macos_the_fallback_job_is_written_and_loaded_again() {
    let rig = Rig::new("self-units-macos");
    let daemon = rig.daemon();
    let first = daemon.install_self_units(PlatformKind::Macos);
    assert_eq!(first.expect("installed"), Installed::Written(vec![job()]));
    let label = fallback_label();
    assert_eq!(label, "dev.hostkeeper.hostkeeper.fallback");
    let mut wanted = put(&units(), &format!("{label}.plist"));
    wanted.push(Event::Unit(UnitCall::Stop(label.clone())));
    wanted.push(Event::Unit(UnitCall::Start(label)));
    assert_eq!(rig.placed_log.events(), wanted);
    let dir = binary_dir(&rig.state);
    assert_eq!(text(&rig, &job()), Some(job_text(&dir)));
    assert!(!rig.there(dropin()) & !rig.there(fallback_unit()));

    rig.placed_log.clear();
    let again = daemon.install_self_units(PlatformKind::Macos);
    assert_eq!(again.expect("installed"), Installed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a Mac whose job is booted out and never loaded again, or not booted out at all, being
/// taken for installed: a bootout that fails is the error and nothing is bootstrapped; a
/// bootstrap that fails after the bootout is the error, naming the plist written.
#[test]
fn on_macos_a_bootout_or_bootstrap_that_fails_is_the_error() {
    let rig = Rig::new("self-units-macos-stop-fails");
    let label = fallback_label();
    rig.units.fail_stop(&label);
    let error = rig
        .daemon()
        .install_self_units(PlatformKind::Macos)
        .expect_err("the stop fails");
    assert!(
        matches!(&error, SelfUnitsError::NotTold { written, .. } if *written == [job()]),
        "{error:?}"
    );
    let start = Event::Unit(UnitCall::Start(label.clone()));
    assert!(!rig.placed_log.events().contains(&start));

    let rig = Rig::new("self-units-macos-fails");
    rig.units.fail_start(&label);
    let error = rig
        .daemon()
        .install_self_units(PlatformKind::Macos)
        .expect_err("the start fails");
    assert!(
        matches!(&error, SelfUnitsError::NotTold { written, .. } if *written == [job()]),
        "{error:?}"
    );
    let calls: Vec<Event> = rig
        .placed_log
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Unit(_)))
        .collect();
    assert_eq!(
        calls,
        [
            Event::Unit(UnitCall::Stop(label.clone())),
            Event::Unit(UnitCall::Start(label))
        ]
    );
}

/// Catches a failed reload being hidden, or followed by a rewrite or reload at every later start:
/// the error names the files written and the refusal; the files stay as written (systemd reads
/// them at its next reload or boot); and the next start, finding them in place, writes nothing
/// and does not reload.
#[test]
fn a_failed_reload_is_the_error_and_the_files_stay_for_the_next_reload() {
    let rig = Rig::new("self-units-reload-fails");
    rig.units.fail_reload();
    let daemon = rig.daemon();
    let error = daemon
        .install_self_units(PlatformKind::Linux)
        .expect_err("the reload fails");
    assert_eq!(
        error.to_string(),
        format!(
            "{:?} were written, and the service manager could not be told: \
             DaemonReload failed: the fake fails this call",
            [fallback_unit(), dropin()]
        )
    );
    assert_eq!(text(&rig, &dropin()).as_deref(), Some(DROPIN_TEXT));
    assert!(rig.there(fallback_unit()));

    rig.placed_log.clear();
    let working = Arc::new(FakeUnitControl::new(rig.placed_log.clone()));
    let daemon = rig.daemon_with(rig.placed_fs.clone(), working);
    let again = daemon.install_self_units(PlatformKind::Linux);
    assert_eq!(again.expect("installed"), Installed::Unchanged);
    assert_eq!(rig.placed_log.events(), []);
}

/// Catches a start that tells the service manager of files it could not write, or goes on to
/// the next file after a failure: a write that fails is the error naming its file, nothing after
/// it is written and nothing is reloaded; a read that fails other than for a missing file is the
/// error too, before anything is written; and the next install writes what is missing.
#[test]
fn a_write_or_read_that_fails_is_the_error_and_nothing_is_reloaded() {
    let rig = Rig::new("self-units-write-fails");
    // The fallback unit takes the first five writes; the sixth is the drop-in's directory.
    let fs_fails = Arc::new(FaultFs::failing_at(
        rig.scratch.path(),
        6,
        rig.placed_log.clone(),
    ));
    let daemon = rig.daemon_with(fs_fails, rig.units.clone());
    let error = daemon
        .install_self_units(PlatformKind::Linux)
        .expect_err("the drop-in's write fails");
    assert_eq!(
        error.to_string(),
        format!("{}: injected failure of write 6", dropin().display())
    );
    assert!(rig.there(fallback_unit()) & !rig.there(dropin()));
    let reloaded = Event::Unit(UnitCall::DaemonReload);
    assert!(!rig.placed_log.events().contains(&reloaded));

    rig.placed_log.clear();
    let read_fails =
        Arc::new(FaultFs::new(rig.scratch.path(), rig.placed_log.clone()).failing_read(1, false));
    let daemon = rig.daemon_with(read_fails, rig.units.clone());
    let error = daemon
        .install_self_units(PlatformKind::Linux)
        .expect_err("the first read fails");
    assert!(
        matches!(&error, SelfUnitsError::Io { path, .. } if *path == fallback_unit()),
        "{error:?}"
    );
    assert_eq!(rig.placed_log.events(), []);

    let retried = rig.daemon().install_self_units(PlatformKind::Linux);
    assert_eq!(
        retried.expect("installed"),
        Installed::Written(vec![dropin()])
    );
}

/// Catches a start that stops when the units cannot be installed, or hides why: the start goes
/// on (the daemon serves) and its report holds the error.
#[test]
fn a_start_whose_units_cannot_be_installed_goes_on_and_reports_why() {
    let rig = Rig::new("units-fail");
    rig.units.fail_reload();
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(report.stages.last(), Some(&Stage::SelfUnits));
    let installed = report.self_units.clone().expect("tried");
    let error = installed.expect_err("the reload fails");
    assert!(
        error.ends_with(
            "the service manager could not be told: DaemonReload failed: the fake fails this call"
        ),
        "{error}"
    );
}
