//! hostkeeper's own update on OpenRC is the hand-over, as on the other systems: supervise-daemon
//! starts the packaged binary at every start, which execs `current`, whose start execs `next`
//! while its count lasts (`hostkeeper_daemon::handover`). No release before the hand-over ran on
//! an OpenRC host, so nothing of the old way is written, counted or removed there: no start log,
//! no fallback files; the verb `self-update-fallback`, run by hand, acts as on Linux.

mod support;

use std::fs;
use std::process::ExitCode;

use hostkeeper_daemon::cli::note_start;
use hostkeeper_daemon::fallback::{main_for, record_start};
use hostkeeper_daemon::selfunits::{
    DROPIN_DIR, DROPIN_FILE, FALLBACK_UNIT, Removed, fallback_label, remove,
};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_steps::starts::STARTS_FILE;
use support::selfupdate::{NEW, SelfRig, awaiting, own};
use support::snapshot;

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

/// Catches an OpenRC start counted in the start log, which only a Mac's fallback job of a release
/// before the hand-over reads: a start on OpenRC writes nothing, the log stays absent.
#[test]
fn a_daemon_start_on_openrc_writes_no_start_log() {
    let s = SelfRig::new("openrc-record-start");
    let clock = FakeClock::new();
    let before = snapshot(&s.rig.state);
    assert!(!record_start(&s.rig.state, PlatformKind::Openrc, NEW, &*clock).expect("OpenRC"));
    note_start(&s.rig.settings(), PlatformKind::Openrc, own(NEW), &*clock);
    assert_eq!(snapshot(&s.rig.state), before);
    assert!(!s.hk_state().join(STARTS_FILE).exists());
}

/// Catches the verb run by hand on OpenRC moving a link of a hand-over, or judging a burst no
/// OpenRC host counts: it is the Linux fallback, so with nothing waiting it refuses, and with a
/// hand-over waiting (an update ordered to the daemon) it refuses too and leaves `current` and
/// the record as they were, for the start of `current` to settle.
#[test]
fn the_fallback_verb_on_openrc_acts_as_on_linux_and_leaves_a_hand_over_alone() {
    let s = SelfRig::new("openrc-verb");
    let dir = binary_dir(&s.rig.state);
    let refused = code(ExitCode::from(hostkeeper_daemon::cli::EXIT_FAILED));
    assert_eq!(
        code(main_for(&dir, PlatformKind::Openrc, &*FakeClock::new())),
        refused,
        "nothing waits"
    );
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    let before = (s.current(), snapshot(&s.hk_state()));
    assert_eq!(
        code(main_for(&dir, PlatformKind::Openrc, &*FakeClock::new())),
        refused,
        "a hand-over waits"
    );
    assert_eq!((s.current(), snapshot(&s.hk_state())), before);
}

/// Catches an OpenRC daemon that removes, or tells OpenRC of, files of systemd or of a Mac that
/// are not its own: with every fallback file of the other systems in its unit directory, the
/// removal on OpenRC finds nothing to do, removes nothing and asks the service manager nothing.
#[test]
fn an_openrc_daemon_removes_no_fallback_files_and_tells_nothing() {
    let s = SelfRig::new("openrc-self-units");
    let units = s.rig.scratch.path().join("init.d");
    for (dir, name) in [
        (units.join(DROPIN_DIR), DROPIN_FILE.to_owned()),
        (units.clone(), FALLBACK_UNIT.to_owned()),
        (units.clone(), format!("{}.plist", fallback_label())),
    ] {
        fs::create_dir_all(&dir).expect("the directory");
        fs::write(dir.join(name), "of another system\n").expect("a file");
    }
    let before = snapshot(&units);
    s.rig.placed_log.clear();
    assert_eq!(
        remove(
            PlatformKind::Openrc,
            &units,
            &*s.rig.placed_fs,
            &*s.rig.units
        )
        .expect("removed"),
        Removed::Unchanged
    );
    assert_eq!(
        s.daemon()
            .remove_self_units(PlatformKind::Openrc)
            .expect("removed"),
        Removed::Unchanged
    );
    assert_eq!(snapshot(&units), before);
    assert_eq!(s.rig.placed_log.events(), []);
}
