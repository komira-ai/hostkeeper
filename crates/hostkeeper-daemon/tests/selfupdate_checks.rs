//! What stops or bounds an update of hostkeeper itself before anything is staged: other services'
//! unfinished updates, a release older than the daemon's oldest reader, the list of bad
//! versions, the verbs that never update hostkeeper; and what a confirmed self-update clears.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::checks;
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::selfupdate::{
    APT_RELEASE_BINARY, REASON_PROTOCOL_UNSUPPORTED, REASON_SELF_BINARY_MISSING, Restart,
    binary_dir,
};
use hostkeeper_daemon::verb::HookedVerb;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::record::{AwaitingRestart, RECORD_FILE, State};
use hostkeeper_steps::{BAD_VERSIONS_FILE, BootId, RecordStore, Settled};
use support::apt::open_up;
use support::selfupdate::{
    HK, NEW, OLD, SelfRig, awaiting, binary_text, put_record, record_of, refused_before_commit,
};
use support::{alerting, lines, name, snapshot};

/// An order of hostkeeper on `s`'s daemon of [`OLD`], after a record of service `a` in `state`.
fn with_a(name: &str, state: Option<State>) -> (SelfRig, Reply) {
    let s = SelfRig::new(name);
    match state {
        Some(state) => put_record(&s.rig, "a", &record_of("a", state)),
        None => fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir"),
    }
    let reply = s.daemon().order(&s.order(NEW, 1));
    (s, reply)
}

fn running() -> State {
    State::Running {
        started: hostkeeper_steps::Stamp {
            boot_id: BootId::new("a test", "b").ok(),
            mono_secs: 1,
        },
        deadline_secs: 10,
        unbounded: false,
    }
}

/// Catches the table of other services' records read wrong: a record `pending`, `running`,
/// `done` at a middle step, or written by a newer daemon is an unfinished update, and the order
/// is `busy` naming the service, with no record of hostkeeper written.
#[test]
fn a_pending_record_of_another_service_blocks_the_self_update() {
    let done = State::Done {
        code: Some(0),
        last_line: None,
    };
    for (n, state) in [State::Pending, running(), done].into_iter().enumerate() {
        let (s, reply) = with_a(&format!("su-blocked-{n}"), Some(state.clone()));
        let Reply::Refused { refusal, detail } = &reply else {
            panic!("{state:?}: {reply:?}");
        };
        assert_eq!(*refusal, Refusal::Busy, "{state:?}");
        assert!(detail.contains("step record of a"), "{detail}");
        assert!(s.rig.record(HK).is_none(), "{state:?}");
        assert!(s.hk_steps().is_empty(), "{state:?}");
        assert_eq!(s.tool_runs(), 0, "{state:?}: nothing was staged");
    }
    let s = SelfRig::new("su-blocked-newer");
    fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir");
    fs::write(s.rig.state.join("a").join(RECORD_FILE), r#"{"schema":9}"#).expect("newer");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
}

/// Catches finished updates of other services holding the self-update back: a record finished
/// with any verdict, and a state directory with no record, let the order through to its hand-over.
#[test]
fn finished_records_of_other_services_do_not() {
    let finished = |verdict: Verdict| State::Finished {
        verdict,
        reason: None,
    };
    let rows = [
        Some(finished(Verdict::Succeeded {
            promotion_pending: false,
        })),
        Some(finished(Verdict::Failed {
            phase: hostkeeper_hook::Phase::AfterCommit,
            class: Class::Environment,
        })),
        Some(finished(Verdict::Deferred {
            retry_after_secs: None,
        })),
        Some(finished(Verdict::Halt)),
        None,
    ];
    for (n, state) in rows.into_iter().enumerate() {
        let (s, reply) = with_a(&format!("su-idle-{n}"), state.clone());
        assert_eq!(reply, awaiting(false), "{state:?}");
        assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    }
}

/// Catches a record that cannot be read counted as idle: a corrupt record of another service
/// makes the order `busy`, and nothing of hostkeeper is written.
#[test]
fn an_unreadable_step_record_of_another_service_blocks_the_self_update() {
    let s = SelfRig::new("su-corrupt-other");
    fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir");
    fs::write(s.rig.state.join("a").join(RECORD_FILE), "{").expect("corrupt");
    let reply = s.daemon().order(&s.order(NEW, 1));
    let Reply::Refused { refusal, detail } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*refusal, Refusal::Busy);
    assert!(detail.contains("unreadable"), "{detail}");
    assert!(s.rig.record(HK).is_none());
}

/// Catches an update to a release the daemon cannot fall back to being staged: a release older
/// than the oldest reader (0.1.194) is `Failed` before the commit, of class release,
/// `protocol_unsupported`, with the binary directory as it was, no record and no step run; one at
/// the oldest reader is handed over to.
#[test]
fn a_release_older_than_the_oldest_reader_is_refused_before_anything_is_staged() {
    let s = SelfRig::new("su-below-reader");
    s.release("0.1.193");
    let bin = binary_dir(&s.rig.state);
    let before = snapshot(&bin);
    assert_eq!(
        s.daemon().order(&s.order("0.1.193", 1)),
        refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED)
    );
    assert_eq!(snapshot(&bin), before);
    assert!(s.rig.record(HK).is_none());
    assert!(s.hk_steps().is_empty());

    s.release("0.1.194");
    assert_eq!(s.daemon().order(&s.order("0.1.194", 1)), awaiting(false));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some("0.1.194"));
}

/// Catches a release with no hostkeeper binary being staged: it is `Failed` before the
/// commit, of class release, `self_binary_missing`, and nothing is written.
#[test]
fn a_release_without_the_binary_is_refused_before_anything_is_staged() {
    let s = SelfRig::new("su-no-binary");
    s.release_without_binary(NEW);
    assert_eq!(
        s.daemon().order(&s.order(NEW, 1)),
        refused_before_commit(Class::Release, REASON_SELF_BINARY_MISSING)
    );
    assert!(s.rig.record(HK).is_none());
}

/// Catches a list of bad versions that cannot be read taken for an empty one: an order for
/// hostkeeper is refused `bad_versions_unreadable`, naming the file, and nothing runs or is
/// written.
#[test]
fn a_garbage_bad_versions_file_refuses_self_update_naming_the_file() {
    let s = SelfRig::new("su-garbage");
    fs::create_dir_all(s.hk_state()).expect("hostkeeper's state dir");
    let path = s.hk_state().join(BAD_VERSIONS_FILE);
    fs::write(&path, "[\"1.0\"").expect("garbage");
    let reply = s.daemon().order(&s.order(NEW, 1));
    let Reply::Refused { refusal, detail } = &reply else {
        panic!("{reply:?}");
    };
    assert_eq!(*refusal, Refusal::BadVersionsUnreadable);
    assert_eq!(refusal.token(), "bad_versions_unreadable");
    assert!(detail.contains(&path.display().to_string()), "{detail}");
    assert!(s.rig.record(HK).is_none());
    assert!(s.hk_steps().is_empty());
    assert_eq!(s.tool_runs(), 0, "nothing was staged");
}

/// Catches a `force` reaching hostkeeper's own update, and a `force_rollback` of it that does
/// not take hostkeeper's own path: a `force` is refused `self_service` and writes nothing; a
/// `force_rollback` is handed over to the version it names, as an `update` is
/// (docs/design/packages-native.md, sections 10.4 and 10.6); `update` and `install` of
/// hostkeeper, and every verb of another service, pass the rule.
#[test]
fn a_force_of_the_service_hostkeeper_is_refused_and_a_force_rollback_takes_its_own_path() {
    let s = SelfRig::new("su-forced");
    let daemon = s.daemon();
    let before = snapshot(s.rig.scratch.path());
    let reply = daemon.order(&s.force_order(NEW, 1, 0));
    assert_eq!(reply.refusal(), Some(Refusal::SelfService), "{reply:?}");
    assert_eq!(snapshot(s.rig.scratch.path()), before);
    assert!(s.hk_steps().is_empty());
    assert_eq!(daemon.order(&s.rollback_order(NEW, 1)), awaiting(false));
    assert_eq!(s.layout().next().unwrap().as_deref(), Some(NEW));
    let verbs = [
        HookedVerb::Update,
        HookedVerb::ForceRollback,
        HookedVerb::Install,
        HookedVerb::Force,
    ];
    for verb in verbs {
        let forced = matches!(verb, HookedVerb::Force);
        let refused = checks::self_service(verb, &name(HK));
        assert_eq!(refused.is_err(), forced, "{verb:?}");
        assert_eq!(checks::self_service(verb, &name("web")), Ok(()));
    }
}

/// The step records of four services and their deferrals: `updater_old`, `daemon_too_old`,
/// `rebooted`, and one that succeeded.
fn deferrals(s: &SelfRig) -> Vec<(&'static str, bool)> {
    let deferred = |reason: &str| State::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(reason.to_owned()),
    };
    let rows = [
        ("old", deferred("updater_old"), true),
        ("tooold", deferred("daemon_too_old"), true),
        ("booted", deferred("rebooted"), false),
        (
            "fine",
            State::Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: false,
                },
                reason: None,
            },
            false,
        ),
    ];
    rows.into_iter()
        .map(|(service, state, cleared)| {
            put_record(&s.rig, service, &record_of(service, state));
            (service, cleared)
        })
        .collect()
}

/// A rig whose hostkeeper record waits for the restart into [`NEW`], `current` naming it.
fn waiting(name: &str) -> SelfRig {
    let s = SelfRig::new(name);
    let mut rec = record_of(
        HK,
        State::Done {
            code: Some(0),
            last_line: None,
        },
    );
    rec.target_version = NEW.to_owned();
    rec.step = Step::Cleanup;
    rec.progress.awaiting_restart = Some(AwaitingRestart {
        version: NEW.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
        next: None,
        starts: None,
    });
    put_record(&s.rig, HK, &rec);
    let new = s.rig.scratch.path().join("new-binary");
    fs::write(&new, "#!new\n").expect("the new binary");
    s.layout().stage(NEW, &new).expect("stage");
    s.layout().switch(NEW).expect("switch");
    s
}

/// Catches a confirmation that clears more or less than the deferrals of an old updater: once
/// the new binary confirms its update, the records deferred `updater_old` and `daemon_too_old`
/// are gone and `Status` shows no record for them; the record deferred for another reason and the
/// one that succeeded are byte for byte as they were; a record that cannot be read is left, and
/// alerted.
#[test]
fn a_confirmed_self_update_clears_updater_old_deferrals() {
    let s = waiting("su-clear");
    let rows = deferrals(&s);
    fs::create_dir_all(s.rig.state.join("broken")).expect("a state dir");
    fs::write(s.rig.state.join("broken").join(RECORD_FILE), "{").expect("corrupt");
    let bytes = |service: &str| fs::read(s.rig.state.join(service).join(RECORD_FILE)).ok();
    let before: Vec<_> = rows.iter().map(|(service, _)| bytes(service)).collect();
    let (daemon, alerts) = alerting(s.daemon_at(NEW));
    assert_eq!(
        daemon.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    let status = daemon.status().expect("status");
    for ((service, cleared), before) in rows.iter().zip(before) {
        let listed = status
            .services
            .iter()
            .find(|listed| listed.service.as_str() == *service)
            .expect("listed");
        match cleared {
            true => {
                assert_eq!(bytes(service), None, "{service}");
                assert_eq!(listed.record, RecordState::Absent, "{service}");
            }
            false => assert_eq!(bytes(service), before, "{service}"),
        }
    }
    assert_eq!(bytes("broken").as_deref(), Some(&b"{"[..]));
    let alerted = lines(&alerts);
    assert_eq!(alerted.len(), 1, "{alerted:?}");
    assert!(
        alerted[0].contains("alert=self_update_unsettled"),
        "{alerted:?}"
    );
    assert!(alerted[0].contains("service=broken"), "{alerted:?}");
}

/// Catches a clearing that runs whatever the start found: when the binary that starts is not the
/// new one (the update fell back), and when no update waits, every deferral stays.
#[test]
fn a_fell_back_self_update_does_not_clear_updater_old() {
    let s = waiting("su-no-clear");
    let rows = deferrals(&s);
    let bytes = |service: &str| fs::read(s.rig.state.join(service).join(RECORD_FILE)).ok();
    let before: Vec<_> = rows.iter().map(|(service, _)| bytes(service)).collect();
    let old = s.daemon_at(OLD);
    assert_eq!(
        old.settle_self_update().expect("settled"),
        Settled::FellBack
    );
    assert_eq!(
        old.settle_self_update().expect("settled"),
        Settled::NotPending
    );
    let after: Vec<_> = rows.iter().map(|(service, _)| bytes(service)).collect();
    assert_eq!(after, before);
}

/// Catches a start that settles when hostkeeper has no state directory, or that cannot list the
/// state directory and says nothing: no directory of hostkeeper is nothing to settle; a state
/// directory that is a file is an error.
#[test]
fn a_start_without_hostkeeper_state_settles_nothing() {
    let s = SelfRig::new("su-no-state");
    fs::remove_dir_all(s.hk_state()).expect("no state of hostkeeper");
    assert_eq!(
        s.daemon().settle_self_update().expect("settled"),
        Settled::NotPending
    );
    let s = SelfRig::new("su-state-file");
    fs::remove_dir_all(&s.rig.state).expect("no state dir");
    fs::write(&s.rig.state, "a file").expect("a file in its place");
    assert!(s.daemon().settle_self_update().is_err());
}

/// Catches a wait for the restart that misses a request made while it waits, or that waits when
/// it was made before: a waiter parked before the request returns once it is made, and a wait
/// after it returns at once.
#[tokio::test]
async fn a_restart_asked_while_the_server_waits_ends_the_wait() {
    let restart = Arc::new(Restart::default());
    let waiter = {
        let restart = Arc::clone(&restart);
        tokio::spawn(async move { restart.wait().await })
    };
    tokio::task::yield_now().await;
    assert!(!restart.requested());
    restart.request();
    waiter.await.expect("the waiter");
    restart.wait().await;
    assert!(restart.requested());
}

/// Catches the self-update going on when its own state cannot be used: a held halt that cannot be
/// read refuses it `halt_unreadable`, and a state directory of hostkeeper that cannot be made
/// refuses it `store_failed`; neither runs or records anything.
#[test]
fn a_self_update_whose_state_cannot_be_used_is_refused() {
    let s = SelfRig::new("su-halt-unreadable");
    fs::write(s.hk_state().join("held-halt"), "two\n").expect("a halt that cannot be read");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::HaltUnreadable), "{reply:?}");
    assert!(s.rig.record(HK).is_none());

    let s = SelfRig::new("su-state-dangling");
    fs::remove_dir_all(s.hk_state()).expect("no state of hostkeeper");
    std::os::unix::fs::symlink("nowhere", s.hk_state()).expect("a dangling link");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(s.hk_steps().is_empty());
}

/// Catches a release that changed after it was verified being handed over to or answered as a release
/// failure: when the release's binary changes while the swap waits for the step lock, the order
/// is refused `release_unreadable`, nothing is run or recorded, and a resend runs again.
#[test]
fn a_release_that_changed_before_the_hand_over_is_refused_unreadable() {
    let s = SelfRig::new("su-changed-release");
    fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir");
    let version = s.rig.packages.join(HK).join(NEW);
    let binary = version.join(APT_RELEASE_BINARY);
    let looks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seam: hostkeeper_daemon::StoreSeam = {
        let (version, binary, looks) = (version.clone(), binary.clone(), Arc::clone(&looks));
        Arc::new(move |service: &ServiceName, store: RecordStore| {
            // The second look at `a` is the one under the step lock, after the package was staged
            // and sealed.
            let looked = (service.as_str() == "a")
                .then(|| looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1);
            if looked == Some(2) {
                open_up(&version);
                fs::write(&binary, "#!changed\n").expect("change the release");
            }
            store
        })
    };
    let reply = s.daemon().with_stores(seam).order(&s.order(NEW, 1));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::ReleaseUnreadable),
        "{reply:?}"
    );
    assert!(s.hk_steps().is_empty());
    assert!(s.rig.record(HK).is_none());
    // The release as it was sealed, put back: the resend runs again.
    fs::write(&binary, binary_text(NEW)).expect("restore the release");
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
}
