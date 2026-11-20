//! The edges of a host's maintenance turn that the other `maintain` tests do not reach: a release
//! that changes or breaks between the plan and a step, a launcher that cannot stop or start a
//! unit, the run's parameters missing, unreadable or not writable, a record that cannot be
//! written, what each shape of a hook's last line says in the reason, and the intake's refusals
//! for a package step the host does not show in a lock.

mod support;
mod support_maintain;

use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::link_adapters::DaemonMaintain;
use hostkeeper_daemon::maintenance_hook::Kind as HookKind;
use hostkeeper_daemon::maintenance_run::Driven;
use hostkeeper_daemon::testing::AdditiveFault;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Params, Verdict};
use hostkeeper_link::{MaintainOrder, MaintainSink, OrderId};
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_steps::maintenance::{
    Ending, Kind, MAINTENANCE_RECORD_FILE, MaintenanceRecord, MaintenanceStore, RunStep,
};
use hostkeeper_steps::record::{HookSel, Progress, RECORD_FILE, Record, RecordStore, State};
use hostkeeper_steps::{Bound, CrashGate, CrashPoint};
use support::{Rig, alerting, lines, manifest_with, name};
use support_maintain::{
    HOOK, ORDER, RUN, draining, launched, maintain, order_of, reboot_on_cutover, record, terms,
};

const LINUX: [MaintenanceKind; 1] = [MaintenanceKind::Linux];

/// The `[hook]` lines of a release that names a `drain` and no `resume`.
const DRAIN_ONLY: &str = "drain = \"drain\"";
/// The `[hook]` lines of a release that names a `resume` and no `drain`.
const RESUME_ONLY: &str = "resume = \"resume\"";

fn strings(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|step| (*step).to_owned()).collect()
}

fn finished(verdict: Verdict, reason: &str) -> Reply {
    Reply::Finished {
        verdict,
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

fn halt(reason: &str) -> Reply {
    finished(Verdict::Halt, reason)
}

fn deferred(reason: &str) -> Reply {
    finished(
        Verdict::Deferred {
            retry_after_secs: None,
        },
        reason,
    )
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The record of a new Linux turn over `services`, before any step.
fn fresh_over(services: &[&str]) -> MaintenanceRecord {
    let names = services.iter().map(|service| name(service)).collect();
    MaintenanceRecord::new(RUN, ORDER, 1, &[Kind::Linux], names).expect("a record")
}

/// The record of the Linux turn over `services` that ended well.
fn done_over(services: &[&str]) -> MaintenanceRecord {
    let mut record = fresh_over(services);
    record.kinds[0].applied = true;
    record.step = RunStep::Done;
    record
}

/// The directory of the run record.
fn maintenance_dir(rig: &Rig) -> PathBuf {
    MaintenanceStore::beside(&rig.state).dir().to_path_buf()
}

/// A gate that stops the record's writer at `at`, as a crash would.
fn stop_at(at: CrashPoint) -> CrashGate {
    Arc::new(move |point: CrashPoint| point == at)
}

/// Runs the Linux order of `order` on a daemon that is stopped at the fourth write of the run
/// record: the new turn, the start and the end of the stage are written, the start of the next
/// step (a service's drain, or the cutover of a host with none) is not.
fn stopped_after_the_stage(rig: &Rig, order: &hostkeeper_proto::hostkeeper::agent::v1::Order) {
    let reply = rig
        .daemon()
        .with_maintenance_crash_gate(stop_at(CrashPoint::BeforeWrite(4)))
        .order(order);
    assert!(
        matches!(
            reply,
            Reply::Interrupted {
                refusal: Refusal::StoreFailed,
                ..
            }
        ),
        "{reply:?}"
    );
}

/// The turn the record holds, resumed by a new daemon.
fn resumed(rig: &Rig) -> Driven {
    rig.daemon().resume_maintenance().expect("a turn to resume")
}

/// The reason a resumed turn halted with.
fn halted_with(driven: Driven) -> String {
    match driven {
        Driven::Ended(Ending::Halted(why)) => why,
        other => unreachable!("the turn did not halt: {other:?}"),
    }
}

/// A host whose services `alpha` and `beta` both drain.
fn fleet(rig: &Rig) {
    draining(rig, "alpha", "1.0.0");
    draining(rig, "beta", "1.0.0");
}

/// `service` runs `1.0.0` from a sealed package whose `[hook]` has `lines` added.
fn hooked(rig: &Rig, service: &str, lines: &str) {
    rig.add_with(service, "1.0.0", &manifest_with(service, "1.0.0", lines));
    rig.store.link(service, "current", "1.0.0");
}

/// Catches a service whose `current` is gone by the time its drain is due being launched or
/// failing the turn: the turn is stopped before `alpha`'s drain, `alpha`'s `current` is removed,
/// and the resumed turn passes `alpha` over (its drain and its resume launch nothing) and ends
/// done with the record as a turn that drained every service.
#[test]
fn a_service_whose_current_is_gone_at_its_drain_is_passed_over() {
    let rig = Rig::new("edges-current-gone");
    fleet(&rig);
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    fs::remove_file(rig.packages.join("alpha").join("current")).expect("current removed");
    assert_eq!(resumed(&rig), Driven::Ended(Ending::Done));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
        ])
    );
    assert_eq!(record(&rig), Some(done_over(&["alpha", "beta"])));
}

/// Catches a service whose release lost its `drain` after the plan being drained anyway: `alpha`
/// now runs a release that names a `resume` and no `drain`, so its drain launches nothing and its
/// resume is launched.
#[test]
fn a_release_that_lost_its_drain_after_the_plan_is_not_drained() {
    let rig = Rig::new("edges-drain-lost");
    fleet(&rig);
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    rig.add_with(
        "alpha",
        "2.0.0",
        &manifest_with("alpha", "2.0.0", RESUME_ONLY),
    );
    rig.store.link("alpha", "current", "2.0.0");
    assert_eq!(resumed(&rig), Driven::Ended(Ending::Done));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume",
        ])
    );
    assert_eq!(record(&rig), Some(done_over(&["alpha", "beta"])));
}

/// Catches a service whose release lost its `resume` after the plan being resumed anyway:
/// `alpha` now runs a release that names a `drain` and no `resume`, so its drain is launched and
/// its resume launches nothing.
#[test]
fn a_release_that_lost_its_resume_after_the_plan_is_not_resumed() {
    let rig = Rig::new("edges-resume-lost");
    fleet(&rig);
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    rig.add_with(
        "alpha",
        "2.0.0",
        &manifest_with("alpha", "2.0.0", DRAIN_ONLY),
    );
    rig.store.link("alpha", "current", "2.0.0");
    assert_eq!(resumed(&rig), Driven::Ended(Ending::Done));
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
        ])
    );
    assert_eq!(record(&rig), Some(done_over(&["alpha", "beta"])));
}

/// Catches a plan that drains a service that names only one of the two steps, or one that is
/// not installed: `alpha` names a `drain` alone, `beta` a `resume` alone, `delta` has a package
/// and no `current`, and `gamma` names both, so only `gamma` is in the record and launched.
#[test]
fn only_a_service_that_names_both_steps_and_runs_is_planned() {
    let rig = Rig::new("edges-plan");
    hooked(&rig, "alpha", DRAIN_ONLY);
    hooked(&rig, "beta", RESUME_ONLY);
    rig.stage("delta", "1.0.0");
    draining(&rig, "gamma", "1.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&maintain(&LINUX)), succeeded());
    assert_eq!(
        launched(&rig),
        strings(&[
            "hostkeeper prepare",
            "gamma drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "gamma resume",
        ])
    );
    assert_eq!(record(&rig), Some(done_over(&["gamma"])));
}

/// Catches a drain that is launched over a unit of the same name the launcher cannot stop: the
/// stop before `alpha`'s drain fails, so the drain is not launched, the turn halts with the
/// launcher's words and the record keeps `alpha` drained.
#[test]
fn a_drain_whose_old_unit_cannot_be_stopped_halts_before_the_launch() {
    let rig = Rig::new("edges-drain-stop");
    draining(&rig, "alpha", "1.0.0");
    rig.launcher.fail_stops("alpha");
    let daemon = rig.daemon();
    let why = "drain alpha: the launcher failed: updater-alpha-drain-1 could not be stopped";
    assert_eq!(daemon.order(&maintain(&LINUX)), halt(why));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    let mut expected = fresh_over(&["alpha"]);
    expected.step = RunStep::Drain;
    expected.drained = vec![name("alpha")];
    expected.halted = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a stage that is launched over a unit of the same name the launcher cannot stop: the
/// stop before `prepare` fails, so `prepare` is not launched and the turn defers with nothing
/// drained.
#[test]
fn a_stage_whose_old_unit_cannot_be_stopped_defers_before_the_launch() {
    let rig = Rig::new("edges-stage-stop");
    draining(&rig, "alpha", "1.0.0");
    rig.launcher.fail_stops(HOOK);
    let daemon = rig.daemon();
    let why = "stage: the launcher failed: updater-hostkeeper-prepare-1 could not be stopped";
    assert_eq!(daemon.order(&maintain(&LINUX)), deferred(why));
    assert!(launched(&rig).is_empty());
    let mut expected = fresh_over(&["alpha"]);
    expected.step = RunStep::Done;
    expected.deferred = Some(why.to_owned());
    assert_eq!(record(&rig), Some(expected));
}

/// Catches a lost hook unit whose stop also fails being reported as stopped: `prepare` is lost
/// by the launcher and its unit cannot be stopped, so the reason says both.
#[test]
fn a_lost_unit_that_cannot_be_stopped_is_said_in_the_reason() {
    let rig = Rig::new("edges-lost-unstoppable");
    rig.launcher
        .additive_fault(HOOK, AdditiveFault::LostUnstoppable);
    let daemon = rig.daemon();
    let why = "stage: the launcher failed: the launcher failed: updater-hostkeeper-prepare-1 was lost; its unit updater-hostkeeper-prepare-1 was not stopped: the launcher failed: updater-hostkeeper-prepare-1 could not be stopped";
    assert_eq!(daemon.order(&maintain(&LINUX)), deferred(why));
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
}

/// Catches a service's drain that the launcher loses or cannot start being read as anything but
/// a failed drain: each halts the turn with the launcher's words, and the unit of the drain is
/// stopped after the launch.
#[test]
fn a_drain_the_launcher_loses_or_cannot_start_halts_the_turn() {
    let cases = [
        (
            AdditiveFault::Lost,
            "drain alpha: the launcher failed: the launcher failed: updater-alpha-drain-1 was lost",
        ),
        (
            AdditiveFault::NoStart,
            "drain alpha: the launcher failed: the hook did not start: no such file",
        ),
    ];
    for (fault, why) in cases {
        let rig = Rig::new(&format!("edges-drain-{fault:?}"));
        draining(&rig, "alpha", "1.0.0");
        rig.launcher.additive_fault("alpha", fault);
        let daemon = rig.daemon();
        assert_eq!(daemon.order(&maintain(&LINUX)), halt(why), "{fault:?}");
        assert_eq!(
            launched(&rig),
            strings(&["hostkeeper prepare", "alpha drain"]),
            "{fault:?}"
        );
        let stops = rig.launcher.stops();
        let unit = "updater-alpha-drain-1";
        assert_eq!(stops.iter().filter(|stop| *stop == unit).count(), 2);
    }
}

/// Catches a stage's reason that is not made of the exit and what the hook's last line says: a
/// report with a reason and no detail says the reason, a report with no reason and a last line
/// that is no report say the exit alone, and a hook the launcher killed has no exit code to say.
#[test]
fn the_reason_of_a_stage_says_the_exit_and_what_the_last_line_reports() {
    let cases = [
        (
            "reason-only",
            r#"{"class":"release","reason":"disk_full"}"#,
            "stage: exit 1, disk_full",
        ),
        ("no-reason", r#"{"class":"release"}"#, "stage: exit 1"),
        ("not-a-report", "the disk is full", "stage: exit 1"),
    ];
    for (label, line, why) in cases {
        let rig = Rig::new(&format!("edges-stage-{label}"));
        rig.launcher.additive_replies(HOOK, &[(1, line)]);
        let daemon = rig.daemon();
        assert_eq!(daemon.order(&maintain(&LINUX)), deferred(why), "{label}");
    }
    let rig = Rig::new("edges-stage-killed");
    rig.launcher.additive_fault(HOOK, AdditiveFault::Killed);
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&LINUX)),
        deferred("stage: ended with no exit code")
    );
}

/// The answer of a turn over `alpha` with a drain deadline of 60 seconds, whose drain exits
/// `code` after `spent` seconds on the clock, and what it launched.
fn drain_that_exits(label: &str, code: i32, spent: u64) -> (Reply, Vec<String>) {
    let rig = Rig::new(label);
    draining(&rig, "alpha", "1.0.0");
    rig.launcher.additive_exits("alpha", &[code]);
    let clock = Arc::clone(&rig.clock);
    rig.launcher.during_additive(move |call| {
        if call.service == "alpha" && call.step == "drain" {
            clock.advance(spent);
        }
    });
    let daemon = rig.daemon();
    let reply = daemon.order(&order_of(ORDER, 1, terms(&LINUX, Some(60))));
    (reply, launched(&rig))
}

/// Catches a drain that fails at its deadline being halted, and one that fails before it being
/// deferred: exit 1 with the clock past the 60 seconds is the launcher's end at the bound, so the
/// turn defers and puts `alpha` back; exit 1 before the deadline and exit 76 after it are the
/// hook's own, so the turn halts with `alpha` left drained.
#[test]
fn a_drain_that_exits_1_at_its_deadline_defers_and_before_or_with_76_halts() {
    let (reply, ran) = drain_that_exits("edges-drain-1-late", 1, 60);
    assert_eq!(
        reply,
        deferred("drain alpha: the drain deadline passed: exit 1")
    );
    assert_eq!(
        ran,
        strings(&["hostkeeper prepare", "alpha drain", "alpha resume"])
    );
    let (reply, ran) = drain_that_exits("edges-drain-1-early", 1, 59);
    assert_eq!(reply, halt("drain alpha: exit 1"));
    assert_eq!(ran, strings(&["hostkeeper prepare", "alpha drain"]));
    let (reply, ran) = drain_that_exits("edges-drain-76-late", 76, 60);
    assert_eq!(reply, halt("drain alpha: exit 76"));
    assert_eq!(ran, strings(&["hostkeeper prepare", "alpha drain"]));
}

/// Catches a drain deadline that wraps when the clock is at its end: with the unix clock at
/// `u64::MAX` and a deadline of 5 seconds, the drain is bounded by 5 seconds and told the
/// largest deadline a params file holds, not a small or negative one.
#[test]
fn a_drain_deadline_past_the_end_of_time_is_told_as_the_largest_deadline() {
    let rig = Rig::new("edges-deadline-saturates");
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig
        .daemon()
        .with_unix_clock(Arc::new(hostkeeper_link::testing::FakeClock::new(
            u64::MAX,
            0,
        )));
    let order = order_of(ORDER, 1, terms(&LINUX, Some(5)));
    assert_eq!(daemon.order(&order), succeeded());
    let ran = rig.launcher.additive_runs();
    let drain = &ran[1];
    assert_eq!(drain.service, "alpha");
    assert_eq!(drain.step, "drain");
    assert_eq!(
        drain.bound,
        Bound::Secs(NonZeroU64::new(5).expect("a non-zero bound"))
    );
    let params = Params::parse(&drain.params).expect("the drain's params");
    assert_eq!(params.drain_deadline_unix(), Some(i64::MAX.unsigned_abs()));
}

/// Catches a resumed turn that goes on without the run's parameters: with the file gone, the
/// `cutover` is not launched (it would name no kind), and the turn halts saying the parameters
/// and where they were; with the file not the order's JSON it halts saying the parameters are
/// unreadable; a drain halts the same way.
#[test]
fn a_resumed_turn_without_readable_parameters_halts_at_the_step_that_needs_them() {
    let rig = Rig::new("edges-params-missing");
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    let params = maintenance_dir(&rig).join("maintain-params.json");
    fs::remove_file(&params).expect("the parameters removed");
    let why = halted_with(resumed(&rig));
    let prefix = format!("apply linux: the run's parameters {}: ", params.display());
    assert!(why.starts_with(&prefix), "{why}");
    assert!(
        why.ends_with("No such file or directory (os error 2)"),
        "{why}"
    );
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    assert_eq!(record(&rig).and_then(|held| held.halted), Some(why));

    let rig = Rig::new("edges-params-unreadable");
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    fs::write(
        maintenance_dir(&rig).join("maintain-params.json"),
        "not json",
    )
    .expect("the parameters overwritten");
    let why = halted_with(resumed(&rig));
    assert!(
        why.starts_with("apply linux: the run's parameters: "),
        "{why}"
    );
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
    assert_eq!(record(&rig).and_then(|held| held.halted), Some(why));

    let rig = Rig::new("edges-params-drain");
    draining(&rig, "alpha", "1.0.0");
    stopped_after_the_stage(&rig, &maintain(&LINUX));
    fs::remove_file(maintenance_dir(&rig).join("maintain-params.json"))
        .expect("the parameters removed");
    let why = halted_with(resumed(&rig));
    assert!(
        why.starts_with("drain alpha: the run's parameters "),
        "{why}"
    );
    assert_eq!(launched(&rig), strings(&["hostkeeper prepare"]));
}

/// Catches a params file that cannot be written being launched over: a directory is where the
/// temporary file of `alpha`'s params goes, so the drain halts with the write's error and is not
/// launched; the same for the `cutover`'s params.
#[test]
fn a_params_file_that_cannot_be_written_halts_the_step_that_needs_it() {
    let cases = [
        (
            "alpha.params.tmp",
            "drain alpha: Is a directory (os error 21)",
            strings(&["hostkeeper prepare"]),
        ),
        (
            "maintain-cutover.json.tmp",
            "apply linux: Is a directory (os error 21)",
            strings(&["hostkeeper prepare", "alpha drain"]),
        ),
    ];
    for (blocked, why, ran) in cases {
        let rig = Rig::new(&format!("edges-write-{blocked}"));
        draining(&rig, "alpha", "1.0.0");
        fs::create_dir_all(maintenance_dir(&rig).join(blocked)).expect("a directory in the way");
        let daemon = rig.daemon();
        assert_eq!(daemon.order(&maintain(&LINUX)), halt(why), "{blocked}");
        assert_eq!(launched(&rig), ran, "{blocked}");
    }
}

/// Catches a new turn that starts when its parameters or its record cannot be written: each is
/// refused `store_failed` with the error, nothing is launched and no record is left (the
/// parameters' file, the record's file and the record's directory are each blocked in turn).
#[test]
fn a_new_turn_that_cannot_be_written_is_refused_and_launches_nothing() {
    let cases = [
        ("maintain-params.json.tmp", "Is a directory (os error 21)"),
        (
            "maintenance-record.json.tmp",
            "cannot use the maintenance directory: Is a directory (os error 21)",
        ),
    ];
    for (blocked, detail) in cases {
        let rig = Rig::new(&format!("edges-new-{blocked}"));
        fs::create_dir_all(maintenance_dir(&rig).join(blocked)).expect("a directory in the way");
        let daemon = rig.daemon();
        assert_eq!(
            daemon.order(&maintain(&LINUX)),
            Reply::refused(Refusal::StoreFailed, detail),
            "{blocked}"
        );
        assert!(launched(&rig).is_empty(), "{blocked}");
        assert_eq!(record(&rig), None, "{blocked}");
    }
    let rig = Rig::new("edges-new-dir-is-a-file");
    let dir = maintenance_dir(&rig);
    fs::create_dir_all(dir.parent().expect("its parent")).expect("the parent");
    fs::write(&dir, "in the way").expect("a file where the directory goes");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&LINUX)),
        Reply::refused(
            Refusal::StoreFailed,
            "cannot use the maintenance directory: Not a directory (os error 20)"
        )
    );
    assert!(launched(&rig).is_empty());
}

/// Catches a turn that goes on when its record is damaged under it: the record is overwritten
/// while the `cutover` runs, so the sequencer cannot read the reboot the hook may have written
/// and cannot write the end of the step; the turn stops with the record as it is (no launch after
/// the `cutover`) and the answer is an interruption.
#[test]
fn a_record_damaged_while_a_step_runs_stops_the_turn() {
    let rig = Rig::new("edges-record-damaged");
    let file = maintenance_dir(&rig).join(MAINTENANCE_RECORD_FILE);
    let damaged = file.clone();
    rig.launcher.during_additive(move |call| {
        if call.step == "cutover" {
            fs::write(&damaged, "garbage").expect("the record damaged");
        }
    });
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&LINUX));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed));
    assert!(
        matches!(&reply, Reply::Interrupted { detail, .. }
            if detail.starts_with("maintenance run run-1: ")),
        "{reply:?}"
    );
    assert_eq!(
        launched(&rig),
        strings(&["hostkeeper prepare", "hostkeeper cutover"])
    );
    assert_eq!(fs::read_to_string(&file).expect("the record"), "garbage");
}

/// A release that breaks as `breaks` says, on a host whose `alpha` drains.
fn reply_to_a_broken_release(label: &str, breaks: fn(&Rig)) -> (Reply, Rig) {
    let rig = Rig::new(label);
    draining(&rig, "alpha", "1.0.0");
    let daemon = rig.daemon();
    breaks(&rig);
    let reply = daemon.order(&maintain(&LINUX));
    (reply, rig)
}

/// Catches a service passed over when its release cannot be read: each way a release breaks
/// (no seal, a seal that is not one, a manifest that is not one, a `current` that is a file or
/// names a path, a packages directory that is not a directory) refuses the order
/// `release_unreadable` naming the service where there is one, with nothing launched and no
/// record.
#[test]
fn every_way_a_release_cannot_be_read_refuses_the_turn() {
    type Breaks = fn(&Rig);
    type Says = fn(&str) -> bool;
    let cases: [(&str, Breaks, Says); 6] = [
        (
            "no-seal",
            |rig| fs::remove_file(rig.packages.join("alpha/1.0.0.seal")).expect("seal removed"),
            |detail| detail.starts_with("alpha: "),
        ),
        (
            "bad-seal",
            |rig| fs::write(rig.packages.join("alpha/1.0.0.seal"), "garbage").expect("seal"),
            |detail| detail.starts_with("alpha: "),
        ),
        (
            "bad-manifest",
            |rig| fs::write(rig.packages.join("alpha/1.0.0/target.json"), "garbage").expect("it"),
            |detail| detail.starts_with("alpha: "),
        ),
        (
            "current-is-a-file",
            |rig| {
                fs::remove_file(rig.packages.join("alpha/current")).expect("link removed");
                fs::write(rig.packages.join("alpha/current"), "a file").expect("a file");
            },
            |detail| detail.starts_with("alpha: `current` is not a link: "),
        ),
        (
            "current-names-a-path",
            |rig| rig.store.link("alpha", "current", "../elsewhere"),
            |detail| {
                detail
                    == "alpha: `current` links to ../elsewhere, not to a version directory beside it"
            },
        ),
        (
            "packages-is-a-file",
            |rig| {
                fs::remove_dir_all(&rig.packages).expect("packages removed");
                fs::write(&rig.packages, "a file").expect("a file");
            },
            |detail| detail == "Not a directory (os error 20)",
        ),
    ];
    for (label, breaks, says) in cases {
        let (reply, rig) = reply_to_a_broken_release(&format!("edges-release-{label}"), breaks);
        assert_eq!(
            reply.refusal(),
            Some(Refusal::ReleaseUnreadable),
            "{label}: {reply:?}"
        );
        assert!(
            matches!(&reply, Reply::Refused { detail, .. } if says(detail)),
            "{label}: {reply:?}"
        );
        assert!(launched(&rig).is_empty(), "{label}");
        assert_eq!(record(&rig), None, "{label}");
    }
}

/// The step record of `web` in `dir` in `state`.
fn step_record(dir: &std::path::Path, state: State) -> Record {
    Record {
        schema: 1,
        service: "web".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "cd".repeat(32),
        tarball_digest: "ab".repeat(32),
        previous_digest: None,
        generation: 1,
        params: dir.join("params.toml"),
        step: hostkeeper_hook::Step::Prepare,
        attempt: 1,
        state,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

/// Catches a finished package step holding the host: `web`'s last update finished, so the turn
/// runs (and a host whose step record does not read, or is of a newer schema, is refused as one
/// whose step is unfinished).
#[test]
fn a_finished_step_record_does_not_hold_the_host_and_an_unreadable_one_does() {
    let rig = Rig::new("edges-step-finished");
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the state directory of web");
    let finished_update = step_record(
        &dir,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
        },
    );
    RecordStore::new(&dir)
        .save(&finished_update)
        .expect("the step record");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&maintain(&LINUX)), succeeded());
    assert_eq!(record(&rig), Some(done_over(&[])));

    for (label, text) in [("garbage", "not json"), ("newer", r#"{"schema":999}"#)] {
        let rig = Rig::new(&format!("edges-step-{label}"));
        let dir = rig.state.join("web");
        fs::create_dir_all(&dir).expect("the state directory of web");
        fs::write(dir.join(RECORD_FILE), text).expect("the step record");
        let daemon = rig.daemon();
        assert_eq!(
            daemon.order(&maintain(&LINUX)),
            Reply::refused(Refusal::Busy, "a package step of web is unfinished"),
            "{label}"
        );
        assert!(launched(&rig).is_empty(), "{label}");
        assert_eq!(record(&rig), None, "{label}");
    }
}

/// Catches a turn started over an uninstall that is half done: `placed.json` holds a tombstone
/// for `web`, so the order is refused `busy` naming it, with nothing launched and no record.
#[test]
fn an_uninstall_under_way_refuses_the_turn() {
    let rig = Rig::new("edges-uninstalling");
    let mut placed = hostkeeper_placed::PlacedRecord::default();
    placed
        .uninstalling
        .insert("web".to_owned(), hostkeeper_placed::Tombstone::default());
    rig.save_placed(&placed);
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&LINUX)),
        Reply::refused(Refusal::Busy, "the uninstall of web is unfinished")
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches a runner that logs the answer of a turn that only waits for a reboot: the turn of the
/// order waits, the answer is `Running`, and the log has no line of the runner (the heartbeat's
/// report is what says where the turn is).
#[test]
fn the_runner_logs_nothing_for_a_turn_that_waits_for_its_reboot() {
    let rig = Rig::new("edges-runner-waits");
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let (daemon, log) = alerting(rig.daemon());
    let runner = DaemonMaintain::new(Arc::new(daemon));
    runner.maintain(&MaintainOrder {
        id: OrderId::new(ORDER).expect("an order id"),
        order_try: 1,
        maintain: terms(&LINUX, None),
    });
    assert_eq!(
        launched(&rig),
        strings(&["hostkeeper prepare", "hostkeeper cutover"])
    );
    assert!(
        lines(&log)
            .iter()
            .all(|line| !line.contains("maintain order")),
        "{:?}",
        lines(&log)
    );
}
