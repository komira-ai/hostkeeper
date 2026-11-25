//! A maintenance turn that stops and is resumed from its run record: a stop at every write of the
//! record, a reboot in the middle of the apply, the boot id that cannot be read, a start with no
//! turn to resume, a turn that stopped and alerted, and the order sent again after a restart.

mod support;
mod support_maintain;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::executor::Reply;
use hostkeeper_daemon::maintenance_hook::Kind as HookKind;
use hostkeeper_daemon::maintenance_run::{Driven, unfinished};
use hostkeeper_daemon::maintenance_state::run_state;
use hostkeeper_daemon::refusal::Refusal;
use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceKind, MaintenanceStep};
use hostkeeper_steps::maintenance::{
    Ending, Kind, MAINTENANCE_RECORD_FILE, MaintenanceRecord, MaintenanceStore, RunStep,
};
use hostkeeper_steps::{CrashGate, CrashPoint};

use support::{Rig, alerting, lines, name};
use support_maintain::{
    ORDER, RUN, boot, draining, launched, maintain, order_of, params_of, plain, reboot_on_cutover,
    record, terms,
};

/// The answer of a turn that ended well.
fn succeeded(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent,
    }
}

/// A gate that stops the record's writer at `at`, as a crash would.
fn stop_at(at: CrashPoint) -> CrashGate {
    Arc::new(move |point: CrashPoint| point == at)
}

/// Two services whose release names `drain` and `resume`.
fn fleet(rig: &Rig) {
    draining(rig, "alpha", "1.0.0");
    draining(rig, "beta", "1.0.0");
}

/// One service whose release names neither: a turn with nothing to drain.
fn lone(rig: &Rig) {
    plain(rig, "alpha", "1.0.0");
}

/// `list` without a step launched twice in a row: a step cut short is launched again, and that is
/// the one repeat a resumed turn may show.
fn once_each(mut list: Vec<String>) -> Vec<String> {
    list.dedup();
    list
}

/// The record of the turn [`ORDER`], try 1, that ended well: every kind applied, nothing drained.
fn done(kinds: &[Kind], services: &[&str]) -> MaintenanceRecord {
    let names: Vec<_> = services.iter().map(|service| name(service)).collect();
    let mut record = MaintenanceRecord::new(RUN, ORDER, 1, kinds, names).expect("a valid record");
    for progress in &mut record.kinds {
        progress.applied = true;
    }
    record.step = RunStep::Done;
    record
}

/// The path of the run record of the rig's daemon.
fn record_file(rig: &Rig) -> std::path::PathBuf {
    MaintenanceStore::beside(&rig.state)
        .dir()
        .join(MAINTENANCE_RECORD_FILE)
}

/// The steps a full turn of the Linux kind launches over `alpha` and `beta`, in order.
const FULL_TURN: [&str; 7] = [
    "hostkeeper prepare",
    "alpha drain",
    "beta drain",
    "hostkeeper cutover",
    "hostkeeper verify",
    "beta resume",
    "alpha resume",
];

/// Catches a resume that skips a step, launches a step finished earlier, or ends the turn in a
/// different state: the turn is stopped before and after every write of its record, then resumed
/// (or, with no record yet, the order is sent again), and every time it launches the same steps in
/// the same order as the turn that was never stopped (a step cut short is launched again), ends
/// done with every kind applied and nothing drained, and leaves the same record.
#[test]
fn a_turn_stopped_at_any_write_of_its_record_resumes_to_the_same_end() {
    let kinds = [MaintenanceKind::Linux];
    let rig = Rig::new("resume_clean");
    fleet(&rig);
    let writes = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&writes);
    let counting = rig
        .daemon()
        .with_maintenance_crash_gate(Arc::new(move |point: CrashPoint| {
            if let CrashPoint::BeforeWrite(n) = point {
                counted.fetch_max(n, Ordering::SeqCst);
            }
            false
        }));
    assert_eq!(counting.order(&maintain(&kinds)), succeeded(false));
    let clean = once_each(launched(&rig));
    assert_eq!(clean, FULL_TURN);
    // The record of the new turn, then the start and the end of each of its seven steps.
    let total = writes.load(Ordering::SeqCst);
    assert_eq!(total, 1 + 2 * clean.len() as u64);
    let clean_record = record(&rig).expect("the record of the clean turn");
    assert_eq!(clean_record, done(&[Kind::Linux], &["alpha", "beta"]));

    for n in 1..=total {
        for at in [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)] {
            let when = match at {
                CrashPoint::BeforeWrite(_) => "before",
                CrashPoint::AfterWrite(_) => "after",
            };
            let label = format!("{at:?}");
            let rig = Rig::new(&format!("resume_{when}_{n}"));
            fleet(&rig);
            let reply = rig
                .daemon()
                .with_maintenance_crash_gate(stop_at(at))
                .order(&maintain(&kinds));
            // The first write is the new turn's: nothing has run, the order is refused.
            let stopped = match n {
                1 => matches!(
                    reply,
                    Reply::Refused {
                        refusal: Refusal::StoreFailed,
                        ..
                    }
                ),
                _ => matches!(
                    reply,
                    Reply::Interrupted {
                        refusal: Refusal::StoreFailed,
                        ..
                    }
                ),
            };
            assert!(stopped, "{label}: {reply:?}");

            let restarted = rig.daemon();
            match record(&rig) {
                None => assert_eq!(
                    restarted.order(&maintain(&kinds)),
                    succeeded(false),
                    "{label}: the order sent again"
                ),
                Some(held) if unfinished(&held) => assert_eq!(
                    restarted.resume_maintenance(),
                    Some(Driven::Ended(Ending::Done)),
                    "{label}: the resumed turn"
                ),
                Some(_) => assert_eq!(
                    restarted.resume_maintenance(),
                    None,
                    "{label}: a turn whose last write was durable has nothing to resume"
                ),
            }

            assert_eq!(
                once_each(launched(&rig)),
                clean,
                "{label}: the steps launched"
            );
            assert_eq!(
                record(&rig),
                Some(clean_record.clone()),
                "{label}: the record"
            );
        }
    }
}

/// Catches a resume after a reboot that applies the kind again, or skips the next one: the
/// `macos` cutover asks for a reboot, the turn waits (`Running`, the record says `rebooting` and
/// holds the boot id), a restart on the same boot launches the `macos` cutover once more (which
/// asks for the reboot again) and keeps waiting with the record unchanged, and on the new boot
/// the next kind (`xcode`) is applied alone, then the check, with `macos` applied once.
#[test]
fn a_reboot_in_the_apply_continues_with_the_next_kind_and_not_the_finished_one() {
    let rig = Rig::new("resume_reboot_next");
    lone(&rig);
    reboot_on_cutover(&rig, HookKind::Macos, MaintenanceKind::Macos);
    let order = maintain(&[MaintenanceKind::Macos, MaintenanceKind::Xcode]);

    assert_eq!(rig.daemon().order(&order), Reply::Running);
    let waiting = record(&rig).expect("the record");
    assert_eq!(waiting.step, RunStep::Apply);
    assert!(!waiting.running);
    assert!(!waiting.kinds[0].applied);
    assert_eq!(waiting.kinds[0].reboot_under_way, Some(boot("boot-1")));
    assert!(!waiting.kinds[1].applied);
    assert_eq!(waiting.kinds[1].reboot_under_way, None);
    assert_eq!(
        run_state(&waiting).step,
        i32::from(MaintenanceStep::Rebooting)
    );
    assert_eq!(launched(&rig), ["hostkeeper prepare", "hostkeeper cutover"]);

    // The same boot: the reboot has not happened; the macOS cutover is asked again, and waits.
    assert_eq!(
        rig.daemon().resume_maintenance(),
        Some(Driven::Waiting(Kind::Macos))
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper cutover"
        ]
    );
    assert!(params_of(&rig, 2).contains("\"MAINTENANCE_KIND_MACOS\""));
    assert!(!params_of(&rig, 2).contains("\"MAINTENANCE_KIND_XCODE\""));
    assert_eq!(record(&rig), Some(waiting));

    rig.clock.set_boot_id("boot-2");
    assert_eq!(
        rig.daemon().resume_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper cutover",
            "hostkeeper cutover",
            "hostkeeper verify"
        ]
    );
    assert!(params_of(&rig, 1).contains("\"MAINTENANCE_KIND_MACOS\""));
    assert!(!params_of(&rig, 1).contains("\"MAINTENANCE_KIND_XCODE\""));
    assert!(params_of(&rig, 3).contains("\"MAINTENANCE_KIND_XCODE\""));
    assert!(!params_of(&rig, 3).contains("\"MAINTENANCE_KIND_MACOS\""));
    assert_eq!(record(&rig), Some(done(&[Kind::Macos, Kind::Xcode], &[])));
}

/// Catches a `reboot` kind that reboots the host a second time: the Linux cutover reboots, and on
/// the new boot the `reboot` kind that follows it is applied with no cutover of its own (only the
/// Linux cutover is ever launched, and no params file names the `reboot` kind).
#[test]
fn a_reboot_kind_after_a_kind_that_rebooted_is_applied_without_a_second_reboot() {
    let rig = Rig::new("resume_reboot_kind_after");
    lone(&rig);
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let order = maintain(&[MaintenanceKind::Linux, MaintenanceKind::Reboot]);

    assert_eq!(rig.daemon().order(&order), Reply::Running);
    let waiting = record(&rig).expect("the record");
    assert_eq!(waiting.kinds[0].kind, Kind::Linux);
    assert_eq!(waiting.kinds[0].reboot_under_way, Some(boot("boot-1")));
    assert!(!waiting.kinds[1].applied);

    rig.reboot();
    assert_eq!(
        rig.daemon().resume_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper verify"
        ]
    );
    assert!(!params_of(&rig, 1).contains("MAINTENANCE_KIND_REBOOT"));
    assert!(params_of(&rig, 1).contains("\"MAINTENANCE_KIND_LINUX\""));
    assert_eq!(record(&rig), Some(done(&[Kind::Linux, Kind::Reboot], &[])));
}

/// Catches a second reboot asked for after a restart: on the new boot, the first write of the
/// record (the one stopped right after it) already holds both the Linux kind and the `reboot`
/// kind that follows it applied, so a daemon that restarts there resumes at the check and never
/// launches a cutover of the `reboot` kind.
#[test]
fn the_reboot_kind_is_applied_in_the_same_write_as_the_reboot_it_rode_on() {
    let rig = Rig::new("resume_reboot_kind_one_write");
    lone(&rig);
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let order = maintain(&[MaintenanceKind::Linux, MaintenanceKind::Reboot]);
    assert_eq!(rig.daemon().order(&order), Reply::Running);

    rig.reboot();
    let gate: CrashGate = Arc::new(|point| point == CrashPoint::AfterWrite(1));
    let stopped = rig
        .daemon()
        .with_maintenance_crash_gate(gate)
        .resume_maintenance();
    assert!(matches!(stopped, Some(Driven::Stopped(_))), "{stopped:?}");
    let written = record(&rig).expect("the record");
    assert_eq!(
        written
            .kinds
            .iter()
            .map(|progress| (
                progress.kind,
                progress.applied,
                progress.reboot_under_way.clone()
            ))
            .collect::<Vec<_>>(),
        [(Kind::Linux, true, None), (Kind::Reboot, true, None)]
    );
    assert_eq!(
        rig.daemon().resume_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper verify"
        ]
    );
}

/// Catches a `reboot` kind that is applied without anything asking for the reboot: a run of the
/// `reboot` kind alone, with no earlier kind that rebooted, launches the hook's cutover for it
/// (its params name the kind), between the stage and the check.
#[test]
fn a_reboot_kind_with_no_earlier_reboot_launches_its_cutover() {
    let rig = Rig::new("resume_reboot_kind_alone");
    lone(&rig);

    assert_eq!(
        rig.daemon().order(&maintain(&[MaintenanceKind::Reboot])),
        succeeded(false)
    );
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper cutover",
            "hostkeeper verify"
        ]
    );
    assert!(params_of(&rig, 1).contains("\"MAINTENANCE_KIND_REBOOT\""));
    assert_eq!(record(&rig), Some(done(&[Kind::Reboot], &[])));
}

/// Catches a turn that goes on when the boot id cannot be read: whether the reboot happened cannot
/// be told, so the resumed turn keeps waiting, launches nothing, leaves the record as it is and
/// logs why; once the boot id reads again and is a new one, the turn finishes.
#[test]
fn a_boot_id_that_cannot_be_read_keeps_the_turn_waiting_and_is_logged() {
    let rig = Rig::new("resume_boot_id_lost");
    lone(&rig);
    reboot_on_cutover(&rig, HookKind::Macos, MaintenanceKind::Macos);
    assert_eq!(
        rig.daemon().order(&maintain(&[MaintenanceKind::Macos])),
        Reply::Running
    );
    let waiting = record(&rig).expect("the record");
    let before = launched(&rig);

    rig.clock.lose_boot_id();
    let (daemon, log) = alerting(rig.daemon());
    assert_eq!(
        daemon.resume_maintenance(),
        Some(Driven::Waiting(Kind::Macos))
    );
    let said = lines(&log);
    assert_eq!(said.len(), 2, "{said:?}");
    assert_eq!(
        said[0],
        "hostkeeper daemon: maintenance run run-1: resumed at wait for the reboot of macos"
    );
    assert!(
        said[1].starts_with(
            "hostkeeper daemon: maintenance run run-1: waits for the reboot of macos: "
        ),
        "{said:?}"
    );
    assert_eq!(launched(&rig), before);
    assert_eq!(record(&rig), Some(waiting));

    rig.reboot();
    assert_eq!(
        rig.daemon().resume_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(record(&rig), Some(done(&[Kind::Macos], &[])));
}

/// Catches a start that makes up a turn: with no run record, `resume_maintenance` answers `None`,
/// launches nothing and says nothing.
#[test]
fn nothing_is_resumed_when_there_is_no_record() {
    let rig = Rig::new("resume_no_record");
    lone(&rig);
    let (daemon, log) = alerting(rig.daemon());

    assert_eq!(daemon.resume_maintenance(), None);
    assert_eq!(launched(&rig), Vec::<String>::new());
    assert_eq!(lines(&log), Vec::<String>::new());
    assert_eq!(record(&rig), None);
}

/// Catches a start that runs a finished turn again: with a record whose turn ended,
/// `resume_maintenance` answers `None` and launches nothing more.
#[test]
fn nothing_is_resumed_when_the_turn_in_the_record_has_ended() {
    let rig = Rig::new("resume_ended_record");
    fleet(&rig);
    assert_eq!(
        rig.daemon().order(&maintain(&[MaintenanceKind::Linux])),
        succeeded(false)
    );
    let launches = launched(&rig);
    let ended = record(&rig);

    assert_eq!(rig.daemon().resume_maintenance(), None);
    assert_eq!(launched(&rig), launches);
    assert_eq!(record(&rig), ended);
}

/// Catches two sequencers on one host: while the host's sequencer is held, an unfinished record is
/// not resumed (`None`, nothing launched, the record as it was); once the hold is let go, the same
/// record resumes and the turn finishes.
#[test]
fn nothing_is_resumed_while_a_sequencer_holds_the_host() {
    let rig = Rig::new("resume_held");
    fleet(&rig);
    let reply = rig
        .daemon()
        .with_maintenance_crash_gate(stop_at(CrashPoint::BeforeWrite(3)))
        .order(&maintain(&[MaintenanceKind::Linux]));
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    let stopped = record(&rig).expect("the record of the stopped turn");
    assert!(unfinished(&stopped));
    let launches = launched(&rig);
    assert_eq!(launches, ["hostkeeper prepare"]);

    let daemon = rig.daemon();
    let held = daemon.ctx().maintenance.hold(ORDER).expect("the hold");
    assert_eq!(daemon.resume_maintenance(), None);
    assert_eq!(launched(&rig), launches);
    assert_eq!(record(&rig), Some(stopped));

    drop(held);
    assert_eq!(
        daemon.resume_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(record(&rig), Some(done(&[Kind::Linux], &["alpha", "beta"])));
}

/// Catches a start that resumes, or overwrites, a record it cannot read: a corrupt record is not
/// resumed (`None`, nothing launched), the file is left as it was, and the line says why.
#[test]
fn a_corrupt_record_is_not_resumed_and_the_reason_is_logged() {
    let rig = Rig::new("resume_corrupt");
    lone(&rig);
    let file = record_file(&rig);
    fs::create_dir_all(file.parent().expect("the record's directory")).expect("make it");
    fs::write(&file, b"this is not a record").expect("write the record");
    let (daemon, log) = alerting(rig.daemon());

    assert_eq!(daemon.resume_maintenance(), None);
    assert_eq!(launched(&rig), Vec::<String>::new());
    let said = lines(&log);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with(
            "hostkeeper daemon: maintenance run record not resumed: the maintenance record is not readable: "
        ),
        "{said:?}"
    );
    assert_eq!(
        fs::read(&file).expect("the record"),
        b"this is not a record"
    );
}

/// Catches a start that resumes a record of a newer hostkeeper: it is not resumed and not touched,
/// and the line names its schema.
#[test]
fn a_record_of_a_newer_schema_is_not_resumed_and_the_reason_is_logged() {
    let rig = Rig::new("resume_newer");
    lone(&rig);
    let file = record_file(&rig);
    fs::create_dir_all(file.parent().expect("the record's directory")).expect("make it");
    fs::write(&file, br#"{"schema": 99}"#).expect("write the record");
    let (daemon, log) = alerting(rig.daemon());

    assert_eq!(daemon.resume_maintenance(), None);
    assert_eq!(launched(&rig), Vec::<String>::new());
    assert_eq!(
        lines(&log),
        [
            "hostkeeper daemon: maintenance run record not resumed: the maintenance record has schema 99, of a newer hostkeeper; it is left as it is"
        ]
    );
    assert_eq!(fs::read(&file).expect("the record"), br#"{"schema": 99}"#);
}

/// Catches a turn that stops silently: when the record cannot be written mid-turn, the reply is
/// `Interrupted` (`store_failed`) with the run named, one `maintenance_stopped` alert carries the
/// same words, and the record stays at the step that was launched.
#[test]
fn a_turn_stopped_mid_way_alerts_with_the_run_named() {
    let rig = Rig::new("resume_alert");
    fleet(&rig);
    let (daemon, log) = alerting(
        rig.daemon()
            .with_maintenance_crash_gate(stop_at(CrashPoint::BeforeWrite(3))),
    );

    let reply = daemon.order(&maintain(&[MaintenanceKind::Linux]));
    let why = "maintenance run run-1: stopped at BeforeWrite(3)";
    assert_eq!(
        reply,
        Reply::Interrupted {
            refusal: Refusal::StoreFailed,
            detail: why.to_owned(),
        }
    );
    let alerts: Vec<String> = lines(&log)
        .into_iter()
        .filter(|line| line.contains("alert="))
        .collect();
    assert_eq!(
        alerts,
        [format!(
            "hostkeeper daemon: level=error alert=maintenance_stopped {why}"
        )]
    );
    let held = record(&rig).expect("the record");
    assert!(held.running);
    assert_eq!(held.step, RunStep::Stage);
}

/// Catches an order sent again after a restart that starts the turn over or relaunches finished
/// steps: with the record durable after the cutover ended, a higher try of the same order resumes
/// the turn and finishes it, so the steps launched in all are exactly those of one turn; and a
/// further resend answers the end again (`resent`) and launches nothing.
#[test]
fn the_order_sent_again_after_a_restart_resumes_the_same_turn() {
    let rig = Rig::new("resume_resent");
    fleet(&rig);
    let kinds = [MaintenanceKind::Linux];
    let reply = rig
        .daemon()
        .with_maintenance_crash_gate(stop_at(CrashPoint::AfterWrite(9)))
        .order(&maintain(&kinds));
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover"
        ]
    );

    let again = order_of(ORDER, 2, terms(&kinds, None));
    assert_eq!(rig.daemon().order(&again), succeeded(false));
    assert_eq!(launched(&rig), FULL_TURN);
    assert_eq!(record(&rig), Some(done(&[Kind::Linux], &["alpha", "beta"])));

    assert_eq!(rig.daemon().order(&again), succeeded(true));
    assert_eq!(launched(&rig), FULL_TURN);
}

/// Catches a daemon start that leaves a turn where a crash stopped it: the start resumes the
/// unfinished turn of the run record on a thread of its own, which ends the turn (the stage cut
/// short is launched again, then the rest of the turn), and a start with no record resumes
/// nothing.
#[test]
fn the_daemon_start_resumes_an_unfinished_turn() {
    let rig = Rig::new("resume_at_start");
    fleet(&rig);
    let reply = rig
        .daemon()
        .with_maintenance_crash_gate(stop_at(CrashPoint::BeforeWrite(3)))
        .order(&maintain(&[MaintenanceKind::Linux]));
    assert!(matches!(reply, Reply::Interrupted { .. }), "{reply:?}");
    assert_eq!(launched(&rig), ["hostkeeper prepare"]);

    let mut started = hostkeeper_daemon::start::startup(&rig.settings(), rig.ports(), support::OWN)
        .expect("the daemon starts");
    assert_eq!(
        started.join_maintenance(),
        Some(Driven::Ended(Ending::Done))
    );
    assert_eq!(started.join_maintenance(), None);
    assert_eq!(record(&rig), Some(done(&[Kind::Linux], &["alpha", "beta"])));
    assert_eq!(
        launched(&rig),
        [
            "hostkeeper prepare",
            "hostkeeper prepare",
            "alpha drain",
            "beta drain",
            "hostkeeper cutover",
            "hostkeeper verify",
            "beta resume",
            "alpha resume"
        ]
    );
    drop(started);

    let empty = Rig::new("resume_at_start_none");
    let mut started =
        hostkeeper_daemon::start::startup(&empty.settings(), empty.ports(), support::OWN)
            .expect("the daemon starts");
    assert_eq!(started.join_maintenance(), None);
}
