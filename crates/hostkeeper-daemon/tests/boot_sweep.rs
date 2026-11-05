//! The daemon killed before and after every write of a step record of an update, then started
//! again after a reboot, and that start killed in turn at every write of its own: each update
//! reaches the verdict the resume table gives the record the first start found, and a committed
//! step is never run again nor a rollback skipped. The expected cells are this file's own reading
//! of the table, not the daemon's.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::boot::{BootReport, Outcome};
use hostkeeper_daemon::{Daemon, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{Record, State};
use hostkeeper_steps::{CrashPoint, Finished, ResumeAction};
use support::{IRREVERSIBLE, PENDING_LINE, Rig, crash_at, d, finished, name, update};

#[derive(Clone, Copy, Debug)]
enum Fixture {
    Success,
    RolledBackBeforeCommit,
    CommittedThenVerifyFails,
    Deferred,
    Watched,
}

const DEFER_LINE: &str = r#"{"retry_after_secs":100}"#;

/// Scripts the hook of `web` for the fixture. A step run again answers as it did the first
/// time, as a hook deciding from the world would.
fn script(rig: &Rig, fixture: Fixture) {
    let launcher = &rig.launcher;
    match fixture {
        Fixture::Success => {}
        Fixture::RolledBackBeforeCommit => launcher.exits("web", Step::Verify, &[1; 10]),
        Fixture::CommittedThenVerifyFails => {
            launcher.replies("web", Step::Cutover, &[(0, IRREVERSIBLE); 10]);
            launcher.exits("web", Step::Verify, &[1; 10]);
        }
        Fixture::Deferred => launcher.replies("web", Step::Prepare, &[(75, DEFER_LINE); 10]),
        Fixture::Watched => launcher.replies("web", Step::Verify, &[(0, PENDING_LINE), (0, "")]),
    }
}

/// A rig with `web`'s package 2.0.0, its hook scripted, and a booted daemon's boot id written.
fn rig_for(fixture: Fixture, id: &str) -> Rig {
    let rig = Rig::new(id);
    rig.add("web", "2.0.0");
    script(&rig, fixture);
    let (_, _) = rig.booted();
    rig
}

/// The verdict of the fixture run without a crash, and how many record writes it took.
fn baseline(fixture: Fixture) -> (Finished, u64) {
    let rig = rig_for(fixture, &format!("sweep-base-{fixture:?}"));
    let count = Arc::new(AtomicU64::new(0));
    let daemon = rig.daemon().with_stores(crash_at(None, Arc::clone(&count)));
    let reply = daemon.order(&update("web", "2.0.0", &d(), 1));
    let Reply::Finished {
        verdict, reason, ..
    } = reply
    else {
        panic!("{fixture:?}: {reply:?}")
    };
    (Finished { verdict, reason }, count.load(Ordering::SeqCst))
}

/// The cell of the table of design section 6.5 for a record of boot 1 read in boot 2: every
/// step it holds as pending or running was cut off by the boot.
fn expected_cell(rec: &Record) -> ResumeAction {
    let watching = rec.progress.watching.is_some();
    match (&rec.state, watching, rec.step, rec.progress.committed) {
        (State::Finished { .. }, ..) => ResumeAction::NothingFinished,
        (_, true, ..) => ResumeAction::WatcherRestart,
        (State::Done { .. }, ..) => ResumeAction::RecomputeNext,
        (_, _, Step::Prepare, _) => ResumeAction::RollbackThenDeferred { reason: "rebooted" },
        (_, _, Step::Cutover, false) => ResumeAction::RollbackThenFailed {
            phase: Phase::BeforeCommit,
        },
        (_, _, Step::Cutover, true) => ResumeAction::RestartNewReleaseContinueNext,
        (_, _, Step::Verify, false) => ResumeAction::RollbackThenFailed {
            phase: Phase::GateRelease,
        },
        (_, _, Step::Verify, true) => ResumeAction::ContinueVerifyFreshWindow,
        (_, _, Step::Rollback | Step::Cleanup | Step::Uninstall, _) => ResumeAction::RerunStep,
    }
}

/// What the cell says the update ends as, `None` when the uninterrupted run decides.
fn by_the_table(action: ResumeAction) -> Option<Finished> {
    match action {
        ResumeAction::RollbackThenDeferred { reason } => Some(finished(
            Verdict::Deferred {
                retry_after_secs: None,
            },
            Some(reason),
        )),
        ResumeAction::RollbackThenFailed { phase } => Some(finished(
            Verdict::Failed {
                phase,
                class: Class::Release,
            },
            None,
        )),
        _ => None,
    }
}

/// The fixture's update run by a daemon killed at `at`: the rig, the record the daemon left, and
/// how many steps had run.
fn first_crash(fixture: Fixture, at: CrashPoint, id: &str) -> (Rig, Option<Record>, usize) {
    let rig = rig_for(fixture, id);
    let dying = rig
        .daemon()
        .with_stores(crash_at(Some(at), Arc::new(AtomicU64::new(0))));
    let reply = dying.order(&update("web", "2.0.0", &d(), 1));
    assert!(
        matches!(reply, Reply::Interrupted { .. }),
        "{fixture:?} killed at {at:?}: {reply:?}"
    );
    drop(dying);
    let before = rig.launcher.steps("web").len();
    let found = rig.record("web");
    (rig, found, before)
}

/// Waits for the watcher the start left, if it left one.
fn settle(daemon: &Daemon, report: &BootReport) {
    if let Some(Outcome::Watching) = report.services.get("web").map(|web| web.outcome.clone()) {
        drop(daemon.ctx().watchers.wait(&name("web")));
    }
}

/// The update ended as the table says for `found`, ran no committed step again and skipped no
/// rollback.
fn check(rig: &Rig, found: &Record, before: usize, want: &Finished, what: &str) {
    let action = expected_cell(found);
    let expected = by_the_table(action).unwrap_or_else(|| want.clone());
    assert_eq!(rig.verdict("web"), Some(expected), "{what}");
    let after = rig.launcher.steps("web")[before..].to_vec();
    if found.progress.committed {
        assert!(
            !after.contains(&Step::Prepare) && !after.contains(&Step::Cutover),
            "{what}: a committed step ran again: {after:?}"
        );
    }
    if by_the_table(action).is_some() {
        assert_eq!(after.first(), Some(&Step::Rollback), "{what}: {after:?}");
    }
}

fn crash_once(fixture: Fixture, at: CrashPoint, want: &Finished) {
    let what = format!("{fixture:?} killed at {at:?}");
    let (rig, found, before) = first_crash(fixture, at, &format!("sweep-{fixture:?}"));
    rig.reboot();
    let (daemon, report) = rig.booted();
    settle(&daemon, &report);
    let Some(found) = found else {
        assert_eq!(report.services["web"].outcome, Outcome::Idle, "{what}");
        assert_eq!(rig.launcher.steps("web").len(), before, "{what}");
        return;
    };
    assert_eq!(
        report.services["web"].action,
        Some(expected_cell(&found)),
        "{what}"
    );
    check(&rig, &found, before, want, &what);
}

fn sweep(fixture: Fixture) {
    let (want, writes) = baseline(fixture);
    assert!(writes >= 7, "{fixture:?}: {writes} writes");
    for n in 1..=writes {
        for at in [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)] {
            crash_once(fixture, at, &want);
        }
    }
}

/// Catches a start after a reboot that leaves an update somewhere the table does not: a clean
/// update killed at every write ends as the table's cell for its record says.
#[test]
fn a_boot_id_sweep_over_every_step_position_reaches_the_table_verdict() {
    for fixture in [
        Fixture::Success,
        Fixture::RolledBackBeforeCommit,
        Fixture::CommittedThenVerifyFails,
        Fixture::Deferred,
        Fixture::Watched,
    ] {
        sweep(fixture);
    }
}

/// The start after the boot killed at every write of its own, then a third start: the verdict
/// is still the one the table gives the record the first start found.
fn crash_during_start(fixture: Fixture, at: CrashPoint, want: &Finished) {
    let (rig, found, _) = first_crash(fixture, at, &format!("sweep2-count-{fixture:?}"));
    let Some(found) = found else {
        return;
    };
    rig.reboot();
    let count = Arc::new(AtomicU64::new(0));
    let counting = rig.daemon().with_stores(crash_at(None, Arc::clone(&count)));
    let report = counting.boot_pass().expect("the start pass");
    settle(&counting, &report);
    drop(counting);
    let writes = count.load(Ordering::SeqCst);
    for k in 1..=writes {
        for again in [CrashPoint::BeforeWrite(k), CrashPoint::AfterWrite(k)] {
            let what = format!("{fixture:?} killed at {at:?}, its start killed at {again:?}");
            let (rig, _, before) = first_crash(fixture, at, &format!("sweep2-{fixture:?}"));
            rig.reboot();
            let dying = rig
                .daemon()
                .with_stores(crash_at(Some(again), Arc::new(AtomicU64::new(0))));
            let report = dying.boot_pass().expect("the start pass");
            settle(&dying, &report);
            drop(dying);
            let (daemon, report) = rig.booted();
            settle(&daemon, &report);
            check(&rig, &found, before, want, &what);
        }
    }
}

/// Catches a start that, killed itself, leaves an update somewhere the table does not: after
/// the update is killed at every write and the host reboots, the start is killed at every write
/// of its rewrites and of the resumed update, and the next start still ends the update as the
/// table's cell for the first record says.
#[test]
fn a_start_killed_at_every_write_of_its_own_still_reaches_the_table_verdict() {
    for fixture in [
        Fixture::Success,
        Fixture::CommittedThenVerifyFails,
        Fixture::Deferred,
        Fixture::Watched,
    ] {
        let (want, writes) = baseline(fixture);
        for n in 1..=writes {
            for at in [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)] {
                crash_during_start(fixture, at, &want);
            }
        }
    }
}
