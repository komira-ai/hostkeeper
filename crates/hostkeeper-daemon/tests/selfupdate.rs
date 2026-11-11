//! The update of hostkeeper itself through the daemon: the hand-over under the step lock, the
//! record that waits for the restart, the start that settles it, and the local list of bad
//! versions.

mod support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::cli::{self, EXIT_FAILED};
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::own_package::OwnActivate;
use hostkeeper_daemon::selfupdate::APT_RELEASE_BINARY;
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{Log, Signal, spawn};
use hostkeeper_daemon::{Daemon, Refusal, Reply, StoreSeam};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::record::{Activated, AwaitingRestart, State};
use hostkeeper_steps::{
    BAD_VERSIONS_FILE, BadVersions, CrashPoint, LaunchError, RecordStore, Settled,
};
use support::apt::{bridge, open_up};
use support::selfupdate::{
    HK, LATER, NEW, OLD, SelfRig, awaiting, fell_back, own, refused_before_commit,
};
use support::{crash_after_writing, crash_at, lines};

/// The two other services of the step lock test: dpkg package names have two letters at least.
const A: &str = "aa";
const B: &str = "bb";

fn hk_record(s: &SelfRig) -> Option<hostkeeper_steps::Record> {
    s.rig.record(HK)
}

/// The wait of the update to `version` an order of `SelfRig::order` ran: it keeps that order's
/// id, for the binary that settles it to report the verdict under, and it is a hand-over, with
/// `next` naming the version and no start yet.
fn waiting_for(version: &str) -> Option<AwaitingRestart> {
    Some(AwaitingRestart {
        next: Some(version.to_owned()),
        starts: Some(0),
        ..waiting_the_old_way(version)?
    })
}

/// The wait a release before the hand-over wrote: the order's id and no `next`.
fn waiting_the_old_way(version: &str) -> Option<AwaitingRestart> {
    Some(AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: false,
        order_id: Some(format!("order-{HK}-{version}")),
        next: None,
        starts: None,
    })
}

/// A host that ran the hand-over to [`NEW`]: its record waits for the restart with `next` naming
/// the new binary, and `current` still names the old one; the daemon of [`OLD`] that ran it is
/// returned.
fn swapped(name: &str) -> (SelfRig, Daemon) {
    let s = SelfRig::new(name);
    let daemon = s.daemon();
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    (s, daemon)
}

/// Catches the old binary reporting the update succeeded, or switching `current` itself: through
/// the executor, the package's steps run (built in: hostkeeper's package has no hook program, so
/// the launcher runs none), the new binary is staged and `next` names it while
/// `current` still names the old one, and the record waits for the restart (`done` at `cleanup`,
/// never `finished`) with `next` and a start count of 0; `Status` reports no result, the reply is
/// `Deferred` `awaiting_restart`, and the daemon is asked to exit. A resend is answered the same
/// from the record and runs nothing; an order for another version is `busy`.
#[test]
fn the_record_says_awaiting_restart_not_succeeded_until_the_new_binary_has_run() {
    let s = SelfRig::new("su-awaiting");
    s.release(LATER);
    let daemon = s.daemon();
    assert!(!daemon.ctx().restart().requested());
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    assert!(daemon.ctx().restart().requested());
    assert_eq!(s.hk_steps(), []);
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().previous().expect("previous"), None);
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    let staged = fs::read_to_string(s.layout().version_path(NEW)).expect("the staged binary");
    assert_eq!(staged, format!("#!hostkeeper {NEW}\n"));

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

    let ran = s.tool_runs();
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(true));
    let other = daemon.order(&s.order(LATER, 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy), "{other:?}");
    assert_eq!(s.tool_runs(), ran, "nothing ran again");
    assert_eq!(s.hk_steps(), []);
}

/// Catches a confirmation that is skipped, repeated, or runs work again: the start of the new
/// binary finishes the switch (`previous` the old binary, `current` the new one) and settles the
/// wait in one write, removing `next`, the record then waiting for the `activate` of the package
/// the new binary installs; a second settle writes nothing; once the `activate` ended, the record
/// is `Succeeded` and a resend of the order is answered `Succeeded` from the record with no
/// launcher call.
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
    assert_eq!(rec.progress.awaiting_restart, None);
    assert_eq!(
        rec.progress
            .activate
            .as_ref()
            .map(|pending| pending.version.clone())
            .as_deref(),
        Some(NEW)
    );
    assert!(!matches!(rec.state, State::Finished { .. }), "{rec:?}");
    assert_eq!(
        new.activate_own_package(),
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );
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
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(
        s.layout().previous().expect("previous").as_deref(),
        Some(OLD)
    );
    assert_eq!(s.layout().next().expect("next"), None);
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
    let started = start::startup(&s.rig.settings(), s.ports(), own(NEW)).expect("started");
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
    let ran = s.tool_runs();
    fs::write(s.hk_state().join(BAD_VERSIONS_FILE), "garbage").expect("garbage");
    let started = start::startup(&s.rig.settings(), s.ports(), own(OLD)).expect("started");
    let settled = started.boot_report().self_update.clone().expect("tried");
    let error = settled.expect_err("the list is garbage");
    assert!(error.contains(BAD_VERSIONS_FILE), "{error}");
    let rec = hk_record(&s).expect("the record");
    assert_eq!(rec.progress.awaiting_restart, waiting_for(NEW));
    assert!(matches!(rec.state, State::Done { .. }));
    assert_eq!(s.tool_runs(), ran, "the boot pass ran nothing of it");
    assert_eq!(s.hk_steps(), []);
    assert_eq!(
        started.boot_report().services[HK].outcome,
        hostkeeper_daemon::Outcome::Idle
    );
}

/// Catches the old binary's daemon serving on after its own update asked it to exit, or exiting
/// with a status its unit's `Restart=` does not restart: the serve returns once the restart is
/// asked for, which the command line exits with as 0; an error is its own status.
#[tokio::test]
async fn the_daemon_stops_serving_once_its_restart_is_asked() {
    let s = SelfRig::new("su-serve");
    let started = start::startup(&s.rig.settings(), s.ports(), own(OLD)).expect("started");
    let daemon = Arc::clone(started.daemon());
    let served = tokio::spawn(start::serve(started, support::idle_links()));
    daemon.ctx().restart().request();
    served.await.expect("the server task");
    assert_eq!(cli::exit_status(&Ok(())), 0);
    let failed = Err(start::StartError::NoNotifySocket);
    assert_eq!(cli::exit_status(&failed), EXIT_FAILED);
}

/// The design's named test. Catches a fallback after which the host could be ordered the same
/// broken version again: the old binary that starts after a hand-over that did not settle
/// records the update `Failed` after the commit, of class release, `self_update_fallback`, and
/// the version bad, with `current` still the old binary; a resend of the order to it runs
/// nothing and is answered with that failure.
/// (The coordinator's half, that a host reporting it is not ordered the version again, is in the
/// coordinator crate's test of the same name.)
#[test]
fn a_fallback_marks_the_version_bad_and_the_coordinator_stops_ordering_it() {
    let (s, _old) = swapped("su-fallback");
    let ran = s.tool_runs();
    let old = s.daemon_at(OLD);
    assert_eq!(
        old.settle_self_update().expect("settled"),
        Settled::FellBack
    );
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next"), None);
    let rec = hk_record(&s).expect("the record");
    let (verdict, reason) = fell_back();
    assert_eq!(rec.state, State::Finished { verdict, reason });
    let bad = BadVersions::load(&s.hk_state()).expect("the list");
    assert_eq!(bad.versions(), [NEW]);

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
    assert_eq!(s.tool_runs(), ran, "nothing ran again");
    assert_eq!(s.hk_steps(), []);
}

/// Catches a bad version held only in the step record: after the version fell back, an update to
/// another version succeeds and replaces the record, and an order for the bad version again is
/// still answered with its failure and runs nothing.
#[test]
fn a_later_order_for_another_version_does_not_forget_a_bad_version() {
    let (s, _old) = swapped("su-later");
    let old = s.daemon_at(OLD);
    assert_eq!(
        old.settle_self_update().expect("settled"),
        Settled::FellBack
    );
    s.release(LATER);
    assert_eq!(old.order(&s.order(LATER, 2)), awaiting(false));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(LATER));
    let later = s.daemon_at(LATER);
    assert_eq!(
        later.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(s.current().as_deref(), Some(LATER));
    let rec = hk_record(&s).expect("the record");
    assert_eq!(rec.target_version, LATER);
    let ran = s.tool_runs();
    let (verdict, reason) = fell_back();
    assert_eq!(
        later.order(&s.order(NEW, 3)),
        Reply::Finished {
            verdict,
            reason,
            resent: true,
        }
    );
    assert_eq!(s.tool_runs(), ran);
    assert_eq!(hk_record(&s).expect("the record").target_version, LATER);
}

/// The design's named test. Catches a swap under a running step, or a step started in the
/// middle of the swap: while a step of service `aa` runs (its `prepare` held), an order for
/// hostkeeper is `busy` and writes no record; once `aa` has finished, the next order takes the
/// step lock, and an update of service `bb` ordered while the swap holds it (the swap held at its
/// first write of the record, which it makes under the step lock) starts its first step only after
/// the swap has written the record that waits for the restart.
#[test]
fn self_update_waits_for_an_idle_step_lock() {
    let s = SelfRig::new("su-step-lock");
    for service in [A, B] {
        s.installed_service(service);
        s.publish_service(service, "2.0");
    }
    let log = Log::default();
    s.rig.launcher.log_to(log.clone());
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    let noted = log.clone();
    // The swap's first write of its record is held until `release` is sent.
    let (entered, held_swap) = Signal::new();
    let (release, released) = mpsc::channel::<()>();
    let (released, first) = (Mutex::new(released), AtomicBool::new(true));
    let seam: StoreSeam = {
        let gate = Arc::new((entered, released, first));
        Arc::new(move |service: &ServiceName, store: RecordStore| {
            if service.as_str() != HK {
                return store;
            }
            let (record, noted, gate) = (record.clone(), noted.clone(), Arc::clone(&gate));
            store.with_crash_gate(Arc::new(move |point| {
                let (entered, released, first) = &*gate;
                if matches!(point, CrashPoint::BeforeWrite(_))
                    && first.swap(false, Ordering::SeqCst)
                {
                    entered.send(());
                    released
                        .lock()
                        .expect("the release")
                        .recv()
                        .unwrap_or_default();
                }
                let waits =
                    fs::read_to_string(&record).is_ok_and(|text| text.contains("awaiting_restart"));
                if matches!(point, CrashPoint::AfterWrite(_)) & waits {
                    noted.lock().expect("the log").push("awaiting".to_owned());
                }
                false
            }))
        })
    };
    let daemon = Arc::new(s.daemon().with_stores(seam));
    let (extracted, b_extracted) = Signal::new();
    let tools = bridge(Arc::clone(&s.host));
    s.rig.launcher.answer_tools(Arc::new(move |call| {
        let answered = tools(call);
        let under_b = format!("/packages/{B}/");
        if call
            .args
            .iter()
            .any(|arg| arg.to_string_lossy().contains(&under_b))
        {
            extracted.send(());
        }
        answered
    }));

    let held = s.rig.launcher.hold(A, Step::Prepare);
    let a = {
        let (daemon, order) = (Arc::clone(&daemon), s.update_of(A, "2.0", 1));
        spawn("a's order", move || daemon.order(&order))
    };
    held.entered(&a).expect("a's prepare runs");
    let refused = daemon.order(&s.order(NEW, 1));
    assert_eq!(refused.refusal(), Some(Refusal::Busy), "{refused:?}");
    assert!(hk_record(&s).is_none());
    held.release();
    assert!(matches!(
        a.join().expect("a"),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));

    let swap = {
        let (daemon, order) = (Arc::clone(&daemon), s.order(NEW, 1));
        spawn("the swap", move || daemon.order(&order))
    };
    held_swap.wait(&swap).expect("the swap holds the step lock");
    let b = {
        let (daemon, order) = (Arc::clone(&daemon), s.update_of(B, "2.0", 1));
        spawn("b's order", move || daemon.order(&order))
    };
    // b's package is being staged under its own directory; its extraction is the last tool run
    // before b waits for the step lock.
    b_extracted.wait(&b).expect("b's package is extracted");
    release.send(()).expect("release the swap");
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
    let b_starts = at(&format!("run updater-{B}-prepare-")).expect("b ran");
    assert!(waits < b_starts, "{events:?}");
}

/// Catches a link that fails being taken for a hand-over: the follow-on stops `link_failed` with
/// `current` and `next` unchanged and the record not waiting; a resend, once the way is clear,
/// runs no step again and finishes the hand-over.
#[test]
fn a_link_that_fails_is_interrupted_and_finished_by_a_resend() {
    let s = SelfRig::new("su-link-fails");
    let blocker = s.layout().dir().join("next.tmp");
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
    let ran = s.tool_runs();
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    assert_eq!(s.tool_runs(), ran, "nothing was staged again");
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
}

/// Catches a follow-on that stops being answered as a hand-over: a record that cannot be written
/// stops it `store_failed` with the link `next` taken back, and a release whose binary changed
/// after its steps ran stops it `package_failed`; neither asks the daemon to exit.
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
    assert_eq!(s.layout().next().expect("next"), None);
    assert!(!daemon.ctx().restart().requested());

    let s = SelfRig::new("su-follow-changed");
    let version = s.rig.packages.join(HK).join(NEW);
    let binary = version.join(APT_RELEASE_BINARY);
    // The release changes once the swap's last step is done in its record, before the follow-on
    // looks at it.
    let record = s.hk_state().join(hostkeeper_steps::record::RECORD_FILE);
    let changed = Arc::new(AtomicBool::new(false));
    let seam: StoreSeam = {
        let (version, binary) = (version.clone(), binary.clone());
        Arc::new(move |service: &ServiceName, store: RecordStore| {
            if service.as_str() != HK {
                return store;
            }
            let (record, version, binary, changed) = (
                record.clone(),
                version.clone(),
                binary.clone(),
                Arc::clone(&changed),
            );
            store.with_crash_gate(Arc::new(move |point| {
                let done = fs::read_to_string(&record).is_ok_and(|text| {
                    text.contains("\"step\": \"cleanup\"") & text.contains("\"state\": \"done\"")
                });
                if matches!(point, CrashPoint::AfterWrite(_))
                    && done
                    && !changed.swap(true, Ordering::SeqCst)
                {
                    open_up(&version);
                    fs::write(&binary, "#!another binary\n").expect("change the release");
                }
                false
            }))
        })
    };
    let daemon = s.daemon().with_stores(seam);
    let reply = daemon.order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::PackageFailed), "{reply:?}");
    assert_eq!(
        fs::read_to_string(&binary).expect("the release"),
        "#!another binary\n"
    );
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

/// Catches a launcher failure of hostkeeper's own update leaving it half done: hostkeeper's steps
/// are built in, so no step of it is launched; the launcher that loses the first tool run of the
/// stage ends the order `Failed` before the commit, in the environment (`io_error`), with nothing
/// staged (no version directory, no record, no link), and the resend, the launcher working again,
/// runs it; and a crash of the daemon in the middle of the steps is resumed by the next order to
/// the same swap.
#[test]
fn hostkeeper_steps_that_stop_are_resumed_by_the_resend() {
    let s = SelfRig::new("su-lost");
    let tools = bridge(Arc::clone(&s.host));
    let lost = Arc::new(AtomicBool::new(true));
    s.rig.launcher.answer_tools(Arc::new(move |call| {
        match lost.swap(false, Ordering::SeqCst) {
            true => Err(LaunchError::Launcher("the tool run was lost".to_owned())),
            false => tools(call),
        }
    }));
    let daemon = s.daemon();
    let reply = daemon.order(&s.order(NEW, 1));
    assert_eq!(reply, refused_before_commit(Class::Environment, "io_error"));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert!(!s.rig.packages.join(HK).join(NEW).exists());
    assert!(hk_record(&s).is_none());
    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));

    let s = SelfRig::new("su-crash-steps");
    let count = Arc::new(AtomicU64::new(0));
    let crashed = s
        .daemon()
        .with_stores(crash_at(Some(CrashPoint::AfterWrite(3)), count));
    assert!(crashed.order(&s.order(NEW, 1)).refusal().is_some());
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
}
