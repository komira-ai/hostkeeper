//! The fallback of hostkeeper's own update on OpenRC: no unit or job runs a verb. supervise-daemon
//! starts the packaged binary at every start, respawns included, and that binary judges the
//! burst from the start log, as a Mac's fallback job does, before it execs `current`
//! (`hostkeeper_daemon::packaged`). Each daemon start on OpenRC goes to the start log.

mod support;

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_daemon::cli::note_start;
use hostkeeper_daemon::fallback::{FallbackError, Judged, main_for, record_start};
use hostkeeper_daemon::packaged::{self, PackagedError};
use hostkeeper_daemon::selfunits::{Installed, UnitFile, install, unit_files};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_placed::testing::Event;
use hostkeeper_steps::record::State;
use hostkeeper_steps::{BadVersions, BootId, Clock, DaemonStart, Settled, Stamp, StartLog};
use support::selfupdate::{HK, NEW, OLD, SelfRig, awaiting, fell_back, own};
use support::snapshot;

/// A host that ran the swap to [`NEW`]: its record waits for the restart and `current` names the
/// new binary.
fn swapped(name: &str) -> SelfRig {
    let s = SelfRig::new(name);
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    s
}

/// Starts of `version` in the boot of the tests' clock ([`FakeClock`]: `boot-1`, now at second
/// 1000), at the monotonic seconds `monos`.
fn started(s: &SelfRig, version: &str, monos: &[u64]) {
    let mut log = StartLog::load(&s.hk_state()).expect("the start log");
    for mono in monos {
        log.record(DaemonStart {
            version: version.to_owned(),
            at: Stamp {
                boot_id: BootId::new("a test", "boot-1").ok(),
                mono_secs: *mono,
            },
        })
        .expect("a start");
    }
}

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

/// The binary the Alpine package installed, in the test's tree: an executable file.
fn packaged_binary(s: &SelfRig) -> PathBuf {
    let path = s.rig.scratch.path().join("usr-bin-hostkeeper");
    fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("the packaged binary");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("its mode");
    path
}

/// One exec: the path, the arguments, and what `current` named at that moment.
type ExecCall = (PathBuf, Vec<OsString>, Option<PathBuf>);

/// What each exec was asked.
#[derive(Debug, Default)]
struct Execs {
    calls: RefCell<Vec<ExecCall>>,
}

impl Execs {
    /// An exec that records its call and returns as one that failed.
    fn exec(&self, path: &Path, args: &[OsString]) -> io::Error {
        self.calls
            .borrow_mut()
            .push((path.to_path_buf(), args.to_vec(), fs::read_link(path).ok()));
        io::Error::other("recorded, not run")
    }
}

/// The start of the packaged binary of `s` with the burst judged on `burst`, its exec recorded in
/// `execs`.
fn start_packaged(
    s: &SelfRig,
    image: &Path,
    burst: Option<&dyn Clock>,
    execs: &Execs,
) -> PackagedError {
    let args = [OsString::from("daemon"), OsString::from("--notify-systemd")];
    packaged::start(
        image,
        &[image],
        &s.rig.state,
        OLD,
        &args,
        burst,
        &|path, args| execs.exec(path, args),
    )
    .expect("the packaged binary execs current")
}

/// Catches an OpenRC host whose new binary crash-loops for good (nothing puts the previous binary
/// back: no fallback unit, no timer job), or whose packaged binary falls back before the burst or
/// after it execs: with [`NEW`] started twice, the packaged start changes nothing and execs
/// `current`, which still names [`NEW`]; started three times within the window, the next
/// packaged start puts [`OLD`] back before it execs `current`, the record is `Failed` after the
/// commit (`self_update_fallback`), and [`NEW`] is on the local list of bad versions.
#[test]
fn the_packaged_start_on_openrc_puts_the_previous_binary_back_after_the_burst_then_execs() {
    let s = swapped("openrc-packaged-burst");
    let image = packaged_binary(&s);
    let clock = FakeClock::new();
    let current = binary_dir(&s.rig.state).join("current");
    started(&s, NEW, &[900, 910]);

    let execs = Execs::default();
    let error = start_packaged(&s, &image, Some(&*clock), &execs);
    assert!(matches!(error, PackagedError::Exec { ref path, .. } if *path == current));
    assert_eq!(s.current().as_deref(), Some(NEW));

    started(&s, NEW, &[920]);
    let error = start_packaged(&s, &image, Some(&*clock), &execs);
    assert!(matches!(error, PackagedError::Exec { ref path, .. } if *path == current));
    assert_eq!(s.current().as_deref(), Some(OLD));
    let calls = execs.calls.borrow();
    let named: Vec<_> = calls
        .iter()
        .map(|(path, args, target)| (path.clone(), args.len(), target.clone()))
        .collect();
    assert_eq!(
        named,
        [
            (
                current.clone(),
                2,
                Some(PathBuf::from(format!("versions/{NEW}")))
            ),
            (
                current.clone(),
                2,
                Some(PathBuf::from(format!("versions/{OLD}")))
            ),
        ],
        "each start execs current, the second once the previous binary is back"
    );
    let (verdict, reason) = fell_back();
    let rec = s.rig.record(HK).expect("the record of hostkeeper");
    assert_eq!(rec.target_version, NEW);
    assert_eq!(rec.state, State::Finished { verdict, reason });
    assert!(
        BadVersions::load(&s.hk_state())
            .expect("the list")
            .contains(NEW)
    );
}

/// Catches a burst judged on a host whose service manager does not start the packaged binary
/// every time (systemd's unit starts `current`; a Mac's job does, and its timer judges): with no
/// clock given, three starts of [`NEW`] within the window move nothing, and the start execs
/// `current`.
#[test]
fn without_a_clock_the_packaged_start_judges_nothing() {
    let s = swapped("openrc-packaged-no-burst");
    let image = packaged_binary(&s);
    started(&s, NEW, &[900, 910, 920]);
    let before = snapshot(&s.rig.state);
    let execs = Execs::default();
    start_packaged(&s, &image, None, &execs);
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(snapshot(&s.rig.state), before);
    assert_eq!(execs.calls.borrow().len(), 1);
}

/// Catches a fallback that cannot judge stopping the start: a record of hostkeeper that is not
/// one makes the judge fail, the start says why, and `current` is still exec'd unchanged.
#[test]
fn a_burst_that_cannot_be_judged_does_not_stop_the_start() {
    let s = swapped("openrc-packaged-unjudged");
    let image = packaged_binary(&s);
    started(&s, NEW, &[900, 910, 920]);
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    fs::write(&record, "{").expect("a record that is not one");
    let execs = Execs::default();
    start_packaged(&s, &image, Some(&*FakeClock::new()), &execs);
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(execs.calls.borrow().len(), 1);
}

/// Catches what the start says of a judged burst: the previous binary put back, and a judge that
/// failed, each said with its cause; nothing to judge, starts left, and a busy state directory
/// are said by nothing.
#[test]
fn the_start_says_a_fallback_and_a_burst_it_could_not_judge_and_nothing_else() {
    assert_eq!(
        packaged::judged(&Ok(Judged::FellBack(Settled::FellBack))).as_deref(),
        Some("the update of hostkeeper spent its starts: the previous binary is back")
    );
    let error = FallbackError::Lock("the lock is gone".to_owned());
    assert_eq!(
        packaged::judged(&Err(error)).as_deref(),
        Some(
            "the burst of the update of hostkeeper is not judged: the state directory cannot be \
             locked: the lock is gone"
        )
    );
    for quiet in [
        Judged::NotPending,
        Judged::Busy("a daemon holds it".to_owned()),
        Judged::NotYet { starts: 2 },
    ] {
        assert_eq!(packaged::judged(&Ok(quiet)), None);
    }
}

/// Catches an OpenRC start that is not counted (the burst would never be reached): each start of
/// a host with hostkeeper's state directory goes to its start log with the daemon's version and
/// the clock's stamp, as on a Mac.
#[test]
fn each_daemon_start_on_openrc_goes_to_the_start_log() {
    let s = SelfRig::new("openrc-record-start");
    let clock = FakeClock::new();
    assert!(record_start(&s.rig.state, PlatformKind::Openrc, NEW, &*clock).expect("OpenRC"));
    note_start(&s.rig.settings(), PlatformKind::Openrc, own(NEW), &*clock);
    let stamp = Stamp::now(&*clock);
    let start = DaemonStart {
        version: NEW.to_owned(),
        at: stamp,
    };
    assert_eq!(
        StartLog::load(&s.hk_state()).expect("the log").starts(),
        [start.clone(), start]
    );
}

/// Catches the verb run by hand on OpenRC acting as the systemd fallback unit's (which falls back
/// at once): on OpenRC it judges the burst as on a Mac, so two starts of [`NEW`] change nothing
/// and exit 0, and a third within the window puts [`OLD`] back.
#[test]
fn the_fallback_verb_on_openrc_judges_the_burst() {
    let s = swapped("openrc-verb-burst");
    let dir = binary_dir(&s.rig.state);
    started(&s, NEW, &[900, 910]);
    assert_eq!(
        code(main_for(&dir, PlatformKind::Openrc, &*FakeClock::new())),
        code(ExitCode::SUCCESS)
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    started(&s, NEW, &[920]);
    assert_eq!(
        code(main_for(&dir, PlatformKind::Openrc, &*FakeClock::new())),
        code(ExitCode::SUCCESS)
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches an OpenRC daemon that writes systemd's fallback unit and drop-in (or a Mac's job) into
/// `/etc/init.d`, or tells OpenRC of them: on OpenRC the daemon has no files of its own update to
/// write, its install writes nothing and asks the service manager nothing.
#[test]
fn an_openrc_daemon_writes_no_units_of_its_own_update() {
    let s = SelfRig::new("openrc-self-units");
    assert!(unit_files(PlatformKind::Openrc, Path::new("/etc/init.d"), &s.rig.state).is_empty());
    let before = snapshot(s.rig.scratch.path());
    s.rig.placed_log.clear();
    assert_eq!(
        s.daemon()
            .install_self_units(PlatformKind::Openrc)
            .expect("installed"),
        Installed::Unchanged
    );
    assert_eq!(snapshot(s.rig.scratch.path()), before);
    assert_eq!(s.rig.placed_log.events(), []);
}

/// Catches an OpenRC install that tells systemd or launchd of a file it wrote (a `daemon-reload`
/// or the bootout of a Mac's job, neither of which OpenRC has): a file that differs is written,
/// and OpenRC is told nothing.
#[test]
fn a_file_written_on_openrc_tells_the_service_manager_nothing() {
    let s = SelfRig::new("openrc-self-units-told");
    let file = UnitFile {
        dir: s.rig.scratch.path().join("units"),
        name: "a-file".to_owned(),
        text: "text\n".to_owned(),
    };
    s.rig.placed_log.clear();
    let installed = install(
        PlatformKind::Openrc,
        std::slice::from_ref(&file),
        &*s.rig.placed_fs,
        &*s.rig.units,
    )
    .expect("installed");
    assert_eq!(installed, Installed::Written(vec![file.path()]));
    assert_eq!(
        hostkeeper_placed::Fs::read(&*s.rig.placed_fs, &file.path()).expect("written"),
        b"text\n"
    );
    let events = s.rig.placed_log.events();
    assert!(
        !events.iter().any(|event| matches!(event, Event::Unit(_))),
        "{events:?}"
    );
}
