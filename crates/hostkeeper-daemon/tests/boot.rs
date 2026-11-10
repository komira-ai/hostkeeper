//! The start pass (design section 6.5): each cell of the resume table end to end through the
//! daemon, the boot id written only after every record was read, held records, a record that
//! cannot be written, a release changed under an unfinished update, and the order of the start.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::boot::{
    self, ALERT_JOURNAL, BOOT_ID_FILE, BootReport, BootStep, Outcome, ServiceBoot, Stage,
    alert_line, boot_changed, commit_boot,
};
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{FakeNotifier, Log};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Refusal, Reply, WriteStep};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::{BootId, CrashPoint, Finished, RecordStore, ResumeAction};
use support::{
    IRREVERSIBLE, OWN, alerting, crash_after_writing, crash_at, d, finished, lines, name, ok,
};
use support_apt::open_up;
use support_host::NativeHost;

fn failed(phase: Phase) -> Finished {
    finished(
        Verdict::Failed {
            phase,
            class: Class::Release,
        },
        None,
    )
}

fn rebooted_deferred() -> Finished {
    finished(
        Verdict::Deferred {
            retry_after_secs: None,
        },
        Some("rebooted"),
    )
}

/// A booted daemon runs `web`'s update to 2.0.0 until the launcher loses the run of `step`: the
/// daemon dies with that step's unit alive, its record `running`.
fn lost_in(rig: &NativeHost, step: Step) {
    rig.add_staged("web", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("web", step);
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed), "{reply:?}");
}

/// A new daemon's start pass, with the launcher's calls from now on in a log.
fn restart(rig: &NativeHost) -> (Daemon, BootReport, Log) {
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let (daemon, report) = rig.booted();
    (daemon, report, log)
}

fn calls(log: &Log) -> Vec<String> {
    log.lock().expect("the log").clone()
}

fn web(report: &BootReport) -> &ServiceBoot {
    &report.services["web"]
}

/// Catches a restart in the same boot that runs a step again over its live unit, or skips it:
/// the unit is stopped by name, then the step runs again, and the update ends as it would have.
#[test]
fn a_daemon_restart_in_the_same_boot_stops_the_unit_and_reruns_the_step() {
    let rig = NativeHost::new("boot-same-boot");
    lost_in(&rig, Step::Prepare);
    let (_, report, log) = restart(&rig);
    assert!(!report.rebooted);
    assert_eq!(web(&report).action, Some(ResumeAction::StopUnitAndRerun));
    assert_eq!(web(&report).verdict(), Some(ok()));
    assert_eq!(
        calls(&log),
        [
            "stop updater-web-prepare-1",
            "run updater-web-prepare-1",
            "run updater-web-cutover-1",
            "run updater-web-verify-1",
            "run updater-web-cleanup-1",
        ]
    );
}

/// Catches a `prepare` cut off by a reboot that is run again or reported as a failure: it is
/// rolled back, and the update is `Deferred` with the reason `rebooted`.
#[test]
fn a_reboot_during_prepare_rolls_back_and_reports_deferred_rebooted() {
    let rig = NativeHost::new("boot-prepare-reboot");
    lost_in(&rig, Step::Prepare);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert!(report.rebooted);
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::RollbackThenDeferred { reason: "rebooted" })
    );
    assert_eq!(web(&report).verdict(), Some(rebooted_deferred()));
    assert_eq!(rig.verdict("web"), Some(rebooted_deferred()));
    assert_eq!(calls(&log), ["run updater-web-rollback-1"]);
}

/// Catches a `cutover` cut off by a reboot before the commit that is run again, or anything
/// started before its rollback: `rollback` is the one call to the launcher, and the update is
/// `Failed(before_commit)`.
#[test]
fn a_reboot_before_commit_during_cutover_rolls_back_before_any_unit_starts() {
    let rig = NativeHost::new("boot-cutover-reboot");
    lost_in(&rig, Step::Cutover);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::RollbackThenFailed {
            phase: Phase::BeforeCommit
        })
    );
    assert_eq!(web(&report).verdict(), Some(failed(Phase::BeforeCommit)));
    assert_eq!(calls(&log), ["run updater-web-rollback-1"]);
}

/// Catches a committed update undone by a reboot, or its cut-off step run again: after `prepare`
/// said `reversible: false`, a `cutover` cut off by the reboot is over (the reboot restarted the
/// new release) and the update goes on with `verify`; nothing is rolled back.
#[test]
fn a_reboot_after_commit_continues_with_the_next_step() {
    let rig = NativeHost::new("boot-commit-reboot");
    rig.launcher
        .replies("web", Step::Prepare, &[(0, IRREVERSIBLE)]);
    lost_in(&rig, Step::Cutover);
    assert!(rig.record("web").expect("a record").progress.committed);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::RestartNewReleaseContinueNext)
    );
    assert_eq!(web(&report).verdict(), Some(ok()));
    assert_eq!(
        calls(&log),
        ["run updater-web-verify-1", "run updater-web-cleanup-1"]
    );
}

/// Catches a `rollback` or `cleanup` cut off by a reboot that is skipped, or read as a failure:
/// each runs again, and the update ends as it would have.
#[test]
fn a_reboot_during_rollback_or_cleanup_runs_it_again() {
    let rig = NativeHost::new("boot-rollback-reboot");
    rig.launcher.exits("web", Step::Verify, &[1]);
    lost_in(&rig, Step::Rollback);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert_eq!(web(&report).action, Some(ResumeAction::RerunStep));
    assert_eq!(web(&report).verdict(), Some(failed(Phase::GateRelease)));
    assert_eq!(calls(&log), ["run updater-web-rollback-1"]);

    let rig = NativeHost::new("boot-cleanup-reboot");
    lost_in(&rig, Step::Cleanup);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert_eq!(web(&report).action, Some(ResumeAction::RerunStep));
    assert_eq!(web(&report).verdict(), Some(ok()));
    assert_eq!(calls(&log), ["run updater-web-cleanup-1"]);
}

/// Catches the time of another boot counted against a committed update's verify window: the
/// clock is far past the old deadline, and `verify` still runs again with a whole window and the
/// update succeeds.
#[test]
fn the_verify_window_restarts_after_a_reboot() {
    let rig = NativeHost::new("boot-verify-window");
    rig.launcher
        .replies("web", Step::Cutover, &[(0, IRREVERSIBLE)]);
    lost_in(&rig, Step::Verify);
    rig.clock.advance(10_000);
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::ContinueVerifyFreshWindow)
    );
    assert_eq!(web(&report).verdict(), Some(ok()));
    assert_eq!(
        calls(&log),
        ["run updater-web-verify-1", "run updater-web-cleanup-1"]
    );
    let began = rig.record("web").expect("a record").progress.verify_started;
    assert_eq!(
        began
            .and_then(|stamp| stamp.boot_id)
            .as_ref()
            .map(BootId::as_str),
        Some("boot-2")
    );
}

/// Catches a resend answered by running the update again after the daemon died between its
/// verdict and the reply: the start pass ends the update and dies after writing `Finished`; the
/// next daemon runs nothing at its start, and the same order, same try, is answered with the
/// recorded verdict.
#[test]
fn resend_after_a_death_during_the_boot_pass_returns_the_recorded_verdict() {
    let rig = NativeHost::new("boot-resend");
    lost_in(&rig, Step::Prepare);
    let record = rig.state.join("web").join(RECORD_FILE);
    let dying = rig
        .daemon()
        .with_stores(crash_after_writing(record, r#""state": "finished""#));
    let report = dying.boot_pass().expect("the start pass");
    assert!(matches!(
        web(&report).outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert_eq!(rig.verdict("web"), Some(ok()));
    drop(dying);
    let (daemon, report, log) = restart(&rig);
    assert_eq!(web(&report).outcome, Outcome::Idle);
    let reply = daemon.order(&rig.update("web", "2.0.0", 1));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: true
        }
    );
    assert!(calls(&log).is_empty(), "{:?}", calls(&log));
}

/// `alpha` and `beta` each have an unfinished update: `alpha` lost in `prepare`, `beta` in
/// `cutover`.
fn two_unfinished(rig: &NativeHost) {
    rig.add_staged("alpha", "2.0.0");
    rig.add_staged("beta", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("alpha", Step::Prepare);
    rig.launcher.lose("beta", Step::Cutover);
    for service in ["alpha", "beta"] {
        let reply = daemon.order(&rig.update(service, "2.0.0", 1));
        assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed));
    }
}

fn boot_id(rig: &NativeHost) -> String {
    fs::read_to_string(rig.state.join(BOOT_ID_FILE)).expect("the boot id")
}

/// Catches the boot id written before every record was read: after a boot, `beta`'s record
/// cannot be read; the boot id keeps the old boot, no hook runs for any service, and the next
/// start still reads both records as after a boot.
#[test]
fn the_new_boot_id_is_written_only_after_every_record_was_read() {
    let rig = NativeHost::new("boot-unread");
    two_unfinished(&rig);
    let record = rig.state.join("beta").join(RECORD_FILE);
    let aside = rig.state.join("beta").join("aside.json");
    fs::rename(&record, &aside).expect("move the record aside");
    fs::create_dir(&record).expect("a directory in its place");
    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert!(matches!(
        report.services["beta"].outcome,
        Outcome::Unreadable(_)
    ));
    assert!(matches!(
        report.services["alpha"].outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert_eq!(
        report.services["alpha"].verdict(),
        Some(boot::journal_unavailable())
    );
    assert!(!report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-1");
    assert!(calls(&log).is_empty(), "{:?}", calls(&log));

    fs::remove_dir(&record).expect("remove the directory");
    fs::rename(&aside, &record).expect("put the record back");
    let (_, report, log) = restart(&rig);
    assert!(report.rebooted);
    assert!(report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-2");
    assert_eq!(
        report.services["alpha"].verdict(),
        Some(rebooted_deferred())
    );
    assert_eq!(
        report.services["beta"].verdict(),
        Some(failed(Phase::BeforeCommit))
    );
    assert_eq!(
        calls(&log),
        [
            "run updater-alpha-rollback-1",
            "run updater-beta-rollback-1"
        ]
    );
}

/// Catches a boot id taken as written when the daemon died writing it: stopped before the
/// rename, the old id stays, no hook runs, and the next start reads the records as after a boot;
/// stopped after the rename, the new id is durable and the next start reads them as after a
/// death of the daemon.
#[test]
fn a_daemon_killed_between_reading_records_and_writing_the_boot_id_reads_them_as_after_a_boot() {
    let rig = NativeHost::new("boot-killed-before-id");
    lost_in(&rig, Step::Prepare);
    rig.reboot();
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let mut report = BootReport::default();
    rig.daemon()
        .boot_pass_into(&mut report, &|step| step == WriteStep::BeforeRename)
        .expect("the start pass");
    assert!(!report.boot_id_written);
    assert!(matches!(
        web(&report).outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert_eq!(boot_id(&rig), "boot-1");
    assert!(calls(&log).is_empty());
    let (_, report, log) = restart(&rig);
    assert!(report.rebooted);
    assert_eq!(web(&report).verdict(), Some(rebooted_deferred()));
    assert_eq!(calls(&log), ["run updater-web-rollback-1"]);

    let rig = NativeHost::new("boot-killed-after-id");
    lost_in(&rig, Step::Cutover);
    rig.reboot();
    let mut report = BootReport::default();
    rig.daemon()
        .boot_pass_into(&mut report, &|step| step == WriteStep::AfterRename)
        .expect("the start pass");
    assert!(!report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-2");
    let (_, report, _) = restart(&rig);
    assert!(!report.rebooted);
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::RecomputeNext),
        "the cut-off cutover was read as killed before the death"
    );
    assert_eq!(web(&report).verdict(), Some(failed(Phase::BeforeCommit)));
}

/// Catches a corrupt record that stops the daemon or the other services: `alpha` is held as
/// corrupt, `beta` runs to its verdict, and the readiness notice goes out.
#[test]
fn a_corrupt_record_holds_only_its_service() {
    let rig = NativeHost::new("boot-corrupt");
    two_unfinished(&rig);
    fs::write(rig.state.join("alpha").join(RECORD_FILE), "{").expect("corrupt it");
    let log = Log::default();
    let notifier = FakeNotifier::new(log.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let report = started.boot_report();
    assert!(matches!(
        report.services["alpha"].outcome,
        Outcome::Corrupt(_)
    ));
    assert_eq!(report.services["alpha"].boot_step, BootStep::Held);
    assert_eq!(report.services["beta"].verdict(), Some(ok()));
    assert_eq!(notifier.sent(), 1);
    let reply = started.daemon().order(&rig.update("alpha", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::RecordUnreadable));
}

/// Catches a hook run for a service whose record cannot be written: the rewrite of `web`'s
/// record fails with a full disk; nothing of `web` is launched (no step, no rollback, no boot
/// step), its update is `Deferred` `journal_unavailable`, and the daemon still comes ready.
#[test]
fn a_record_that_cannot_be_written_runs_no_hook() {
    let rig = NativeHost::new("boot-enospc");
    let restore = support::manifest_with("web", "1.0.0", r#"boot = "restore""#);
    rig.add_with("web", "1.0.0", &restore);
    rig.store.link("web", "current", "1.0.0");
    lost_in(&rig, Step::Prepare);
    rig.reboot();
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let full = Arc::new(|_: u64| Some(io::Error::from_raw_os_error(28)));
    let (daemon, alerts) = alerting(rig.daemon().with_stores(Arc::new(
        move |_: &ServiceName, store: RecordStore| store.with_write_fault(full.clone()),
    )));
    let mut report = daemon.boot_pass().expect("the start pass");
    let notifier = FakeNotifier::new(log.clone());
    daemon.after_boot(&mut report, &*notifier);
    assert_eq!(
        lines(&alerts),
        [
            "hostkeeper daemon: level=error alert=journal_unavailable service=web: cannot use the state directory: No space left on device (os error 28)",
            "hostkeeper daemon: level=error alert=journal_unavailable the boot id is not written: a step record could not be read or rewritten, so the old boot id is kept; no hook runs in this start",
        ]
    );
    assert!(matches!(
        web(&report).outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert_eq!(
        web(&report).verdict(),
        Some(finished(
            Verdict::Deferred {
                retry_after_secs: None
            },
            Some("journal_unavailable")
        ))
    );
    assert_eq!(web(&report).boot_step, BootStep::Held);
    assert!(!report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-1");
    assert_eq!(calls(&log), ["ready"]);
    assert!(rig.launcher.additive_runs().is_empty());
    assert_eq!(report.ready, Some(Ok(())));
}

/// Catches a boot id written with anything but the id, a written id read as a boot, or a newer
/// record that stops the daemon: the file holds the id alone; a file with the id and a newline,
/// as one written by hand, is the same boot; a missing one is a boot; a record of schema 2 is
/// held as `record_newer`, unchanged, and the boot id is written all the same.
#[test]
fn the_boot_id_file_is_a_bare_id_and_an_unknown_future_record_schema_is_held() {
    let rig = NativeHost::new("boot-id-file");
    assert!(boot_changed(&rig.state, &*rig.clock));
    fs::create_dir_all(&rig.state).expect("the state dir");
    commit_boot(&rig.state, &*rig.clock).expect("write the boot id");
    assert_eq!(
        fs::read(rig.state.join(BOOT_ID_FILE)).expect("read"),
        b"boot-1"
    );
    fs::write(rig.state.join(BOOT_ID_FILE), "boot-1\n").expect("a recorded fixture");
    assert!(!boot_changed(&rig.state, &*rig.clock));
    rig.reboot();
    assert!(boot_changed(&rig.state, &*rig.clock));

    let newer = r#"{"schema": 2, "service": "web", "a_field_of_schema_2": true}"#;
    fs::create_dir_all(rig.state.join("web")).expect("web's dir");
    fs::write(rig.state.join("web").join(RECORD_FILE), newer).expect("a newer record");
    let (_, report, _) = restart(&rig);
    assert_eq!(web(&report).outcome, Outcome::RecordNewer(2));
    assert!(report.boot_id_written);
    let kept = fs::read_to_string(rig.state.join("web").join(RECORD_FILE)).expect("read");
    assert_eq!(kept, newer);
}

/// Catches an unreadable boot id read as the same boot, or written as an empty one: the start
/// pass reads a changed id as a boot and the same id as none, and a clock whose boot id cannot be
/// read as a boot whatever the file holds, or when there is no file; the boot id is then not
/// written, and the file keeps the old id.
#[test]
fn an_unreadable_boot_id_is_a_boot_and_is_never_written() {
    let rig = NativeHost::new("boot-id-unreadable");
    fs::create_dir_all(&rig.state).expect("the state dir");
    assert!(boot_changed(&rig.state, &*rig.clock));
    rig.clock.lose_boot_id();
    assert!(boot_changed(&rig.state, &*rig.clock));
    rig.clock.set_boot_id("boot-1");
    commit_boot(&rig.state, &*rig.clock).expect("write the boot id");
    assert!(!boot_changed(&rig.state, &*rig.clock));
    rig.clock.set_boot_id("boot-2");
    assert!(boot_changed(&rig.state, &*rig.clock));

    rig.clock.lose_boot_id();
    assert!(boot_changed(&rig.state, &*rig.clock));
    let error = commit_boot(&rig.state, &*rig.clock).expect_err("no boot id to write");
    assert_eq!(
        error.to_string(),
        "the boot id read from the fake clock is empty"
    );
    assert_eq!(boot_id(&rig), "boot-1");
    assert!(!rig.state.join(format!("{BOOT_ID_FILE}.tmp")).exists());
}

/// Catches a Mac that lost power mid-update read as not rebooted because its boot id cannot be
/// read: the start pass reports a reboot, alerts that the boot id is not written, and runs no
/// hook; once the id can be read again and is another boot's, the next start reads the record as
/// after a boot, rolls the cut-off `prepare` back and reports `Deferred` `rebooted`.
#[test]
fn a_start_whose_boot_id_cannot_be_read_reports_a_reboot_and_runs_no_hook() {
    let rig = NativeHost::new("boot-id-unknown-start");
    lost_in(&rig, Step::Prepare);
    rig.clock.lose_boot_id();
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let (daemon, alerts) = alerting(rig.daemon());
    let report = daemon.boot_pass().expect("the start pass");
    assert!(report.rebooted);
    assert!(!report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-1");
    assert_eq!(
        lines(&alerts),
        [
            "hostkeeper daemon: level=error alert=journal_unavailable the boot id is not written: the boot id read from the fake clock is empty; no hook runs in this start",
            "hostkeeper daemon: level=error alert=journal_unavailable service=web: the boot id is not written: the boot id read from the fake clock is empty",
        ]
    );
    assert_eq!(
        web(&report).action,
        Some(ResumeAction::RollbackThenDeferred { reason: "rebooted" })
    );
    assert!(matches!(
        web(&report).outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert!(calls(&log).is_empty(), "{:?}", calls(&log));

    rig.reboot();
    let (_, report, log) = restart(&rig);
    assert!(report.rebooted);
    assert!(report.boot_id_written);
    assert_eq!(boot_id(&rig), "boot-2");
    assert_eq!(web(&report).verdict(), Some(rebooted_deferred()));
    assert_eq!(rig.verdict("web"), Some(rebooted_deferred()));
    assert_eq!(calls(&log), ["run updater-web-rollback-1"]);
}

/// Catches a start whose parts run out of order: the registration socket and the placed links
/// first, then the records, the boot id, the uninstalls, the updates, the watchers, the boot steps
/// and the readiness notice.
#[test]
fn the_start_runs_its_parts_in_order_with_before_records_first() {
    let rig = NativeHost::new("boot-order");
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(
        started.boot_report().stages,
        [
            Stage::UnlinkRegistrationSocket,
            Stage::ClearPolicyRcD,
            Stage::BeforeRecords,
            Stage::ReadRecords,
            Stage::WriteBootId,
            Stage::ResumeUninstalls,
            Stage::RunUpdates,
            Stage::RestartWatchers,
            Stage::StartRunUnits,
            Stage::BootSteps,
            Stage::Ready,
        ]
    );
    assert_eq!(started.boot_report().ready, Some(Ok(())));
}

/// Catches a release changed under an unfinished update being run, or the update lost: the tree
/// of 2.0.0 no longer has the digest the record verified, so the update halts with
/// `artifact_changed` after its unit is stopped, and nothing of it runs; when the unit cannot be
/// stopped, the update is left as it was.
#[test]
fn a_release_changed_under_an_unfinished_update_halts_it() {
    let rig = NativeHost::new("boot-changed");
    lost_in(&rig, Step::Cutover);
    open_up(&rig.packages.join("web/2.0.0"));
    fs::write(rig.packages.join("web/2.0.0/extra"), "x").expect("change the tree");
    rig.launcher.fail_stops("web");
    let (_, report, log) = restart(&rig);
    assert!(matches!(
        web(&report).outcome,
        Outcome::Interrupted {
            refusal: Refusal::LauncherFailed,
            ..
        }
    ));
    assert_eq!(calls(&log), ["stop updater-web-cutover-1"]);
    assert_eq!(rig.verdict("web"), None);

    rig.launcher.allow_stops("web");
    let (_, report, log) = restart(&rig);
    let halted = finished(Verdict::Halt, Some("artifact_changed"));
    assert_eq!(web(&report).verdict(), Some(halted.clone()));
    assert_eq!(rig.verdict("web"), Some(halted));
    assert_eq!(calls(&log), ["stop updater-web-cutover-1"]);
}

/// Catches an update resumed without what it needs: with its params file gone, or its package
/// directory gone, or under a daemon older than its manifest asks for, the update is held as
/// unresumable and nothing of it runs.
#[test]
fn an_update_that_cannot_be_rebuilt_is_held() {
    let rig = NativeHost::new("boot-no-params");
    lost_in(&rig, Step::Prepare);
    fs::remove_file(rig.state.join("web/params.toml")).expect("remove the params");
    let (_, report, log) = restart(&rig);
    assert!(matches!(web(&report).outcome, Outcome::Unresumable(_)));
    assert!(calls(&log).is_empty());

    let rig = NativeHost::new("boot-no-package");
    lost_in(&rig, Step::Prepare);
    open_up(&rig.packages.join("web/2.0.0"));
    fs::remove_dir_all(rig.packages.join("web/2.0.0")).expect("remove the package");
    let (_, report, _) = restart(&rig);
    assert!(matches!(web(&report).outcome, Outcome::Unresumable(_)));

    let rig = NativeHost::new("boot-old-daemon");
    lost_in(&rig, Step::Prepare);
    let old = hostkeeper_config::manifest::HostkeeperVersion {
        major: 0,
        minor: 0,
        patch: 1,
    };
    let daemon = Daemon::new(support::config(), rig.dirs(), rig.ports(), old);
    let report = daemon.boot_pass().expect("the start pass");
    assert!(matches!(web(&report).outcome, Outcome::Unresumable(_)));
    assert!(
        rig.launcher.steps("web").len() == 1,
        "only the lost prepare ran"
    );
}

/// Catches a start pass that runs an update whose service is locked: the update is left as it
/// is and reported busy.
#[test]
fn an_update_whose_service_is_locked_at_the_start_is_left_busy() {
    let rig = NativeHost::new("boot-locked");
    lost_in(&rig, Step::Prepare);
    let daemon = rig.daemon();
    let package = Package {
        verb: HookedVerb::ForceRollback,
        version: hostkeeper_config::names::Version::new("1.0.0").expect("version"),
        digest: PackageDigest::new(&d()).expect("digest"),
    };
    let _held = daemon
        .ctx()
        .locks
        .take(&name("web"), package)
        .expect("the lock");
    let report = daemon.boot_pass().expect("the start pass");
    let Outcome::Interrupted { refusal, detail } = &web(&report).outcome else {
        panic!("{:?}", web(&report))
    };
    assert_eq!(*refusal, Refusal::Busy);
    assert_eq!(
        detail,
        "service=web: locked by an order to force_rollback it to 1.0.0"
    );
}

/// Catches a start pass that goes on without its records, or one that stops at a package
/// directory it cannot list: a state directory that is not a directory is an error naming it;
/// a package directory that cannot be listed runs no boot step, and the notice still goes out.
#[test]
fn what_the_start_cannot_list_is_an_error_or_an_alert() {
    let rig = NativeHost::new("boot-unlisted");
    fs::write(&rig.state, "not a directory").expect("a file");
    let error = rig.daemon().boot_pass().expect_err("refused");
    assert!(
        error.to_string().starts_with(&format!(
            "cannot list the step records under {}",
            rig.state.display()
        )),
        "{error}"
    );

    let rig = NativeHost::new("boot-packages-unlisted");
    fs::remove_dir(&rig.packages).expect("the empty packages directory");
    fs::write(&rig.packages, "not a directory").expect("a file");
    let (daemon, mut report) = rig.booted();
    let notifier = FakeNotifier::failing();
    daemon.after_boot(&mut report, &*notifier);
    assert!(report.services.is_empty());
    assert_eq!(notifier.sent(), 1);
    assert_eq!(
        report.ready,
        Some(Err("the service manager is gone".to_owned()))
    );
}

/// Catches an alert that a log search for `alert=` would miss, or that names no level: the line
/// is exactly the daemon's prefix, `level=error`, the `alert=` field and the detail.
#[test]
fn an_alert_is_one_error_line_with_its_alert_field() {
    assert_eq!(
        alert_line(ALERT_JOURNAL, "service=web: disk full"),
        "hostkeeper daemon: level=error alert=journal_unavailable service=web: disk full"
    );
}

/// Catches a seam that changes the daemon's own record writes outside a test: with a gate that
/// stops nothing, an update runs to its verdict and every write is counted.
#[test]
fn a_store_seam_that_stops_nothing_changes_nothing() {
    let rig = NativeHost::new("boot-seam");
    rig.add_staged("web", "2.0.0");
    let count = Arc::new(AtomicU64::new(0));
    let daemon = rig.daemon().with_stores(crash_at(None, count.clone()));
    assert_eq!(
        daemon.order(&rig.update("web", "2.0.0", 1)),
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: false
        }
    );
    // The first record, then `pending`, `running` and `done` for each of the four steps but the
    // first's `pending`, then the verdict.
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 13);
    assert!(matches!(
        rig.record("web").expect("a record").state,
        State::Finished { .. }
    ));
}

/// Catches a release that is not even a tree of plain files being run, or its halt lost when the
/// record cannot be written: a link in 2.0.0 halts the update with `artifact_changed`; when the
/// halt cannot be written either, no hook runs and the update is `journal_unavailable`.
#[test]
fn a_release_with_a_link_in_it_halts_the_update_and_a_halt_needs_its_record() {
    let rig = NativeHost::new("boot-link");
    lost_in(&rig, Step::Cutover);
    open_up(&rig.packages.join("web/2.0.0"));
    std::os::unix::fs::symlink("nowhere", rig.packages.join("web/2.0.0/link")).expect("a link");
    let (_, report, log) = restart(&rig);
    let halted = finished(Verdict::Halt, Some("artifact_changed"));
    assert_eq!(web(&report).verdict(), Some(halted));
    assert_eq!(calls(&log), ["stop updater-web-cutover-1"]);

    let rig = NativeHost::new("boot-halt-unwritten");
    lost_in(&rig, Step::Prepare);
    open_up(&rig.packages.join("web/2.0.0"));
    fs::write(rig.packages.join("web/2.0.0/extra"), "x").expect("change the tree");
    let full = Arc::new(|_: u64| Some(io::Error::from_raw_os_error(28)));
    let daemon = rig
        .daemon()
        .with_stores(Arc::new(move |_: &ServiceName, store: RecordStore| {
            store.with_write_fault(full.clone())
        }));
    let report = daemon.boot_pass().expect("the start pass");
    assert!(matches!(
        web(&report).outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert_eq!(rig.verdict("web"), None);
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
}

/// Catches the daemon's own file taking the place of a service: a service named `boot-id` has
/// its directory and record read as any other, the boot id is written beside it, and a new
/// update of that service runs.
#[test]
fn a_service_named_boot_id_keeps_its_directory_and_takes_orders() {
    let rig = NativeHost::new("boot-named-boot-id");
    rig.add_staged("boot-id", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("boot-id", Step::Prepare);
    let reply = daemon.order(&rig.update("boot-id", "2.0.0", 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed));
    let (daemon, report, _) = restart(&rig);
    assert_eq!(report.services["boot-id"].verdict(), Some(ok()));
    assert!(report.boot_id_written);
    assert!(rig.state.join("boot-id").is_dir());
    assert_eq!(boot_id(&rig), "boot-1");

    rig.add_staged("boot-id", "3.0.0");
    let reply = daemon.order(&rig.update("boot-id", "3.0.0", 2));
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: ok().verdict,
            reason: None,
            resent: false
        }
    );
}

/// Catches a daemon killed between the rewrites of two services that writes the boot id, or
/// reads one record twice as after a boot: the first record is rewritten and the daemon dies;
/// the boot id stays, and the next start reads both as after a boot, each reaching its cell's
/// verdict.
#[test]
fn a_daemon_killed_between_two_rewrites_reads_both_as_after_a_boot() {
    let rig = NativeHost::new("boot-two-rewrites");
    two_unfinished(&rig);
    rig.reboot();
    let dying = rig.daemon().with_stores(crash_at(
        Some(CrashPoint::AfterWrite(1)),
        Arc::new(AtomicU64::new(0)),
    ));
    let report = dying.boot_pass().expect("the start pass");
    assert!(matches!(
        report.services["beta"].outcome,
        Outcome::JournalUnavailable(_)
    ));
    assert!(!report.boot_id_written);
    drop(dying);
    let (_, report, log) = restart(&rig);
    assert!(report.rebooted);
    assert_eq!(
        report.services["alpha"].action,
        Some(ResumeAction::RerunStep)
    );
    assert_eq!(
        report.services["alpha"].verdict(),
        Some(rebooted_deferred())
    );
    assert_eq!(
        report.services["beta"].verdict(),
        Some(failed(Phase::BeforeCommit))
    );
    assert_eq!(
        calls(&log),
        [
            "run updater-alpha-rollback-1",
            "run updater-beta-rollback-1"
        ]
    );
}

/// Catches a stale temporary file of the boot id that stops every later start, is followed, or
/// leaves hooks running when it cannot be cleared: a stale file and a stale link are replaced
/// (the link's target untouched); a stale directory, and a directory where the boot id goes,
/// keep the old boot id, no hook runs, the readiness notice still goes out, and once cleared the
/// next start reads the records as after a boot.
#[test]
fn stale_entries_of_the_boot_id_are_cleared_or_keep_the_old_boot_id() {
    let tmp = format!("{BOOT_ID_FILE}.tmp");
    let rig = NativeHost::new("boot-stale-file");
    fs::create_dir_all(&rig.state).expect("the state dir");
    fs::write(rig.state.join(&tmp), "stale").expect("a stale file");
    let target = rig.scratch.path().join("target");
    fs::write(&target, "kept").expect("a target");
    let (_, report) = rig.booted();
    assert!(report.boot_id_written);
    let rig2 = NativeHost::new("boot-stale-link");
    fs::create_dir_all(&rig2.state).expect("the state dir");
    std::os::unix::fs::symlink(&target, rig2.state.join(&tmp)).expect("a stale link");
    let (_, report) = rig2.booted();
    assert!(report.boot_id_written);
    assert_eq!(fs::read_to_string(&target).expect("the target"), "kept");
    assert_eq!(boot_id(&rig2), "boot-1");

    for (case, blocked) in [("dir", tmp.as_str()), ("id-dir", BOOT_ID_FILE)] {
        let rig = NativeHost::new(&format!("boot-stale-{case}"));
        lost_in(&rig, Step::Prepare);
        fs::remove_file(rig.state.join(BOOT_ID_FILE)).expect("no boot id");
        fs::create_dir_all(rig.state.join(blocked).join("inside")).expect("a directory");
        rig.reboot();
        let log = Log::default();
        rig.launcher.log_to(log.clone());
        let mut report = rig.daemon().boot_pass().expect("the start pass");
        assert!(!report.boot_id_written, "{case}");
        assert!(matches!(
            web(&report).outcome,
            Outcome::JournalUnavailable(_)
        ));
        let notifier = FakeNotifier::new(log.clone());
        rig.daemon().after_boot(&mut report, &*notifier);
        assert_eq!(calls(&log), ["ready"], "{case}");
        fs::remove_dir_all(rig.state.join(blocked)).expect("clear it");
        let (_, report, _) = restart(&rig);
        assert!(report.rebooted, "{case}");
        assert_eq!(web(&report).verdict(), Some(rebooted_deferred()), "{case}");
    }
}

/// Catches a record trusted for a service it does not name, or a counter at its end that stops
/// the start: a record naming another service is held as corrupt while the other services go on;
/// a cut-off `rollback` whose attempt is the largest count runs again and the update ends.
#[test]
fn a_record_of_another_service_is_held_and_a_full_attempt_count_is_kept() {
    let rig = NativeHost::new("boot-misfiled");
    two_unfinished(&rig);
    let mut alpha = rig.record("alpha").expect("alpha's record");
    alpha.service = "beta".to_owned();
    RecordStore::new(&rig.state.join("alpha"))
        .save(&alpha)
        .expect("misfile it");
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let report = started.boot_report();
    assert_eq!(
        report.services["alpha"].outcome,
        Outcome::Corrupt("the step record names the service \"beta\"".to_owned())
    );
    assert_eq!(report.services["beta"].verdict(), Some(ok()));
    assert_eq!(notifier.sent(), 1);
    drop(started);

    let rig = NativeHost::new("boot-full-attempt");
    rig.launcher.exits("web", Step::Verify, &[1]);
    lost_in(&rig, Step::Rollback);
    let mut rec = rig.record("web").expect("a record");
    rec.attempt = u32::MAX;
    RecordStore::new(&rig.state.join("web"))
        .save(&rec)
        .expect("a full count");
    rig.reboot();
    let (_, report, _) = restart(&rig);
    assert_eq!(web(&report).verdict(), Some(failed(Phase::GateRelease)));
    assert_eq!(rig.record("web").expect("a record").attempt, u32::MAX);
}
