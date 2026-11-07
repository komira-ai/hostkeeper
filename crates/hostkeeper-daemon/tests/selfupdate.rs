//! The update of hostkeeper itself through the daemon: the swap under the step lock, the record
//! that waits for the restart, the start that settles it, the fallback verb and the local list of
//! bad versions.

mod support;

use std::fs;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use clap::Parser;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::cli::{self, Cli, EXIT_FAILED};
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::fallback::{self, FallbackError};
use hostkeeper_daemon::selfupdate::{RELEASE_BINARY, binary_dir, protocol_line};
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{FakeClock, Log, Signal, spawn};
use hostkeeper_daemon::{Daemon, Refusal, Reply, StoreSeam};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_steps::record::{AwaitingRestart, State};
use hostkeeper_steps::{
    BAD_VERSIONS_FILE, BadVersions, CrashPoint, RecordStore, SelfUpdateError, Settled,
};
use support::selfupdate::{HK, LATER, NEW, OLD, SelfRig, awaiting, fell_back, own};
use support::{crash_after_writing, crash_at, d, lines, snapshot, update};

fn hk_record(s: &SelfRig) -> Option<hostkeeper_steps::Record> {
    s.rig.record(HK)
}

fn waiting_for(version: &str) -> Option<AwaitingRestart> {
    Some(AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: false,
    })
}

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

/// The fallback verb as the systemd unit of a Linux host runs it, whatever host this test runs
/// on: the verb on a Mac judges a burst of starts first (`tests/selfupdate_mac.rs`), the verb on
/// Linux acts on what the record and `previous` say.
fn linux_verb(dir: &std::path::Path) -> ExitCode {
    fallback::main_for(dir, PlatformKind::Linux, &*FakeClock::new())
}

/// Makes the host's own verb act: on a Mac, three starts of [`NEW`] in the start log, a burst
/// that is just made (a start of another boot has spent no time); on Linux systemd counts the
/// starts and nothing is needed.
#[cfg(target_os = "macos")]
fn fail_the_start_limit(s: &SelfRig) {
    let mut log = hostkeeper_steps::StartLog::load(&s.hk_state()).expect("the start log");
    for mono in [100, 110, 120] {
        log.record(hostkeeper_steps::DaemonStart {
            version: NEW.to_owned(),
            at: hostkeeper_steps::Stamp {
                boot_id: hostkeeper_steps::BootId::new("a test", "boot").ok(),
                mono_secs: mono,
            },
        })
        .expect("a start");
    }
}

#[cfg(not(target_os = "macos"))]
fn fail_the_start_limit(_: &SelfRig) {}

/// A host that ran the swap to [`NEW`]: its record waits for the restart and `current` names the
/// new binary; the daemon of [`OLD`] that ran it is returned.
fn swapped(name: &str) -> (SelfRig, Daemon) {
    let s = SelfRig::new(name);
    let daemon = s.daemon();
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    (s, daemon)
}

/// Catches the old binary reporting the update succeeded: through the executor, the hook's steps
/// run, the new binary is staged and `current` names it, and the record waits for the restart
/// (`done` at `cleanup`, never `finished`); `Status` reports no result, the reply is `Deferred`
/// `awaiting_restart`, and the daemon is asked to exit. A resend is answered the same from the
/// record and runs nothing; an order for another version is `busy`.
#[test]
fn the_record_says_awaiting_restart_not_succeeded_until_the_new_binary_has_run() {
    let s = SelfRig::new("su-awaiting");
    let daemon = s.daemon();
    assert!(!daemon.ctx().restart().requested());
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    assert!(daemon.ctx().restart().requested());
    assert_eq!(
        s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(
        s.layout().previous().expect("previous").as_deref(),
        Some(OLD)
    );
    let staged = fs::read_to_string(s.layout().version_path(NEW)).expect("the staged binary");
    assert_eq!(staged, format!("#!hostkeeper {NEW}\n"));
    let probed = s.probe.asked();
    assert_eq!(probed.len(), 1);
    assert!(probed[0].ends_with(RELEASE_BINARY), "{probed:?}");

    let rec = hk_record(&s).expect("the record");
    assert_eq!(rec.step, Step::Cleanup);
    assert!(
        matches!(rec.state, State::Done { code: Some(0), .. }),
        "{rec:?}"
    );
    assert_eq!(rec.progress.awaiting_restart, waiting_for(NEW));
    let status = daemon.status().expect("status");
    let hk = status
        .services
        .iter()
        .find(|service| service.service.as_str() == HK)
        .expect("hostkeeper in the status");
    assert_eq!(hk.last_result(), None);
    assert!(matches!(hk.record, RecordState::Unfinished(_)));

    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(true));
    let other = daemon.order(&s.order(LATER, 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy), "{other:?}");
    assert_eq!(s.hk_steps().len(), 4, "nothing ran again");
    assert_eq!(s.probe.asked().len(), 1);
}

/// Catches a confirmation that is skipped, repeated, or runs work again: the start of the new
/// binary settles the record `Succeeded` in one write, a second settle writes nothing, and a
/// resend of the order is answered `Succeeded` from the record with no launcher call.
#[test]
fn the_new_binary_confirms_on_first_start() {
    let (s, _old) = swapped("su-confirm");
    let ran = s.rig.launcher.runs().len();
    let writes = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&writes);
    let seam: StoreSeam = Arc::new(move |service: &ServiceName, store: RecordStore| {
        let count = Arc::clone(&count);
        let hk = service.as_str() == HK;
        store.with_crash_gate(Arc::new(move |point| {
            if hk & matches!(point, CrashPoint::BeforeWrite(_)) {
                count.fetch_add(1, Ordering::SeqCst);
            }
            false
        }))
    });
    let new = s.daemon_at(NEW).with_stores(seam);
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::NotPending
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let rec = hk_record(&s).expect("the record");
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
        }
    );
    assert_eq!(rec.progress.awaiting_restart, None);
    assert_eq!(
        new.order(&s.order(NEW, 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            resent: true,
        }
    );
    assert_eq!(s.rig.launcher.runs().len(), ran);
    assert_eq!(s.probe.asked().len(), 1);
    assert!(!new.ctx().restart().requested());
}

/// Catches a start that does not settle before the boot pass, or that stops on a record it cannot
/// settle: the start of the new binary reports the update confirmed and answers the resend
/// `Succeeded`; a start of the old binary whose list of bad versions is garbage reports why it
/// could not settle, starts all the same, and leaves the record waiting, which the boot pass does
/// not resume.
#[test]
fn the_start_settles_before_the_boot_pass_and_goes_on_when_it_cannot() {
    let (s, _old) = swapped("su-start");
    let started = start::startup(&s.rig.settings(), s.rig.ports(), own(NEW)).expect("started");
    assert_eq!(
        started.boot_report().self_update,
        Some(Ok(Settled::Confirmed))
    );
    assert!(matches!(
        started.daemon().order(&s.order(NEW, 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            resent: true,
            ..
        }
    ));
    drop(started);

    let (s, _old) = swapped("su-start-garbage");
    fs::write(s.hk_state().join(BAD_VERSIONS_FILE), "garbage").expect("garbage");
    let started = start::startup(&s.rig.settings(), s.rig.ports(), own(OLD)).expect("started");
    let settled = started.boot_report().self_update.clone().expect("tried");
    let error = settled.expect_err("the list is garbage");
    assert!(error.contains(BAD_VERSIONS_FILE), "{error}");
    let rec = hk_record(&s).expect("the record");
    assert_eq!(rec.progress.awaiting_restart, waiting_for(NEW));
    assert!(matches!(rec.state, State::Done { .. }));
    assert_eq!(s.hk_steps().len(), 4, "the boot pass ran nothing of it");
    assert_eq!(
        started.boot_report().services[HK].outcome,
        hostkeeper_daemon::Outcome::Idle
    );
}

/// Catches the old binary's daemon serving on after its own update asked it to exit, or exiting
/// with a status its unit's `Restart=` does not restart: the admin socket's server returns `Ok`
/// once the restart is asked for, which the command line exits with as 0; an error is its own
/// status.
#[tokio::test]
async fn the_daemon_stops_serving_once_its_restart_is_asked() {
    let s = SelfRig::new("su-serve");
    let started = start::startup(&s.rig.settings(), s.rig.ports(), own(OLD)).expect("started");
    let daemon = Arc::clone(started.daemon());
    let served = tokio::spawn(start::serve(started, support::idle_links()));
    daemon.ctx().restart().request();
    let ended = served.await.expect("the server task");
    assert!(ended.is_ok(), "{ended:?}");
    assert_eq!(cli::exit_status(&ended), 0);
    let failed = Err(start::StartError::NoNotifySocket);
    assert_eq!(cli::exit_status(&failed), EXIT_FAILED);
}

/// Catches the fallback unit putting back a binary that no update switched in: systemd runs it
/// whenever the daemon's unit fails its start limit, for any reason (a config that does not
/// load, a burst of restarts). Once the new binary confirmed its update, the fallback leaves
/// `current` as it is, writes nothing, and exits non-zero naming why, so the unit's
/// `ExecStartPost=` lines do not start the daemon again.
#[test]
fn the_fallback_leaves_a_binary_no_update_waits_for() {
    let (s, _old) = swapped("su-fallback-confirmed");
    assert_eq!(
        s.daemon_at(NEW).settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    let dir = binary_dir(&s.rig.state);
    let before = snapshot(&s.rig.state);
    let err = fallback::fallback(&dir).expect_err("nothing waits");
    assert!(matches!(err, FallbackError::NothingWaits(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        format!(
            "no update of hostkeeper waits for its restart, so {} is left as it is: the binary \
             that runs was not switched in by an update",
            dir.display()
        )
    );
    assert_eq!(code(linux_verb(&dir)), code(ExitCode::from(EXIT_FAILED)));
    assert_eq!(snapshot(&s.rig.state), before);
    assert_eq!(s.current().as_deref(), Some(NEW));
}

/// The design's named test. Catches a fallback after which the host could be ordered the same
/// broken version again: the fallback verb puts the previous binary back and records the update
/// `Failed` after the commit, of class release, `self_update_fallback`, and the version bad; a
/// resend of the order to the restarted old binary runs nothing and is answered with that failure.
/// (The coordinator's half, that a host reporting it is not ordered the version again, is in the
/// coordinator crate's test of the same name.)
#[test]
fn a_fallback_marks_the_version_bad_and_the_coordinator_stops_ordering_it() {
    let (s, _old) = swapped("su-fallback");
    let dir = binary_dir(&s.rig.state);
    let args: Vec<std::ffi::OsString> = vec![
        "hostkeeper".into(),
        "self-update-fallback".into(),
        "--dir".into(),
        dir.clone().into(),
    ];
    fail_the_start_limit(&s);
    assert_eq!(
        code(cli::run(Cli::parse_from(args))),
        code(ExitCode::SUCCESS)
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
    let rec = hk_record(&s).expect("the record");
    let (verdict, reason) = fell_back();
    assert_eq!(rec.state, State::Finished { verdict, reason });
    let bad = BadVersions::load(&s.hk_state()).expect("the list");
    assert_eq!(bad.versions(), [NEW]);

    let old = s.daemon_at(OLD);
    assert_eq!(
        old.settle_self_update().expect("settled"),
        Settled::NotPending
    );
    let (verdict, reason) = fell_back();
    let answered = Reply::Finished {
        verdict,
        reason,
        resent: true,
    };
    assert_eq!(old.order(&s.order(NEW, 1)), answered);
    assert_eq!(old.order(&s.order(NEW, 2)), answered, "another try too");
    assert_eq!(s.hk_steps().len(), 4, "nothing ran again");
}

/// Catches a bad version held only in the step record: after the version fell back, an update to
/// another version succeeds and replaces the record, and an order for the bad version again is
/// still answered with its failure and runs nothing.
#[test]
fn a_later_order_for_another_version_does_not_forget_a_bad_version() {
    let (s, _old) = swapped("su-later");
    assert!(matches!(
        fallback::fallback(&binary_dir(&s.rig.state)),
        Ok(Settled::FellBack)
    ));
    s.release(LATER);
    let old = s.daemon_at(OLD);
    assert_eq!(old.order(&s.order(LATER, 2)), awaiting(false));
    assert_eq!(s.current().as_deref(), Some(LATER));
    let later = s.daemon_at(LATER);
    assert_eq!(
        later.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    let rec = hk_record(&s).expect("the record");
    assert_eq!(rec.target_version, LATER);
    let ran = s.hk_steps().len();
    let (verdict, reason) = fell_back();
    assert_eq!(
        later.order(&s.order(NEW, 3)),
        Reply::Finished {
            verdict,
            reason,
            resent: true,
        }
    );
    assert_eq!(s.hk_steps().len(), ran);
    assert_eq!(hk_record(&s).expect("the record").target_version, LATER);
}

/// Catches a fallback that writes before it flips, or that does something the second time: with
/// the record's write stopped, the previous binary is already back and the version already bad,
/// and the start of the old binary settles the same failure; a second fallback changes nothing.
/// A binary directory with no `previous`, or a `previous` that is no executable file, exits
/// non-zero naming the cause and writes nothing.
#[test]
fn the_fallback_flips_before_it_writes_and_is_idempotent() {
    let (s, _old) = swapped("su-fallback-order");
    let layout = s.layout();
    let stopped = RecordStore::new(&s.hk_state())
        .with_crash_gate(Arc::new(|point| point == CrashPoint::BeforeWrite(1)));
    let crashed = fallback::fallback_with(&layout, &stopped);
    assert!(
        matches!(
            crashed,
            Err(FallbackError::Settle(SelfUpdateError::Record(_)))
        ),
        "{crashed:?}"
    );
    assert!(
        crashed
            .expect_err("stopped")
            .to_string()
            .starts_with("the previous binary is back, but the update could not be recorded: "),
    );
    assert_eq!(s.current().as_deref(), Some(OLD), "flipped first");
    assert_eq!(
        hk_record(&s).expect("the record").progress.awaiting_restart,
        waiting_for(NEW)
    );
    assert_eq!(
        s.daemon_at(OLD).settle_self_update().expect("settled"),
        Settled::FellBack
    );
    let (verdict, reason) = fell_back();
    assert_eq!(
        hk_record(&s).expect("the record").state,
        State::Finished { verdict, reason }
    );
    let before = snapshot(&s.rig.state);
    assert!(matches!(
        fallback::fallback(&binary_dir(&s.rig.state)),
        Ok(Settled::NotPending)
    ));
    assert_eq!(snapshot(&s.rig.state), before);

    let s = SelfRig::new("su-fallback-none");
    let dir = binary_dir(&s.rig.state);
    let err = fallback::fallback(&dir).expect_err("no previous");
    assert!(matches!(err, FallbackError::NoPrevious(_)), "{err:?}");
    assert!(
        err.to_string().contains(&dir.display().to_string()),
        "{err}"
    );
    let before = snapshot(&s.rig.state);
    assert_eq!(code(linux_verb(&dir)), code(ExitCode::from(EXIT_FAILED)));
    assert_eq!(snapshot(&s.rig.state), before);
    assert!(hk_record(&s).is_none());

    let (s, _old) = swapped("su-fallback-odd");
    let previous = s.layout().version_path(OLD);
    fs::set_permissions(
        previous,
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
    )
    .expect("no execute bit");
    let err = fallback::fallback(&binary_dir(&s.rig.state)).expect_err("not executable");
    assert!(matches!(err, FallbackError::Flip(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with("the previous binary cannot be put back: "),
        "{err}"
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert!(!s.hk_state().join(BAD_VERSIONS_FILE).exists());
}

/// The design's named test. Catches a swap under a running step, or a step started in the
/// middle of the swap: while a step of service `a` runs (its `prepare` held), an order for
/// hostkeeper is `busy` and writes no record; once `a` has finished, the next order takes the
/// step lock, and an update of service `b` ordered while the swap holds it starts its first step
/// only after the swap has written the record that waits for the restart.
#[test]
fn self_update_waits_for_an_idle_step_lock() {
    let s = SelfRig::new("su-step-lock");
    for service in ["a", "b"] {
        s.rig.add(service, "2.0");
    }
    let log = Log::default();
    s.rig.launcher.log_to(log.clone());
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    let noted = log.clone();
    let seam: StoreSeam = Arc::new(move |service: &ServiceName, store: RecordStore| {
        if service.as_str() != HK {
            return store;
        }
        let (record, noted) = (record.clone(), noted.clone());
        store.with_crash_gate(Arc::new(move |point| {
            let waits =
                fs::read_to_string(&record).is_ok_and(|text| text.contains("awaiting_restart"));
            if matches!(point, CrashPoint::AfterWrite(_)) & waits {
                noted.lock().expect("the log").push("awaiting".to_owned());
            }
            false
        }))
    });
    let daemon = Arc::new(s.daemon().with_stores(seam));

    let held = s.rig.launcher.hold("a", Step::Prepare);
    let a = {
        let daemon = Arc::clone(&daemon);
        spawn("a's order", move || {
            daemon.order(&update("a", "2.0", &d(), 1))
        })
    };
    held.entered(&a).expect("a's prepare runs");
    let refused = daemon.order(&s.order(NEW, 1));
    assert_eq!(refused.refusal(), Some(Refusal::Busy), "{refused:?}");
    assert!(hk_record(&s).is_none());
    assert!(s.probe.asked().is_empty());
    held.release();
    assert!(matches!(
        a.join().expect("a"),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));

    let (probing, probed) = Signal::new();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    s.probe.set(move || {
        probing.send(());
        released
            .lock()
            .expect("the gate")
            .recv()
            .unwrap_or_default();
        Ok(1)
    });
    let fetched = s.rig.store.notified();
    let swap = {
        let (daemon, order) = (Arc::clone(&daemon), s.order(NEW, 1));
        spawn("the swap", move || daemon.order(&order))
    };
    probed.wait(&swap).expect("the swap holds the step lock");
    let b = {
        let daemon = Arc::clone(&daemon);
        spawn("b's order", move || {
            daemon.order(&update("b", "2.0", &d(), 1))
        })
    };
    while fetched.wait(&b).expect("b has its package") != "b 2.0" {}
    release.send(()).expect("release the probe");
    assert_eq!(swap.join().expect("the swap"), awaiting(false));
    assert!(matches!(
        b.join().expect("b"),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    let events = lines(&log);
    let at = |needle: &str| events.iter().position(|line| line.starts_with(needle));
    let waits = at("awaiting").expect("the record waited");
    let b_starts = at("run updater-b-prepare-").expect("b ran");
    assert!(waits < b_starts, "{events:?}");
}

/// Catches a switch that fails being taken for a swap: the follow-on stops `link_failed` with
/// `current` unchanged and the record not waiting; a resend, once the way is clear, runs no step
/// again and finishes the swap.
#[test]
fn a_switch_that_fails_is_interrupted_and_finished_by_a_resend() {
    let s = SelfRig::new("su-switch-fails");
    let blocker = s.layout().dir().join("current.tmp");
    fs::create_dir(&blocker).expect("in the way");
    let daemon = s.daemon();
    let reply = daemon.order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::LinkFailed), "{reply:?}");
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(
        hk_record(&s).expect("the record").progress.awaiting_restart,
        None
    );
    assert!(!daemon.ctx().restart().requested());
    fs::remove_dir(&blocker).expect("clear the way");
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    assert_eq!(s.hk_steps().len(), 4, "no step ran again");
    assert_eq!(s.current().as_deref(), Some(NEW));
}

/// Catches a follow-on that stops being answered as a swap: a record that cannot be written stops
/// it `store_failed`, and a release whose binary changed after its steps ran stops it
/// `package_failed`; neither asks the daemon to exit.
#[test]
fn a_follow_on_that_stops_is_interrupted_with_its_token() {
    let s = SelfRig::new("su-follow-record");
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    let daemon = s
        .daemon()
        .with_stores(crash_after_writing(record, "awaiting_restart"));
    let reply = daemon.order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert!(!daemon.ctx().restart().requested());

    let s = SelfRig::new("su-follow-changed");
    let daemon = Arc::new(s.daemon());
    let held = s.rig.launcher.hold(HK, Step::Cleanup);
    let binary = s.rig.packages.join(HK).join(NEW).join(RELEASE_BINARY);
    let swap = {
        let (daemon, order) = (Arc::clone(&daemon), s.order(NEW, 1));
        spawn("the swap", move || daemon.order(&order))
    };
    held.entered(&swap).expect("the swap's cleanup runs");
    fs::write(&binary, "#!another binary\n").expect("change the release");
    held.release();
    let reply = swap.join().expect("the swap");
    assert_eq!(reply.refusal(), Some(Refusal::PackageFailed), "{reply:?}");
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert!(!s.layout().version_path(NEW).exists());
    assert!(!daemon.ctx().restart().requested());
}

/// Catches an update of hostkeeper to the version that already runs being left unfinished, or
/// switched: the steps run, nothing is staged, the record is finished `Succeeded`, and the daemon
/// is not asked to exit.
#[test]
fn an_update_to_the_running_version_is_finished_without_a_restart() {
    let s = SelfRig::new("su-same");
    let daemon = s.daemon_at(NEW);
    let reply = daemon.order(&s.order(NEW, 1));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                resent: false,
                ..
            }
        ),
        "{reply:?}"
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert!(matches!(
        hk_record(&s).expect("the record").state,
        State::Finished { .. }
    ));
    assert!(!daemon.ctx().restart().requested());
}

/// Catches a launcher failure of hostkeeper's own steps being answered as a verdict: the run
/// stops `launcher_failed`, nothing is staged, and a crash of the daemon in the middle of the
/// steps is resumed by the next order to the same swap.
#[test]
fn hostkeeper_steps_that_stop_are_resumed_by_the_resend() {
    let s = SelfRig::new("su-lost");
    s.rig.launcher.lose(HK, Step::Cutover);
    let daemon = s.daemon();
    let reply = daemon.order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed), "{reply:?}");
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));

    let s = SelfRig::new("su-crash-steps");
    let count = Arc::new(AtomicU64::new(0));
    let crashed = s
        .daemon()
        .with_stores(crash_at(Some(CrashPoint::AfterWrite(3)), count));
    assert!(crashed.order(&s.order(NEW, 1)).refusal().is_some());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    assert_eq!(s.current().as_deref(), Some(NEW));
}

/// Catches the verb the fallback unit runs, or the protocol verb, doing more than they say: the
/// protocol verb's text is the constant and a newline, and the command line exits 0 for it.
#[test]
fn self_update_protocol_verb_prints_only_the_constant() {
    assert_eq!(protocol_line(), "1\n");
    let ran = cli::run(Cli::parse_from(["hostkeeper", "self-update-protocol"]));
    assert_eq!(code(ran), code(ExitCode::SUCCESS));
}
