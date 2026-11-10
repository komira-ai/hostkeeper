//! The post-success watchers the start pass restarts (design section 6.4): on a thread of the
//! daemon, never running `rollback`, bounded by the promotion ceiling measured from a stamp of
//! this boot, one per service, and stopped by a new order of the service.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use hostkeeper_daemon::boot::{BootReport, Outcome};
use hostkeeper_daemon::testing::{
    Ending, FakeClock, Missed, Signal, spawn, watcher_ends, watcher_stops,
};
use hostkeeper_daemon::watchers::STOPPED;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::RECORD_FILE;
use hostkeeper_steps::{BootId, Finished, LaunchError, ResumeAction, RunError};
use support::{
    PENDING_LINE, alerting, crash_after_writing, finished, lines, manifest_with, name, ok,
};
use support_apt::open_up;
use support_host::NativeHost;

/// `web`'s update to 2.0.0, whose promotion ceiling is 100 seconds, reached the post-success
/// watcher when the daemon died: its record is in the watcher.
fn watched(rig: &NativeHost) {
    let ceiling = manifest_with("web", "2.0.0", "promotion_ceiling_secs = 100");
    rig.running("web", support::BASE);
    rig.publish_manifest("web", "2.0.0", &ceiling);
    let (_, _) = rig.booted();
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE)]);
    let record = rig.state.join("web").join(RECORD_FILE);
    let dying = rig
        .daemon()
        .with_stores(crash_after_writing(record, r#""watching": {"#));
    let reply = dying.order(&rig.update("web", "2.0.0", 1));
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    let rec = rig.record("web").expect("a record");
    assert!(rec.progress.watching.is_some());
    assert_eq!(rig.launcher.steps("web").len(), 4);
}

/// What [`Rig::booted`] makes, and the party of the watcher of `web`: over once that watcher ends.
/// It is set before the start pass, since only ends after it is set are seen.
fn booted_watched(rig: &NativeHost) -> (Daemon, BootReport, Ending) {
    fs::create_dir_all(&rig.state).expect("the state dir");
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
    let daemon = rig.daemon();
    let ends = watcher_ends(&daemon.ctx().watchers);
    let report = daemon.boot_pass().expect("the start pass");
    (daemon, report, ends)
}

fn after_commit() -> Finished {
    finished(
        Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        None,
    )
}

/// The steps the launcher ran for `web` after its first four.
fn later_steps(rig: &NativeHost) -> Vec<Step> {
    rig.launcher.steps("web")[4..].to_vec()
}

/// Catches a watcher that rolls back, or that hides what ended it: a `verify` that fails ends
/// the restarted watcher `Failed(after_commit)` with no `rollback`, and one that halts ends it
/// with a halt.
#[test]
fn the_watcher_never_runs_rollback() {
    for (code, want) in [(1, after_commit()), (76, finished(Verdict::Halt, None))] {
        let rig = NativeHost::new("watch-never-rolls-back");
        watched(&rig);
        rig.launcher.exits("web", Step::Verify, &[code]);
        let (daemon, report) = rig.booted();
        assert_eq!(report.services["web"].outcome, Outcome::Watching);
        assert_eq!(
            report.services["web"].action,
            Some(ResumeAction::WatcherRestart)
        );
        // The watcher ends by itself; joining it waits for nothing the test holds.
        let _ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
        drop(daemon);
        assert_eq!(later_steps(&rig), [Step::Verify]);
        assert_eq!(rig.verdict("web"), Some(want), "exit {code}");
    }
}

/// Catches a watcher's end that a wait cannot see: once the restarted watcher has ended by
/// itself, a wait that names the watcher's party fails at once, saying whose watcher ended.
#[test]
fn a_wait_on_a_watcher_that_has_ended_fails_naming_it() {
    let rig = NativeHost::new("watch-ends");
    watched(&rig);
    rig.launcher.exits("web", Step::Verify, &[1]);
    let (daemon, report, ends) = booted_watched(&rig);
    assert_eq!(report.services["web"].outcome, Outcome::Watching);
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert_eq!(ended.expect("a verdict"), after_commit());
    let (_notice, signal) = Signal::<()>::new();
    let missed = Missed {
        over: "the watcher of web ended".to_owned(),
    };
    assert_eq!(signal.wait(&ends), Err(missed));
}

/// Catches a watcher whose ceiling never runs out after a reboot, or that rolls back: the clock
/// moved far past the ceiling in the old boot; the restarted watcher's ceiling begins in the new
/// boot, `verify` stays pending until it runs out (four waits of 30 seconds past 100), and the
/// update ends `Failed(after_commit)` with no `rollback`.
#[test]
fn the_watcher_restarts_after_a_reboot_and_never_runs_rollback() {
    let rig = NativeHost::new("watch-reboot");
    watched(&rig);
    rig.clock.advance(10_000);
    rig.reboot();
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE); 10]);
    let (daemon, report) = rig.booted();
    assert!(report.rebooted);
    assert_eq!(report.services["web"].outcome, Outcome::Watching);
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert_eq!(ended.expect("a verdict"), after_commit());
    assert_eq!(later_steps(&rig), [Step::Verify; 5]);
    let rec = rig.record("web").expect("a record");
    let began = rec.progress.watching.expect("the watcher's stamp");
    assert_eq!(began.boot_id.as_ref().map(BootId::as_str), Some("boot-2"));
}

/// Catches a promotion that is watched without end: `verify` keeps reporting the promotion
/// pending, and once the ceiling of 100 seconds has passed the watcher ends `Failed(after_commit)`.
#[test]
fn the_watcher_ceiling_ends_failed_after_commit() {
    let rig = NativeHost::new("watch-ceiling");
    watched(&rig);
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE); 10]);
    let (daemon, report) = rig.booted();
    assert!(!report.rebooted);
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert_eq!(ended.expect("a verdict"), after_commit());
    assert_eq!(later_steps(&rig), [Step::Verify; 5]);
    assert!(!later_steps(&rig).contains(&Step::Rollback));
}

/// Catches a second watcher of a service, or a new order refused or run beside the watcher:
/// while the watcher's `verify` runs, a second start pass starts no other; a new order of the
/// service stops the watcher (its `verify` ends, and no further one starts), stops the unit of
/// that `verify` by name, and runs to its verdict; no two steps ever ran at once.
#[test]
fn a_new_order_stops_the_watcher_and_only_one_watcher_runs_per_service() {
    let rig = NativeHost::new("watch-new-order");
    watched(&rig);
    rig.add_staged("web", "3.0.0");
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE)]);
    let gate = rig.launcher.hold("web", Step::Verify);
    let (daemon, _, ends) = booted_watched(&rig);
    // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
    let gate = gate;
    gate.entered(&ends).expect("the watcher's verify runs");
    let web = name("web");
    assert!(daemon.ctx().watchers.running(&web));
    let again = daemon.boot_pass().expect("a second start pass");
    assert_eq!(again.services["web"].outcome, Outcome::Watching);
    assert_eq!(later_steps(&rig), [Step::Verify]);
    let daemon = Arc::new(daemon);
    let stops = watcher_stops(&daemon.ctx().watchers);
    let third = rig.update("web", "3.0.0", 2);
    let order = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || daemon.order(&third))
    };
    // The order has told the watcher to stop and waits for it.
    assert_eq!(
        stops.wait(&order).expect("the watcher is told to stop"),
        "web"
    );
    gate.release();
    let reply = order.join().expect("the order");
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: false
        }
    );
    assert!(!daemon.ctx().watchers.running(&web));
    assert!(daemon.ctx().watchers.wait(&web).is_none());
    assert!(
        rig.launcher
            .stops()
            .contains(&"updater-web-verify-1".to_owned())
    );
    assert_eq!(rig.launcher.most_running(), 1);
    let rec = rig.record("web").expect("a record");
    assert_eq!((rec.generation, rec.target_version.as_str()), (2, "3.0.0"));
}

/// Catches a stopped watcher that runs one more `verify`, or that waits out its poll: the
/// watcher is stopped while its `verify` runs; that `verify` ends, the wait after it ends at
/// once, no other `verify` starts, and the watcher says why it ended.
#[test]
fn a_watcher_stopped_while_it_waits_runs_nothing_more() {
    let rig = NativeHost::new("watch-stopped");
    watched(&rig);
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE); 3]);
    let gate = rig.launcher.hold("web", Step::Verify);
    let (daemon, report, ends) = booted_watched(&rig);
    // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
    let gate = gate;
    assert_eq!(report.services["web"].outcome, Outcome::Watching);
    gate.entered(&ends).expect("the watcher's verify runs");
    let daemon = Arc::new(daemon);
    let stops = watcher_stops(&daemon.ctx().watchers);
    let stopping = {
        let daemon = Arc::clone(&daemon);
        spawn("the stop", move || daemon.ctx().watchers.stop(&name("web")))
    };
    assert_eq!(
        stops.wait(&stopping).expect("the watcher is told to stop"),
        "web"
    );
    gate.release();
    let ended = stopping.join().expect("the stop").expect("a watcher");
    let Err(RunError::Launch(LaunchError::Launcher(why))) = ended else {
        panic!("stopped: {ended:?}")
    };
    assert_eq!(why, STOPPED);
    assert_eq!(later_steps(&rig), [Step::Verify]);
    assert!(rig.verdict("web").is_none());
}

/// Catches a watcher restarted over a release that changed, or without what it needs: a tree no
/// longer the one the record verified halts the update; with the params file gone, it is held.
#[test]
fn a_watcher_that_cannot_be_rebuilt_is_not_restarted() {
    let rig = NativeHost::new("watch-changed");
    watched(&rig);
    open_up(&rig.packages.join("web/2.0.0"));
    std::fs::write(rig.packages.join("web/2.0.0/extra"), "x").expect("change the tree");
    let (daemon, report) = rig.booted();
    assert_eq!(
        report.services["web"].verdict(),
        Some(finished(Verdict::Halt, Some("artifact_changed")))
    );
    assert!(daemon.ctx().watchers.wait(&name("web")).is_none());

    let rig = NativeHost::new("watch-no-params");
    watched(&rig);
    std::fs::remove_file(rig.state.join("web/params.toml")).expect("remove the params");
    let (_, report) = rig.booted();
    assert!(matches!(
        report.services["web"].outcome,
        Outcome::Unresumable(_)
    ));
    assert_eq!(rig.launcher.steps("web").len(), 4);
}

/// Catches an update resumed at the start that reaches its watcher and holds up the start, or
/// whose watcher is lost: the update's `cleanup` was cut off; it runs again, the update reaches
/// the watcher, the start pass goes on, and the watcher on its thread ends the update.
#[test]
fn an_update_that_reaches_its_watcher_at_the_start_is_watched_on_its_thread() {
    let rig = NativeHost::new("watch-handoff");
    rig.add_staged("web", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE), (0, "")]);
    rig.launcher.lose("web", Step::Cleanup);
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    drop(daemon);
    let (daemon, report) = rig.booted();
    assert_eq!(
        report.services["web"].action,
        Some(ResumeAction::StopUnitAndRerun)
    );
    assert_eq!(report.services["web"].outcome, Outcome::Watching);
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert_eq!(ended.expect("a verdict"), ok());
    assert_eq!(
        rig.launcher.steps("web")[4..],
        [Step::Cleanup, Step::Verify]
    );
}

fn alert(kind: &str, why: &str) -> String {
    format!("hostkeeper daemon: level=error alert={kind} service=web: {why}")
}

/// Catches a watcher whose `verify` the dead-man cannot stop being forgotten: the service is
/// locked for good, the status says why, an order of it is `busy`, and the alert names the unit.
#[test]
fn a_watcher_that_cannot_stop_its_verify_locks_the_service_and_alerts() {
    let rig = NativeHost::new("watch-unstopped");
    watched(&rig);
    rig.launcher.fail_stops("web");
    let stops = rig.launcher.stops_seen();
    let gate = rig.launcher.hold("web", Step::Verify);
    let clock = FakeClock::frozen();
    let mut ports = rig.ports();
    ports.clock = clock.clone();
    ports.deadman_poll_secs = Some(1);
    let (daemon, alerts) = alerting(Daemon::new(
        support::config(),
        rig.dirs(),
        ports,
        support::OWN,
    ));
    // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
    let gate = gate;
    let ends = watcher_ends(&daemon.ctx().watchers);
    let report = daemon.boot_pass().expect("the start pass");
    assert_eq!(report.services["web"].outcome, Outcome::Watching);
    gate.entered(&ends).expect("the watcher's verify runs");
    // Past the bound of `verify` (1000 seconds): the dead-man tries to stop it, and fails. It
    // ticks, then sleeps on the clock: by its second sleep from here, one whole tick has looked
    // past the bound.
    clock.advance(1001);
    let ticked = clock.sleeps(2);
    assert_eq!(
        stops.wait(&ticked).expect("the dead-man's stop"),
        "updater-web-verify-1"
    );
    gate.release();
    let web = name("web");
    let _ended = daemon.ctx().watchers.wait(&web).expect("a watcher");
    assert!(daemon.ctx().locks.holder(&web).expect("held").stuck);
    let status = daemon.status().expect("the status");
    assert!(status.services[0].stuck);
    assert_eq!(
        lines(&alerts),
        [alert(
            "unstoppable_unit",
            "the dead-man could not stop updater-web-verify-1 at its bound (the launcher failed: updater-web-verify-1 could not be stopped); the step is left running"
        )]
    );
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
}

/// Catches a watcher that stops without a verdict unseen: one whose record cannot be written
/// alerts `journal_unavailable`, one whose `verify` the launcher loses alerts
/// `watcher_interrupted`; neither locks the service.
#[test]
fn a_watcher_that_stops_without_a_verdict_alerts() {
    let rig = NativeHost::new("watch-journal");
    watched(&rig);
    let full = Arc::new(|_: u64| Some(std::io::Error::from_raw_os_error(28)));
    let (daemon, alerts) = alerting(rig.daemon().with_stores(Arc::new(
        move |_: &hostkeeper_config::names::ServiceName, store: hostkeeper_steps::RecordStore| {
            store.with_write_fault(full.clone())
        },
    )));
    daemon.boot_pass().expect("the start pass");
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert!(matches!(ended, Err(RunError::Record(_))), "{ended:?}");
    assert_eq!(
        lines(&alerts),
        [alert(
            "journal_unavailable",
            "cannot use the state directory: No space left on device (os error 28)"
        )]
    );
    assert!(daemon.ctx().locks.holder(&name("web")).is_none());

    let rig = NativeHost::new("watch-lost");
    watched(&rig);
    rig.launcher.lose("web", Step::Verify);
    let (daemon, alerts) = alerting(rig.daemon());
    daemon.boot_pass().expect("the start pass");
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert!(matches!(ended, Err(RunError::Launch(_))), "{ended:?}");
    assert_eq!(
        lines(&alerts),
        [alert(
            "watcher_interrupted",
            "the launcher failed: updater-web-verify-1 was lost"
        )]
    );
    assert!(daemon.ctx().locks.holder(&name("web")).is_none());
}

/// Catches a watcher that waits out its poll once stopped: stopped while it waits between two
/// runs of `verify`, it ends as soon as the wait sees the stop, and starts no other `verify`.
#[test]
fn a_watcher_stopped_in_its_wait_ends_at_once() {
    let rig = NativeHost::new("watch-stopped-waiting");
    watched(&rig);
    rig.launcher
        .replies("web", Step::Verify, &[(0, PENDING_LINE); 3]);
    let wait = rig.clock.park_next_sleep();
    let (daemon, _, ends) = booted_watched(&rig);
    wait.entered(&ends).expect("the watcher's wait begins");
    let daemon = Arc::new(daemon);
    // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
    let wait = wait;
    let stops = watcher_stops(&daemon.ctx().watchers);
    let stopping = {
        let daemon = Arc::clone(&daemon);
        spawn("the stop", move || daemon.ctx().watchers.stop(&name("web")))
    };
    assert_eq!(
        stops.wait(&stopping).expect("the watcher is told to stop"),
        "web"
    );
    wait.release();
    let ended = stopping.join().expect("the stop").expect("a watcher");
    let Err(RunError::Launch(LaunchError::Launcher(why))) = ended else {
        panic!("stopped: {ended:?}")
    };
    assert_eq!(why, STOPPED);
    assert_eq!(later_steps(&rig), [Step::Verify]);
}

/// Catches a daemon whose end waits forever on a watcher, or leaves it running: the daemon ends
/// while its watcher's `verify` runs, and while it waits; each time the daemon's end returns once
/// the watcher has ended, and no `rollback` ran.
#[test]
fn the_daemon_ends_its_watchers_when_it_ends() {
    for in_verify in [true, false] {
        let rig = NativeHost::new("watch-shutdown");
        watched(&rig);
        rig.launcher
            .replies("web", Step::Verify, &[(0, PENDING_LINE); 10]);
        let gate = in_verify.then(|| rig.launcher.hold("web", Step::Verify));
        let wait = (!in_verify).then(|| rig.clock.park_next_sleep());
        let (daemon, _, ends) = booted_watched(&rig);
        // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
        let (gate, wait) = (gate, wait);
        for held in gate.iter().chain(wait.iter()) {
            held.entered(&ends).expect("the watcher is held");
        }
        let ending = spawn("the daemon's end", move || drop(daemon));
        gate.iter().for_each(|gate| gate.release());
        wait.iter().for_each(|wait| wait.release());
        ending.join().expect("the daemon ends");
        assert!(!later_steps(&rig).contains(&Step::Rollback));
    }
}

/// Catches a watcher held up by another service's long step: while `beta`'s `prepare` runs (and
/// holds the step lock), `web`'s watcher runs its `verify` and ends the update.
#[test]
fn a_watcher_runs_while_another_service_holds_a_long_step() {
    let rig = NativeHost::new("watch-beside");
    watched(&rig);
    rig.add_staged("beta", "2.0.0");
    rig.launcher.exits("web", Step::Verify, &[1]);
    let watcher_verify = rig.launcher.hold("web", Step::Verify);
    let (daemon, _, ends) = booted_watched(&rig);
    watcher_verify
        .entered(&ends)
        .expect("the watcher's verify runs");
    let daemon = Arc::new(daemon);
    // Dropped before the daemon, which joins the watcher the gate holds, if the test fails here.
    let watcher_verify = watcher_verify;
    let beta_prepare = rig.launcher.hold("beta", Step::Prepare);
    let beta = {
        let daemon = Arc::clone(&daemon);
        spawn("beta's order", move || {
            daemon.order(&rig.update("beta", "2.0.0", 1))
        })
    };
    beta_prepare.entered(&beta).expect("beta's prepare runs");
    watcher_verify.release();
    let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
    assert_eq!(ended.expect("a verdict"), after_commit());
    beta_prepare.release();
    assert_eq!(
        beta.join().expect("beta's order"),
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: false
        }
    );
}
