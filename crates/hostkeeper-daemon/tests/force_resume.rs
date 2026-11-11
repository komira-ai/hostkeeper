//! A force's record as its journal (design sections 5.6, 6.5 and 8.3): a crash anywhere in a
//! force resumes it from the top of its record, a resend is answered from it, its verdict is
//! what the heartbeat reads, and a switch of `current` is followed by the prune.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::boot::{BootReport, Outcome};
use hostkeeper_daemon::testing::RecordingPrune;
use hostkeeper_daemon::{Daemon, ForcedResult, Ports, Refusal, Reply, WriteStep};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_placed::testing::{FakeUnitControl, Log as PlacedLog};
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::{CrashPoint, Finished, ForceFailure, ForcePhase, ForceVerdict};
use support::force::{HostState, UNIT, WEB, failed, host_state};
use support::{crash_at, lines};
use support_host::{NativeHost, runs, succeeded, unit};

/// `version` of `web`, with the manifest every rule accepts, published in the host's repository.
fn web(rig: &NativeHost, version: &str) {
    rig.pin(WEB, version);
}

/// `web` at 1.0.0, forced there, with 2.0.0 beside it: a target that drops a `run` unit of 1.0.0
/// and adds one.
fn installed(name: &str) -> NativeHost {
    let rig = NativeHost::new(name);
    rig.manifest(
        WEB,
        "1.0.0",
        &runs(WEB, "1.0.0", &[UNIT, &unit(WEB, "gone")]),
    );
    rig.manifest(
        WEB,
        "2.0.0",
        &runs(WEB, "2.0.0", &[UNIT, &unit(WEB, "new")]),
    );
    rig.forced(&rig.daemon(), "1.0.0", 1);
    rig
}

/// The daemon restarted on the rig's state: its start pass, then the coordinator's resend of the
/// force of 2.0.0, whose answer is returned.
fn restarted(rig: &NativeHost) -> Reply {
    let (daemon, _) = rig.booted();
    daemon.order(&rig.force(WEB, "2.0.0", 2, 0))
}

fn succeeded_now_or_before(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            ..
        }
    )
}

/// What an uninterrupted force of 2.0.0 leaves, and how many record writes it makes.
fn baseline() -> (HostState, u64) {
    let rig = installed("force-crash-base");
    let writes = Arc::new(AtomicU64::new(0));
    let daemon = rig
        .daemon()
        .with_stores(crash_at(None, Arc::clone(&writes)));
    assert_eq!(daemon.order(&rig.force(WEB, "2.0.0", 2, 0)), succeeded());
    (host_state(&rig), writes.load(Ordering::SeqCst))
}

/// Catches a part of a force that is not safe to run twice, or a record that does not let the
/// force resume: a crash before and after each of the eight writes of the force record (one per
/// phase), each followed by a restart and the resend of the order, end `Succeeded` with
/// `current`, `previous`, every link of the service manager, `placed.json` and the active units
/// as an uninterrupted force leaves them.
#[test]
fn a_forced_host_that_crashes_resumes_the_force() {
    let (expected, writes) = baseline();
    assert_eq!(writes, 8);
    for n in 1..=writes {
        for point in [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)] {
            let rig = installed("force-crash");
            let seam = crash_at(Some(point), Arc::new(AtomicU64::new(0)));
            let first = rig
                .daemon()
                .with_stores(seam)
                .order(&rig.force(WEB, "2.0.0", 2, 0));
            assert_eq!(
                first.refusal(),
                Some(Refusal::StoreFailed),
                "{point:?}: {first:?}"
            );
            let reply = restarted(&rig);
            assert!(succeeded_now_or_before(&reply), "{point:?}: {reply:?}");
            assert_eq!(host_state(&rig), expected, "{point:?}");
        }
    }
}

/// Catches a resend that runs a finished force again, or try rules a force does not keep: the
/// same order after its verdict is answered from the record with nothing placed or asked; the
/// same try for another version is `try_conflict`; a lower try is `stale_try`; and an `update`
/// with the force's try and package is `try_conflict` too.
#[test]
fn a_resend_of_a_finished_force_is_answered_from_the_record() {
    let rig = NativeHost::new("force-resend");
    let daemon = rig.daemon();
    web(&rig, "2.0.0");
    web(&rig, "2.1.0");
    rig.forced(&daemon, "2.0.0", 3);
    assert_eq!(
        daemon.order(&rig.force(WEB, "2.0.0", 3, 0)),
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            resent: true,
        }
    );
    assert_eq!(
        daemon.order(&rig.force(WEB, "2.1.0", 3, 0)),
        Reply::Refused {
            refusal: Refusal::TryConflict,
            detail: "force 2.1.0 try 3 of web; the step record holds try 3".to_owned(),
        }
    );
    let stale = daemon.order(&rig.force(WEB, "2.1.0", 2, 0));
    assert_eq!(stale.refusal(), Some(Refusal::StaleTry), "{stale:?}");
    let hooked = daemon.order(&rig.update(WEB, "2.0.0", 3));
    assert_eq!(hooked.refusal(), Some(Refusal::TryConflict), "{hooked:?}");
    assert!(rig.placed_log.events().is_empty());
    assert!(rig.launcher.runs().is_empty());
}

/// The forced verdict of `web` as the status read model has it.
fn forced_of(daemon: &Daemon) -> Option<ForcedResult> {
    let status = daemon.status().expect("the status");
    let web = status
        .services
        .iter()
        .find(|service| service.service.as_str() == WEB)
        .expect("web");
    web.forced()
}

/// Catches a force verdict the heartbeat cannot read, or reads once only: the read model gives
/// `Succeeded`, then `Failed` with `start_failed`, each with the order's id and try, on every
/// read and after a restart, until the next order; an update's verdict is not a forced one; and
/// the step record says `forced` with the order's id.
#[test]
fn force_reports_its_verdict_in_the_next_heartbeat_read() {
    let rig = NativeHost::new("force-report");
    rig.add(WEB, "0.9.0");
    web(&rig, "1.0.0");
    let two = unit(WEB, "two");
    rig.manifest(WEB, "2.0.0", &runs(WEB, "2.0.0", &[&two]));
    rig.units.fail_start(&two);
    let (daemon, _) = rig.booted();
    assert_eq!(forced_of(&daemon), None);
    assert_eq!(daemon.order(&rig.update(WEB, "0.9.0", 1)), succeeded());
    assert_eq!(forced_of(&daemon), None);
    rig.forced(&daemon, "1.0.0", 2);
    let first = ForcedResult {
        order_id: Some("force-web-1.0.0".to_owned()),
        order_try: 2,
        target: "1.0.0".to_owned(),
        verdict: ForceVerdict::Succeeded,
    };
    assert_eq!(forced_of(&daemon), Some(first.clone()));
    assert_eq!(forced_of(&daemon), Some(first.clone()));
    assert_eq!(forced_of(&rig.daemon()), Some(first));
    assert_eq!(
        daemon.order(&rig.force(WEB, "2.0.0", 3, 0)),
        failed(Phase::AfterCommit, Class::Release, "start_failed")
    );
    let second = ForcedResult {
        order_id: Some("force-web-2.0.0".to_owned()),
        order_try: 3,
        target: "2.0.0".to_owned(),
        verdict: ForceVerdict::Failed {
            reason: ForceFailure::StartFailed,
        },
    };
    assert_eq!(forced_of(&daemon), Some(second.clone()));
    assert_eq!(forced_of(&rig.daemon()), Some(second));
    let status = daemon.status().expect("the status");
    let last = status
        .services
        .iter()
        .find(|service| service.service.as_str() == WEB)
        .and_then(|service| service.last_result());
    assert_eq!(
        last,
        Some(Finished {
            verdict: Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            reason: Some("start_failed".to_owned()),
        })
    );
    let shown = rig.record(WEB).expect("the record");
    let force = shown.force.as_ref().expect("the record is of a force");
    assert_eq!(force.order_id.as_deref(), Some("force-web-2.0.0"));
    assert_eq!(force.order_try, 3);
    assert_eq!(
        shown.state,
        State::Finished {
            verdict: Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            reason: Some("start_failed".to_owned()),
        }
    );
}

fn versions(rig: &NativeHost) -> Vec<String> {
    let mut versions: Vec<String> = fs::read_dir(rig.packages.join(WEB))
        .expect("web's packages")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    versions.sort();
    versions
}

/// Catches a prune that is skipped after a switch, run without one, or allowed to change the
/// verdict (design section 4.7). The test gives the force the host's prune behind a recorder: a
/// force that moves `current` from 2.0.0 to 3.0.0 leaves 2.0.0 and 3.0.0 of three; a force to
/// the version `current` names prunes nothing; and a prune that fails leaves the force
/// `Succeeded`, with an alert.
#[test]
fn force_prunes_to_current_and_previous_only_when_current_moved() {
    let rig = NativeHost::new("force-prune");
    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        web(&rig, version);
    }
    let daemon = rig.daemon();
    rig.forced(&daemon, "1.0.0", 1);
    rig.forced(&daemon, "2.0.0", 2);
    assert_eq!(versions(&rig), ["1.0.0", "2.0.0"]);
    let pruner = RecordingPrune::new();
    let pruning = rig.daemon_on(rig.placed_fs.clone(), pruner.clone());
    assert_eq!(pruning.order(&rig.force(WEB, "3.0.0", 3, 0)), succeeded());
    assert_eq!(versions(&rig), ["2.0.0", "3.0.0"]);
    assert!(!rig.packages.join(WEB).join("1.0.0.seal").exists());
    assert_eq!(pruning.order(&rig.force(WEB, "3.0.0", 4, 0)), succeeded());
    assert_eq!(pruner.calls(), [WEB]);

    let failing = RecordingPrune::new();
    failing.fail_next("the disk refused");
    let (pruning, alerts) =
        support::alerting(rig.daemon_on(rig.placed_fs.clone(), failing.clone()));
    assert_eq!(pruning.order(&rig.force(WEB, "2.0.0", 5, 0)), succeeded());
    assert_eq!(
        lines(&alerts),
        [
            "hostkeeper daemon: level=error alert=prune_failed service=web: the old versions of web could not be pruned: the disk refused"
        ]
    );
}

/// The unfinished force record of `web` a crash after its first write leaves.
fn crashed_after_started(name: &str) -> NativeHost {
    let rig = installed(name);
    let seam = crash_at(Some(CrashPoint::AfterWrite(1)), Arc::new(AtomicU64::new(0)));
    let first = rig
        .daemon()
        .with_stores(seam)
        .order(&rig.force(WEB, "2.0.0", 2, 0));
    assert_eq!(first.refusal(), Some(Refusal::StoreFailed), "{first:?}");
    rig
}

/// Catches a force record resumed by the step runner, run past a boot id that could not be
/// written, or resumed with a target it cannot name: the start pass runs the force to
/// `Succeeded` itself; with the boot id unwritten it runs nothing and reports the journal
/// unavailable; and a record whose target is not a version is held, unresumed.
#[test]
fn the_start_pass_resumes_a_force_and_runs_no_hook_for_it() {
    let rig = crashed_after_started("force-boot");
    let (_, report) = rig.booted();
    assert_eq!(
        report.services[WEB].verdict(),
        Some(Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
        })
    );
    assert!(rig.launcher.runs().is_empty());

    let rig = crashed_after_started("force-boot-id");
    fs::create_dir_all(&rig.state).expect("the state directory");
    let daemon = rig.daemon();
    let mut report = BootReport::default();
    daemon
        .boot_pass_into(&mut report, &|step| step == WriteStep::BeforeRename)
        .expect("the start pass");
    assert!(
        matches!(report.services[WEB].outcome, Outcome::JournalUnavailable(_)),
        "{:?}",
        report.services[WEB]
    );
    assert_eq!(
        rig.record(WEB)
            .and_then(|record| record.force)
            .map(|force| force.verdict),
        Some(None)
    );

    let rig = crashed_after_started("force-boot-bad");
    let path = rig.state.join(WEB).join(RECORD_FILE);
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("the record")).expect("JSON");
    record["force"]["target_version"] = serde_json::Value::String("not/a/version".to_owned());
    fs::write(&path, serde_json::to_vec(&record).expect("JSON")).expect("the record");
    let (_, report) = rig.booted();
    assert!(
        matches!(
            &report.services[WEB].outcome,
            Outcome::Interrupted {
                refusal: Refusal::RecordUnreadable,
                ..
            }
        ),
        "{:?}",
        report.services[WEB]
    );
    assert_eq!(
        rig.record(WEB)
            .and_then(|record| record.force)
            .map(|force| force.verdict),
        Some(None)
    );
}

/// Catches a resumed force that forgets the switch it made before the crash: journaled at
/// `linked`, then run again with a service manager that refuses every call, it fails at step 9
/// and still reports the failure after the commit, and still prunes, since `current` moved.
#[test]
fn a_resumed_force_keeps_the_furthest_phase_it_reached() {
    let rig = installed("force-keep-phase");
    let seam = crash_at(Some(CrashPoint::AfterWrite(5)), Arc::new(AtomicU64::new(0)));
    let first = rig
        .daemon()
        .with_stores(seam)
        .order(&rig.force(WEB, "2.0.0", 2, 0));
    assert_eq!(first.refusal(), Some(Refusal::StoreFailed), "{first:?}");
    let pruner = RecordingPrune::new();
    let ports = Ports {
        units: Arc::new(FakeUnitControl::failing(PlacedLog::new())),
        prune: pruner.clone(),
        ..rig.ports()
    };
    let daemon = Daemon::new(support::config(), rig.dirs(), ports, support::OWN);
    assert_eq!(
        daemon.order(&rig.force(WEB, "2.0.0", 2, 0)),
        failed(Phase::AfterCommit, Class::Environment, "link_failed")
    );
    let phase = rig
        .record(WEB)
        .and_then(|record| record.force)
        .map(|force| force.phase);
    assert_eq!(phase, Some(ForcePhase::Verdicted));
    assert_eq!(pruner.calls(), [WEB]);
}
