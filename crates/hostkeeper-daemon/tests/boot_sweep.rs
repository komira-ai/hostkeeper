//! The daemon killed before and after every write of a step record of an update, then started
//! again after a reboot: each start reaches the verdict the resume table gives the record it
//! finds, and a committed step is never run again nor a rollback skipped.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::boot::Outcome;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::{CrashPoint, Finished, ResumeAction, resume};
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

/// What the table's cell says the update ends as, `None` when the uninterrupted run decides.
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

fn crash_once(fixture: Fixture, at: CrashPoint, want: &Finished) {
    let what = format!("{fixture:?} killed at {at:?}");
    let rig = rig_for(fixture, &format!("sweep-{fixture:?}"));
    let dying = rig
        .daemon()
        .with_stores(crash_at(Some(at), Arc::new(AtomicU64::new(0))));
    let reply = dying.order(&update("web", "2.0.0", &d(), 1));
    assert!(
        matches!(reply, Reply::Interrupted { .. }),
        "{what}: {reply:?}"
    );
    drop(dying);
    let before = rig.launcher.steps("web").len();
    let found = rig.record("web");
    rig.reboot();
    let (daemon, report) = rig.booted();
    if let Some(Outcome::Watching) = report.services.get("web").map(|web| web.outcome.clone()) {
        let ended = daemon.ctx().watchers.wait(&name("web")).expect("a watcher");
        assert!(ended.is_ok(), "{what}: {ended:?}");
    }
    let Some(found) = found else {
        assert_eq!(report.services["web"].outcome, Outcome::Idle, "{what}");
        assert_eq!(rig.launcher.steps("web").len(), before, "{what}");
        return;
    };
    let clock = FakeClock::new();
    clock.set_boot_id("boot-2");
    let (action, _) = resume(&mut found.clone(), true, &*clock);
    assert_eq!(report.services["web"].action, Some(action), "{what}");
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
