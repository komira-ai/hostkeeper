//! The fallback of hostkeeper's own update on macOS: the timer job runs the same verb as the
//! systemd fallback unit, and on a Mac the verb falls back only once the version the record
//! waits for has failed its burst of starts, under the state directory's lock, while no other
//! update is unfinished. Each daemon start on a Mac goes to the start log the verb reads.

mod support;

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_daemon::cli::note_start;
use hostkeeper_daemon::fallback::{
    FallbackError, Judged, fallback_after_burst, main_for, record_start,
};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_daemon::start::LOCK_FILE;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_steps::record::{AwaitingRestart, State};
use hostkeeper_steps::{
    BadVersions, BootId, DaemonStart, Settled, Stamp, StartLog, StartLogError, starts::STARTS_FILE,
};
use support::selfupdate::{HK, NEW, OLD, SelfRig, awaiting, fell_back, own, put_record, record_of};
use support::snapshot;

/// A host that ran the swap to [`NEW`]: its record waits for the restart and `current` names the
/// new binary.
fn swapped(name: &str) -> SelfRig {
    let s = SelfRig::new(name);
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    s
}

/// Starts of `version` at the monotonic seconds `monos`, in the start log of `s`.
fn started(s: &SelfRig, version: &str, monos: &[u64]) {
    let mut log = StartLog::load(&s.hk_state()).expect("the start log");
    for mono in monos {
        log.record(DaemonStart {
            version: version.to_owned(),
            at: Stamp {
                boot_id: BootId::new("a test", "boot").ok(),
                mono_secs: *mono,
            },
        })
        .expect("a start");
    }
}

/// Everything under the state directory but the lock file, which a run of the fallback (as a
/// start of the daemon) may make.
fn unchanged(state: &Path) -> Vec<(PathBuf, String)> {
    snapshot(state)
        .into_iter()
        .filter(|(path, _)| *path != state.join(LOCK_FILE))
        .collect()
}

fn hk_record(s: &SelfRig) -> hostkeeper_steps::Record {
    s.rig.record(HK).expect("the record of hostkeeper")
}

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

/// The wait of the update to `version` an order of `SelfRig::order` ran: it keeps that order's
/// id, for the binary that settles it to report the verdict under.
fn waiting_for(version: &str) -> Option<AwaitingRestart> {
    Some(AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: false,
        order_id: Some(format!("order-{HK}-{version}")),
    })
}

/// Catches a Mac trigger that bypasses the verb, or acts before the burst: with the new binary
/// [`NEW`] started twice the verb on macOS changes nothing; started three times within the
/// window and never confirmed, `current` is [`OLD`] again, the record is `Failed` after the
/// commit for [`NEW`] (`self_update_fallback`), and [`NEW`] is on the local list of bad
/// versions, the verdict the systemd fallback writes. Run again, it finds nothing waiting.
#[test]
fn the_mac_fallback_flips_back_marks_the_version_bad_and_reports_after_commit() {
    let s = swapped("mac-fallback-burst");
    let dir = binary_dir(&s.rig.state);
    started(&s, NEW, &[100, 110]);
    let before = unchanged(&s.rig.state);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotYet { starts: 2 }
    );
    assert_eq!(unchanged(&s.rig.state), before);
    assert_eq!(s.current().as_deref(), Some(NEW));

    started(&s, NEW, &[120]);
    assert_eq!(
        code(main_for(&dir, PlatformKind::Macos, &*FakeClock::new())),
        code(ExitCode::SUCCESS)
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
    let (verdict, reason) = fell_back();
    let rec = hk_record(&s);
    assert_eq!(rec.target_version, NEW);
    assert_eq!(rec.state, State::Finished { verdict, reason });
    assert_eq!(rec.progress.awaiting_restart, None);
    assert!(
        BadVersions::load(&s.hk_state())
            .expect("the list")
            .contains(NEW)
    );

    let after = unchanged(&s.rig.state);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotPending
    );
    assert_eq!(unchanged(&s.rig.state), after);
    assert_eq!(
        s.daemon_at(OLD).settle_self_update().expect("settled"),
        Settled::NotPending
    );
}

/// Catches a burst counted from the starts of another binary, or across the window: three starts
/// of [`OLD`] are not the new binary's; three of [`NEW`] spread over more than the window are not
/// a burst; nothing waiting means nothing to judge, whatever the log holds.
#[test]
fn only_the_awaited_versions_starts_within_the_window_are_its_burst() {
    let s = swapped("mac-fallback-other-starts");
    let dir = binary_dir(&s.rig.state);
    started(&s, OLD, &[100, 110, 120]);
    started(&s, NEW, &[200, 600]);
    started(&s, NEW, &[1000]);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotYet { starts: 3 }
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(hk_record(&s).progress.awaiting_restart, waiting_for(NEW));

    let s = SelfRig::new("mac-fallback-nothing-waits");
    let dir = binary_dir(&s.rig.state);
    started(&s, NEW, &[1, 2, 3]);
    let before = unchanged(&s.rig.state);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotPending
    );
    assert_eq!(unchanged(&s.rig.state), before);
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches a Mac fallback that skips the refusals of the self-update itself: while another
/// service's update is running (its record `running`), or while a daemon holds the state
/// directory's lock, the verb changes nothing and says why; once both are gone, the same burst
/// falls back.
#[test]
fn a_mac_fallback_is_refused_while_another_services_step_runs_or_waits_for_the_lock() {
    let s = swapped("mac-fallback-busy");
    let dir = binary_dir(&s.rig.state);
    started(&s, NEW, &[100, 110, 120]);
    let running = State::Running {
        started: Stamp {
            boot_id: BootId::new("a test", "boot").ok(),
            mono_secs: 90,
        },
        deadline_secs: 400,
        unbounded: false,
    };
    put_record(&s.rig, "a", &record_of("a", running));
    let before = unchanged(&s.rig.state);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::Busy(
            "the step record of a is running: hostkeeper updates itself only while no other \
             update is unfinished"
                .to_owned()
        )
    );
    assert_eq!(unchanged(&s.rig.state), before);
    assert_eq!(s.current().as_deref(), Some(NEW));

    let finished = State::Finished {
        verdict: hostkeeper_hook::Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    put_record(&s.rig, "a", &record_of("a", finished));
    let lock_path = s.rig.state.join(LOCK_FILE);
    let held = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("the lock file");
    held.try_lock().expect("the daemon's lock");
    let before = unchanged(&s.rig.state);
    let judged = fallback_after_burst(&dir, &*FakeClock::new()).expect("judged");
    let Judged::Busy(why) = &judged else {
        panic!("{judged:?} is not busy");
    };
    assert!(
        why.starts_with(&format!("a daemon holds {}: ", lock_path.display())),
        "{why}"
    );
    assert_eq!(unchanged(&s.rig.state), before);
    assert_eq!(s.current().as_deref(), Some(NEW));

    drop(held);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::FellBack(Settled::FellBack)
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches a Mac fallback that goes on when it cannot judge: a start log that is not one and a
/// state directory that cannot be locked are errors naming what failed, and the verb exits
/// non-zero; `current` is not moved.
#[test]
fn a_mac_fallback_that_cannot_judge_changes_nothing_and_fails() {
    let s = swapped("mac-fallback-garbage-log");
    let dir = binary_dir(&s.rig.state);
    fs::write(s.hk_state().join(STARTS_FILE), "not a log").expect("garbage");
    let error = fallback_after_burst(&dir, &*FakeClock::new()).expect_err("no log to judge by");
    assert!(matches!(
        error,
        FallbackError::Starts(StartLogError::Garbage { .. })
    ));
    assert!(
        error
            .to_string()
            .starts_with("the burst cannot be judged: "),
        "{error}"
    );
    assert_eq!(
        code(main_for(&dir, PlatformKind::Macos, &*FakeClock::new())),
        code(ExitCode::from(hostkeeper_daemon::cli::EXIT_FAILED))
    );
    assert_eq!(s.current().as_deref(), Some(NEW));

    let s = swapped("mac-fallback-no-lock");
    let dir = binary_dir(&s.rig.state);
    let lock_path = s.rig.state.join(LOCK_FILE);
    fs::create_dir(&lock_path).expect("a directory where the lock goes");
    let error = fallback_after_burst(&dir, &*FakeClock::new()).expect_err("no lock");
    assert!(matches!(error, FallbackError::Lock(_)), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with("the state directory cannot be locked: "),
        "{error}"
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
}

/// Catches a daemon start that is not counted on a Mac, or that writes on Linux: on macOS each
/// start of a host with hostkeeper's state directory is added to its start log with the daemon's
/// version and the clock's stamp; a host without that directory, and any Linux host, writes
/// nothing; a log that cannot be written is an error, which the start says and goes past.
#[test]
fn each_daemon_start_on_a_mac_goes_to_the_start_log() {
    let s = SelfRig::new("mac-record-start");
    let clock = FakeClock::new();
    let before = snapshot(&s.rig.state);
    assert!(!record_start(&s.rig.state, PlatformKind::Linux, NEW, &*clock).expect("Linux"));
    assert_eq!(snapshot(&s.rig.state), before);

    assert!(record_start(&s.rig.state, PlatformKind::Macos, NEW, &*clock).expect("a Mac"));
    note_start(&s.rig.settings(), PlatformKind::Macos, own(NEW), &*clock);
    let log = StartLog::load(&s.hk_state()).expect("the log");
    let stamp = Stamp::now(&*clock);
    assert_eq!(
        log.starts(),
        [
            DaemonStart {
                version: NEW.to_owned(),
                at: stamp.clone(),
            },
            DaemonStart {
                version: NEW.to_owned(),
                at: stamp,
            },
        ]
    );

    let bare = s.rig.scratch.path().join("bare-state");
    fs::create_dir(&bare).expect("a state directory without hostkeeper");
    assert!(!record_start(&bare, PlatformKind::Macos, NEW, &*clock).expect("nothing to count"));
    assert_eq!(fs::read_dir(&bare).expect("list").count(), 0);

    fs::write(s.hk_state().join(STARTS_FILE), "not a log").expect("garbage");
    let error = record_start(&s.rig.state, PlatformKind::Macos, NEW, &*clock)
        .expect_err("a log that is not one");
    assert!(matches!(error, StartLogError::Garbage { .. }), "{error:?}");
    // The start goes on: the error is said, not returned.
    note_start(&s.rig.settings(), PlatformKind::Macos, own(NEW), &*clock);
    assert_eq!(
        fs::read_to_string(s.hk_state().join(STARTS_FILE)).expect("read"),
        "not a log"
    );
}

/// Starts of `version` in the boot of the tests' clock ([`FakeClock`]: `boot-1`, now at second
/// 1000), at the monotonic seconds `monos`.
fn started_in_this_boot(s: &SelfRig, version: &str, monos: &[u64]) {
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

/// Catches a burst long past taken for a binary failing now: three starts within the window
/// whose latest is 880 seconds old change nothing; three whose latest is 299 seconds old fall
/// back.
#[test]
fn a_burst_long_past_is_not_a_binary_failing_now() {
    let s = swapped("mac-fallback-stale");
    let dir = binary_dir(&s.rig.state);
    started_in_this_boot(&s, NEW, &[100, 110, 120]);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotYet { starts: 3 }
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    started_in_this_boot(&s, NEW, &[690, 695, 701]);
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::FellBack(Settled::FellBack)
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches a Mac fallback that judges what it cannot read: a step record of hostkeeper that is
/// not one is an error and nothing moves; one written by a newer daemon waits for nothing this
/// binary knows, so nothing is judged; a burst with no `previous` to go back to is an error, the
/// verb exits non-zero, and the record still waits.
#[test]
fn a_mac_fallback_over_a_record_it_cannot_use_or_no_previous_changes_nothing() {
    let s = swapped("mac-fallback-corrupt");
    let dir = binary_dir(&s.rig.state);
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    fs::write(&record, "{").expect("a record that is not one");
    let error = fallback_after_burst(&dir, &*FakeClock::new()).expect_err("unreadable");
    assert!(matches!(error, FallbackError::Settle(_)), "{error:?}");
    assert_eq!(s.current().as_deref(), Some(NEW));

    fs::write(&record, r#"{"schema":9}"#).expect("a newer record");
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotPending
    );
    assert_eq!(s.current().as_deref(), Some(NEW));

    let s = swapped("mac-fallback-no-previous");
    let dir = binary_dir(&s.rig.state);
    started(&s, NEW, &[100, 110, 120]);
    fs::remove_file(dir.join("previous")).expect("no previous");
    let error = fallback_after_burst(&dir, &*FakeClock::new()).expect_err("nothing to go back to");
    assert!(matches!(error, FallbackError::NoPrevious(_)), "{error:?}");
    assert_eq!(
        code(main_for(&dir, PlatformKind::Macos, &*FakeClock::new())),
        code(ExitCode::from(hostkeeper_daemon::cli::EXIT_FAILED))
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(hk_record(&s).progress.awaiting_restart, waiting_for(NEW));
}

/// Catches starts of an unknown boot counted as one burst: three starts within the window whose
/// boot id could not be read (stored with an empty `boot_id`, as older releases wrote on a Mac)
/// may be of different boots, so they are not a burst and nothing falls back.
#[test]
fn starts_of_an_unknown_boot_are_not_a_burst() {
    let s = swapped("mac-fallback-unknown-boot");
    let dir = binary_dir(&s.rig.state);
    let mut log = StartLog::load(&s.hk_state()).expect("the start log");
    for mono in [990, 995, 999] {
        log.record(DaemonStart {
            version: NEW.to_owned(),
            at: Stamp {
                boot_id: None,
                mono_secs: mono,
            },
        })
        .expect("a start");
    }
    assert_eq!(
        fallback_after_burst(&dir, &*FakeClock::new()).expect("judged"),
        Judged::NotYet { starts: 3 }
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
}
