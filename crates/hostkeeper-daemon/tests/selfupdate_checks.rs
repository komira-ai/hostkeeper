//! What stops or bounds an update of hostkeeper itself before anything is staged: other services'
//! unfinished updates, the protocol of the new binary and its probe, the list of bad versions, the
//! verbs that never update hostkeeper; and what a confirmed self-update clears.

mod support;

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::checks;
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::selfupdate::{
    LauncherProbe, PROBE_OUTPUT_BYTES, PROBE_TIMEOUT_SECS, PROBE_UNIT, PROTOCOL_VERB, ProbeError,
    ProtocolProbe, REASON_PROTOCOL_UNREADABLE, REASON_PROTOCOL_UNSUPPORTED,
    REASON_SELF_BINARY_MISSING, RELEASE_BINARY, Restart, SELF_UPDATE_PROTOCOL, accepted,
    binary_dir, read_protocol,
};
use hostkeeper_daemon::verb::HookedVerb;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::record::{AwaitingRestart, RECORD_FILE, State};
use hostkeeper_steps::{
    BAD_VERSIONS_FILE, LaunchError, ProbeCall, ProbeExit, RecordStore, Settled, StepCall, StepExit,
    StepLauncher,
};
use support::selfupdate::{
    HK, NEW, OLD, SelfRig, awaiting, put_record, record_of, refused_before_commit,
};
use support::{alerting, d, force, force_rollback, lines, name, snapshot};

/// An order of hostkeeper on `s`'s daemon of [`OLD`], after a record of service `a` in `state`.
fn with_a(name: &str, state: Option<State>) -> (SelfRig, Reply) {
    let s = SelfRig::new(name);
    match state {
        Some(state) => put_record(&s.rig, "a", &record_of("a", state)),
        None => fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir"),
    }
    // A protocol this daemon refuses: an order that passed every check before the probe ends
    // there, without a swap.
    s.probe.set(|| Ok(0));
    let reply = s.daemon().order(&s.order(NEW, 1));
    (s, reply)
}

fn running() -> State {
    State::Running {
        started: hostkeeper_steps::Stamp {
            boot_id: "b".to_owned(),
            mono_secs: 1,
        },
        deadline_secs: 10,
    }
}

/// Catches the table of other services' records read wrong: a record `pending`, `running`,
/// `done` at a middle step, or written by a newer daemon is an unfinished update, and the order
/// is `busy` naming the service, with no record of hostkeeper written and nothing probed.
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
        assert!(s.probe.asked().is_empty(), "{state:?}");
    }
    let s = SelfRig::new("su-blocked-newer");
    fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir");
    fs::write(s.rig.state.join("a").join(RECORD_FILE), r#"{"schema":9}"#).expect("newer");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::Busy), "{reply:?}");
}

/// Catches finished updates of other services holding the self-update back: a record finished
/// with any verdict, and a state directory with no record, let the order through to the probe.
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
        assert_eq!(
            reply,
            refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED),
            "{state:?}"
        );
        assert_eq!(s.probe.asked().len(), 1, "{state:?}");
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

/// Catches a protocol check made after staging, or a range off by one: with
/// `SELF_UPDATE_PROTOCOL` = 1, a new binary of protocol 0 or 3 is `Failed` before the commit, of
/// class release, `protocol_unsupported`, with the binary directory as it was and no record; 1 and
/// 2 are accepted and swapped.
#[test]
fn a_protocol_outside_the_accepted_range_is_refused_before_anything_is_staged() {
    assert_eq!(SELF_UPDATE_PROTOCOL, 1);
    for (protocol, ok) in [
        (0, false),
        (1, true),
        (2, true),
        (3, false),
        (u32::MAX, false),
    ] {
        assert_eq!(accepted(protocol), ok, "{protocol}");
    }
    for protocol in [0, 1, 2, 3] {
        let s = SelfRig::new(&format!("su-protocol-{protocol}"));
        s.probe.set(move || Ok(protocol));
        let bin = binary_dir(&s.rig.state);
        let before = snapshot(&bin);
        let reply = s.daemon().order(&s.order(NEW, 1));
        match accepted(protocol) {
            true => {
                assert_eq!(reply, awaiting(false), "{protocol}");
                assert_eq!(s.current().as_deref(), Some(NEW));
            }
            false => {
                assert_eq!(
                    reply,
                    refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED),
                    "{protocol}"
                );
                assert_eq!(snapshot(&bin), before, "{protocol}");
                assert!(s.rig.record(HK).is_none(), "{protocol}");
                assert!(s.hk_steps().is_empty(), "{protocol}");
            }
        }
    }
}

/// A launcher whose hook steps exit 0 and whose probes answer from a script, keeping each probe
/// call.
#[derive(Default)]
struct ProbeLauncher {
    answers: Mutex<VecDeque<Result<ProbeExit, LaunchError>>>,
    calls: Mutex<Vec<ProbeCalled>>,
}

/// One probe call, as the launcher was given it.
#[derive(Clone, Debug, PartialEq)]
struct ProbeCalled {
    service: String,
    executable: PathBuf,
    args: Vec<String>,
    unit: String,
    timeout_secs: u64,
    output_bytes: usize,
}

impl StepLauncher for ProbeLauncher {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        Ok(StepExit {
            code: Some(0),
            last_line: None,
        })
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Ok(())
    }

    fn run_probe(&self, call: &ProbeCall<'_>) -> Result<ProbeExit, LaunchError> {
        self.calls.lock().expect("calls").push(ProbeCalled {
            service: call.service.to_owned(),
            executable: call.executable.to_path_buf(),
            args: call.args.iter().map(|arg| (*arg).to_owned()).collect(),
            unit: call.unit.to_owned(),
            timeout_secs: call.timeout_secs,
            output_bytes: call.output_bytes,
        });
        self.answers
            .lock()
            .expect("answers")
            .pop_front()
            .unwrap_or_else(|| Err(LaunchError::Launcher("no answer".to_owned())))
    }
}

fn printed(code: Option<i32>, output: &str, truncated: bool) -> Result<ProbeExit, LaunchError> {
    Ok(ProbeExit {
        code,
        output: output.as_bytes().to_vec(),
        truncated,
    })
}

/// Catches a probe that is not bounded: the probe of the host asks the launcher to run
/// `<binary> self-update-protocol` for the service `hostkeeper` in its own unit, bounded to 10
/// seconds, keeping at most 64 bytes of what it prints, and reads `1`.
#[test]
fn the_protocol_probe_command_is_bounded() {
    let launcher = Arc::new(ProbeLauncher::default());
    launcher
        .answers
        .lock()
        .expect("answers")
        .push_back(printed(Some(0), "1\n", false));
    let probe = LauncherProbe::new(launcher.clone());
    let binary = Path::new("/release/bin/hostkeeper");
    assert_eq!(probe.protocol(binary).expect("read"), 1);
    assert_eq!((PROBE_TIMEOUT_SECS, PROBE_OUTPUT_BYTES), (10, 64));
    let calls = launcher.calls.lock().expect("calls").clone();
    assert_eq!(
        calls,
        [ProbeCalled {
            service: HK.to_owned(),
            executable: binary.to_path_buf(),
            args: vec![PROTOCOL_VERB.to_owned()],
            unit: PROBE_UNIT.to_owned(),
            timeout_secs: 10,
            output_bytes: 64,
        }]
    );
    assert_eq!(PROTOCOL_VERB, "self-update-protocol");
}

/// Catches a probe that cannot be read taken for an accepted protocol: a probe whose launcher
/// fails, one killed by its bound, one that exits 1, one that prints two lines, a word, nothing,
/// more than it may, or a number past `u32` gives no protocol; one line of digits, with or
/// without its newline, gives it.
#[test]
fn a_probe_reads_one_line_of_digits_and_nothing_else() {
    let rows = [
        (printed(None, "", false), false),
        (printed(Some(1), "1\n", false), false),
        (printed(Some(0), "1\n2\n", false), false),
        (printed(Some(0), "x\n", false), false),
        (printed(Some(0), "", false), false),
        (printed(Some(0), "\n", false), false),
        (printed(Some(0), "1\n", true), false),
        (printed(Some(0), "99999999999\n", false), false),
        (printed(Some(0), " 1\n", false), false),
        (printed(Some(0), "2", false), true),
        (printed(Some(0), "1\n", false), true),
    ];
    for (exit, readable) in rows {
        let exit = exit.expect("an exit");
        assert_eq!(read_protocol(&exit).is_ok(), readable, "{exit:?}");
    }
    let killed = read_protocol(&ProbeExit {
        code: None,
        output: Vec::new(),
        truncated: false,
    });
    assert!(matches!(killed, Err(ProbeError::Exit(None))), "{killed:?}");
    let exited = read_protocol(&printed(Some(1), "1\n", false).expect("an exit"));
    assert_eq!(
        exited.expect_err("exit 1").to_string(),
        "the probe ended with the status Some(1)"
    );
    let word = read_protocol(&printed(Some(0), "x\n", false).expect("an exit"));
    assert_eq!(
        word.expect_err("a word").to_string(),
        "the probe printed \"x\\n\", which is not one line of a decimal number"
    );
    let launcher = Arc::new(ProbeLauncher::default());
    let probe = LauncherProbe::new(launcher);
    let failed = probe.protocol(Path::new("/x"));
    assert!(matches!(failed, Err(ProbeError::Launch(_))), "{failed:?}");
    assert!(
        failed
            .expect_err("failed")
            .to_string()
            .contains("no answer")
    );
}

/// Catches a bad probe read as an accepted protocol through the daemon: the probe's launcher
/// failing, the probe killed by its bound, printing two lines, and printing `x` each refuse the
/// update `Failed` before the commit, of class environment, `protocol_unreadable`, and nothing is
/// staged or recorded. The step launcher of a build without probes refuses the same way.
#[test]
fn an_unreadable_protocol_is_refused_as_environment() {
    let rows = [
        Err(LaunchError::Launcher("lost".to_owned())),
        printed(None, "", false),
        printed(Some(0), "1\n2\n", false),
        printed(Some(0), "x\n", false),
    ];
    for (n, row) in rows.into_iter().enumerate() {
        let s = SelfRig::new(&format!("su-unreadable-{n}"));
        let launcher = Arc::new(ProbeLauncher::default());
        launcher.answers.lock().expect("answers").push_back(row);
        let daemon = s
            .daemon()
            .with_probe(Arc::new(LauncherProbe::new(launcher.clone())));
        let bin = binary_dir(&s.rig.state);
        let before = snapshot(&bin);
        assert_eq!(
            daemon.order(&s.order(NEW, 1)),
            refused_before_commit(Class::Environment, REASON_PROTOCOL_UNREADABLE),
            "row {n}"
        );
        assert_eq!(snapshot(&bin), before, "row {n}");
        assert!(s.rig.record(HK).is_none(), "row {n}");
        let calls = launcher.calls.lock().expect("calls").clone();
        assert_eq!(calls.len(), 1, "row {n}");
        assert!(calls[0].executable.ends_with(RELEASE_BINARY), "row {n}");
    }
    let s = SelfRig::new("su-unreadable-default");
    let daemon = Daemon::new(
        support::config(),
        s.rig.dirs(),
        s.rig.ports(),
        support::selfupdate::own(OLD),
    );
    assert_eq!(
        daemon.order(&s.order(NEW, 1)),
        refused_before_commit(Class::Environment, REASON_PROTOCOL_UNREADABLE)
    );
}

/// Catches a release with no hostkeeper binary being probed or staged: it is `Failed` before the
/// commit, of class release, `self_binary_missing`, and nothing is written.
#[test]
fn a_release_without_the_binary_is_refused_before_anything_is_staged() {
    let s = SelfRig::new("su-no-binary");
    fs::remove_file(s.rig.packages.join(HK).join(NEW).join(RELEASE_BINARY)).expect("remove");
    assert_eq!(
        s.daemon().order(&s.order(NEW, 1)),
        refused_before_commit(Class::Release, REASON_SELF_BINARY_MISSING)
    );
    assert!(s.probe.asked().is_empty());
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
    assert!(s.probe.asked().is_empty());
}

/// Catches a `force` or a `force_rollback` reaching hostkeeper's own update: both are refused
/// `self_service` and write nothing; `update` and `install` of hostkeeper, and every verb of
/// another service, pass the rule.
#[test]
fn force_and_force_rollback_of_the_service_hostkeeper_stay_refused() {
    let s = SelfRig::new("su-forced");
    let daemon = s.daemon();
    let before = snapshot(s.rig.scratch.path());
    for order in [force(HK, NEW, &d(), 1, 0), force_rollback(HK, NEW, &d(), 1)] {
        let reply = daemon.order(&order);
        assert_eq!(reply.refusal(), Some(Refusal::SelfService), "{reply:?}");
    }
    assert_eq!(snapshot(s.rig.scratch.path()), before);
    assert!(s.hk_steps().is_empty());
    let verbs = [
        HookedVerb::Update,
        HookedVerb::ForceRollback,
        HookedVerb::Install,
        HookedVerb::Force,
    ];
    for verb in verbs {
        let forced = matches!(verb, HookedVerb::Force | HookedVerb::ForceRollback);
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
/// refuses it `store_failed`; neither probes, runs or records anything.
#[test]
fn a_self_update_whose_state_cannot_be_used_is_refused() {
    let s = SelfRig::new("su-halt-unreadable");
    fs::write(s.hk_state().join("held-halt"), "two\n").expect("a halt that cannot be read");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::HaltUnreadable), "{reply:?}");
    assert!(s.rig.record(HK).is_none());
    assert!(s.probe.asked().is_empty());

    let s = SelfRig::new("su-state-dangling");
    fs::remove_dir_all(s.hk_state()).expect("no state of hostkeeper");
    std::os::unix::fs::symlink("nowhere", s.hk_state()).expect("a dangling link");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(s.probe.asked().is_empty());
    assert!(s.hk_steps().is_empty());
}

/// Catches a release that changed after it was verified being probed or answered as a release
/// failure: when the release's binary changes while the swap waits for the step lock, the order
/// is refused `release_unreadable`, nothing is probed, run or recorded, and a resend runs again.
#[test]
fn a_release_that_changed_before_the_probe_is_refused_unreadable() {
    let s = SelfRig::new("su-changed-release");
    fs::create_dir_all(s.rig.state.join("a")).expect("a's state dir");
    let binary = s.rig.packages.join(HK).join(NEW).join(RELEASE_BINARY);
    let looks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seam: hostkeeper_daemon::StoreSeam = {
        let (binary, looks) = (binary.clone(), Arc::clone(&looks));
        Arc::new(move |service: &ServiceName, store: RecordStore| {
            // The second look at `a` is the one under the step lock, after the package was sealed.
            let looked = (service.as_str() == "a")
                .then(|| looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1);
            if looked == Some(2) {
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
    assert!(s.probe.asked().is_empty());
    assert!(s.hk_steps().is_empty());
    assert!(s.rig.record(HK).is_none());
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
}
