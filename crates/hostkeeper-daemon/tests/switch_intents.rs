//! The switch a step record says was begun, read at start so that the placed paths are finished
//! or undone before anything runs (design sections 4.7 and 6.5): the table of record shapes, the
//! records read from disk, and a daemon killed in the switch to the target or in the switch back,
//! started again.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::boot::{BootReport, switch_intent};
use hostkeeper_daemon::start::{self, before_records};
use hostkeeper_daemon::{Daemon, Ports, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs};
use hostkeeper_placed::{PathState, PlacedRecord, Side, SwitchIntent};
use hostkeeper_steps::record::{RECORD_FILE, Record, State};
use hostkeeper_steps::{ForcePhase, ForceRecord, ForceVerdict, RecordStore, Stamp};
use support::{BASE, OWN, Rig, Tap, config, d, install, name, ok, update};

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

fn web() -> ServiceName {
    name("web")
}

fn intent(from: Option<&str>, side: Side) -> SwitchIntent {
    SwitchIntent {
        service: web(),
        from: from.map(version),
        target: version("2.0.0"),
        side,
    }
}

/// The finished record of an update of `web` to 2.0.0, for the table to change.
fn finished_record() -> Record {
    let rig = Rig::new("intent-base");
    rig.add("web", "2.0.0");
    let reply = rig.daemon().order(&update("web", "2.0.0", &d(), 1));
    assert!(matches!(reply, Reply::Finished { .. }), "{reply:?}");
    rig.record("web").expect("the record")
}

fn at(base: &Record, step: Step, state: State, committed: bool) -> Record {
    let mut rec = base.clone();
    rec.step = step;
    rec.state = state;
    rec.progress.committed = committed;
    rec.progress.watching = None;
    rec
}

fn done() -> State {
    State::Done {
        code: Some(0),
        last_line: None,
    }
}

fn forcing(phase: ForcePhase, from: Option<&str>, verdict: Option<ForceVerdict>) -> ForceRecord {
    ForceRecord {
        target_version: "2.0.0".to_owned(),
        target_digest: d(),
        from_version: from.map(str::to_owned),
        phase,
        order_try: 1,
        halt_generation: 0,
        order_id: None,
        verdict,
    }
}

/// Catches a record read as the wrong side of a switch, or as one when none is begun: one row per
/// shape of record (a `prepare`, a `cutover` pending or done, a `verify` or a `cutover` cut off
/// by a reboot before and after the commit, a `rollback`, a `cleanup`, a post-success watcher, a
/// finished update, a first install, a version that is no version, and a force before and after
/// its switch of `current`).
#[test]
fn a_record_says_which_side_of_the_switch_it_is_on() {
    let base = finished_record();
    let old = Some("1.0.0");
    let forward = Some(Side::Forward);
    let back = Some(Side::Back);
    // (record, rebooted, the version replaced, the side)
    let mut watching = at(&base, Step::Cleanup, done(), true);
    watching.progress.watching = Some(Stamp {
        boot_id: "boot-1".to_owned(),
        mono_secs: 1,
    });
    let finished = State::Finished {
        verdict: Verdict::Halt,
        reason: None,
    };
    let rows = [
        (
            "prepare",
            at(&base, Step::Prepare, State::Pending, false),
            false,
            old,
            None,
        ),
        (
            "prepare after a reboot",
            at(&base, Step::Prepare, State::Pending, false),
            true,
            old,
            None,
        ),
        (
            "prepare done",
            at(&base, Step::Prepare, done(), false),
            false,
            old,
            None,
        ),
        (
            "cutover pending",
            at(&base, Step::Cutover, State::Pending, false),
            false,
            old,
            forward,
        ),
        (
            "cutover done",
            at(&base, Step::Cutover, done(), false),
            false,
            old,
            forward,
        ),
        (
            "cutover cut off by a reboot",
            at(&base, Step::Cutover, State::Pending, false),
            true,
            old,
            back,
        ),
        (
            "cutover committed, then a reboot",
            at(&base, Step::Cutover, State::Pending, true),
            true,
            old,
            forward,
        ),
        (
            "verify",
            at(&base, Step::Verify, State::Pending, false),
            false,
            old,
            forward,
        ),
        (
            "verify cut off by a reboot",
            at(&base, Step::Verify, State::Pending, false),
            true,
            old,
            back,
        ),
        (
            "verify committed, then a reboot",
            at(&base, Step::Verify, State::Pending, true),
            true,
            old,
            forward,
        ),
        (
            "rollback",
            at(&base, Step::Rollback, State::Pending, false),
            false,
            old,
            back,
        ),
        (
            "rollback after a reboot",
            at(&base, Step::Rollback, State::Pending, true),
            true,
            old,
            back,
        ),
        (
            "rollback done",
            at(&base, Step::Rollback, done(), false),
            false,
            old,
            back,
        ),
        (
            "cleanup done",
            at(&base, Step::Cleanup, done(), true),
            false,
            old,
            forward,
        ),
        (
            "cleanup after a reboot",
            at(&base, Step::Cleanup, State::Pending, true),
            true,
            old,
            forward,
        ),
        ("the post-success watcher", watching, false, old, forward),
        (
            "finished",
            at(&base, Step::Cutover, finished, true),
            false,
            old,
            None,
        ),
        (
            "a first install",
            at(&base, Step::Cutover, State::Pending, false),
            false,
            None,
            forward,
        ),
        (
            "a version that is no version",
            at(&base, Step::Cutover, State::Pending, false),
            false,
            Some("not a version"),
            forward,
        ),
    ];
    for (row, rec, rebooted, previous, side) in rows {
        let from = previous.filter(|text| Version::new(text).is_ok());
        let expected = side.map(|side| intent(from, side));
        assert_eq!(
            switch_intent(&web(), &rec, rebooted, previous),
            expected,
            "{row}"
        );
    }

    let mut unnamed = at(&base, Step::Cutover, State::Pending, false);
    unnamed.target_version = "x y".to_owned();
    assert_eq!(switch_intent(&web(), &unnamed, false, old), None);
    let mut unnamed_force = at(&base, Step::Cutover, State::Pending, false);
    let mut force = forcing(ForcePhase::Switched, old, None);
    force.target_version = "x y".to_owned();
    unnamed_force.force = Some(force);
    assert_eq!(switch_intent(&web(), &unnamed_force, false, None), None);

    // A force: only one that began on another version and passed the switch of `current`.
    let force_rows = [
        (ForcePhase::Started, old, None, None),
        (ForcePhase::UnitsStopped, old, None, None),
        (ForcePhase::Unlinked, old, None, None),
        (ForcePhase::Switched, old, None, forward),
        (ForcePhase::Linked, old, None, forward),
        (ForcePhase::Reloaded, old, None, forward),
        (ForcePhase::UnitsStarted, old, None, forward),
        (
            ForcePhase::Switched,
            old,
            Some(ForceVerdict::Succeeded),
            None,
        ),
        (ForcePhase::Switched, Some("2.0.0"), None, None),
        (ForcePhase::Switched, None, None, forward),
        (ForcePhase::Switched, Some("x y"), None, forward),
    ];
    for (phase, from, verdict, side) in force_rows {
        let mut rec = at(&base, Step::Cutover, State::Pending, false);
        rec.force = Some(forcing(phase, from, verdict));
        let named = from.filter(|text| Version::new(text).is_ok());
        let expected = side.map(|side| intent(named, side));
        assert_eq!(
            switch_intent(&web(), &rec, true, None),
            expected,
            "{phase:?} {from:?}"
        );
    }
}

/// Catches an intent made from a record the start cannot trust: the intents of the state
/// directory are those of the records that load and name their own service; one whose params
/// file cannot be read gives none (a first install cannot be told from an update without it); a
/// force record needs no params file.
#[test]
fn the_intents_are_read_from_the_records_on_disk() {
    let rig = Rig::new("intent-disk");
    rig.add("web", "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose("web", Step::Cutover);
    let reply = daemon.order(&update("web", "2.0.0", &d(), 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed), "{reply:?}");
    assert_eq!(
        daemon.switch_intents(false).expect("listed"),
        [intent(Some(BASE), Side::Forward)]
    );
    assert_eq!(
        daemon.switch_intents(true).expect("listed"),
        [intent(Some(BASE), Side::Back)]
    );

    // Records that do not count: another service's, one that does not parse, a directory with
    // none, and a file among the directories.
    let rec = rig.record("web").expect("the record");
    let text = fs::read_to_string(rig.state.join("web").join(RECORD_FILE)).expect("the record");
    for (dir, body) in [("api", text.as_str()), ("db", "{ not json")] {
        fs::create_dir_all(rig.state.join(dir)).expect("a service dir");
        fs::write(rig.state.join(dir).join(RECORD_FILE), body).expect("a record");
    }
    fs::create_dir_all(rig.state.join("empty")).expect("a service dir");
    fs::write(rig.state.join("stray"), "x").expect("a stray file");
    assert_eq!(
        daemon.switch_intents(false).expect("listed"),
        [intent(Some(BASE), Side::Forward)]
    );

    fs::remove_file(&rec.params).expect("remove the params file");
    assert!(daemon.switch_intents(false).expect("listed").is_empty());

    let mut forced = rec.clone();
    forced.force = Some(forcing(ForcePhase::Switched, Some(BASE), None));
    RecordStore::new(&rig.state.join("web"))
        .save(&forced)
        .expect("save the record");
    assert_eq!(
        daemon.switch_intents(false).expect("listed"),
        [intent(Some(BASE), Side::Forward)]
    );
}

/// `web` installed at 1.0.0 (placing `web-old.service`) by a daemon that started, and 2.0.0
/// ready (placing `web-new.service`).
fn releases(rig: &Rig) {
    let (daemon, _) = rig.booted();
    rig.add_units(
        "web",
        "1.0.0",
        &["web.service", "web-old.service"],
        &["web.service"],
        &[],
    );
    rig.add_units(
        "web",
        "2.0.0",
        &["web.service", "web-new.service"],
        &["web.service"],
        &[],
    );
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());
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

fn link(rig: &Rig, link: &str) -> Option<String> {
    fs::read_link(rig.packages.join("web").join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

fn placed(record: &PlacedRecord, version: &str) -> bool {
    record
        .paths
        .values()
        .all(|entry| (entry.version == version) & (entry.state == PathState::Placed))
}

/// Catches a daemon killed in the switch to the target that leaves the host half switched: the
/// `placed.json` and the filesystem fail at each write of an update in turn; every update that
/// stopped in its switch reads as a switch to finish forward, the start before the records are
/// read finishes it (`current` names 2.0.0, its paths placed, the old unit gone), and the start
/// then runs the update to `Succeeded`.
#[test]
fn a_crash_in_the_switch_to_the_target_is_finished_forward_at_start() {
    let mut interrupted = 0;
    for n in 1..=40 {
        let rig = Rig::new(&format!("intent-forward-{n}"));
        releases(&rig);
        let failing = Arc::new(FaultFs::failing_at(
            rig.scratch.path(),
            n,
            rig.placed_log.clone(),
        ));
        let reply =
            rig.daemon_with(failing, rig.units.clone())
                .order(&update("web", "2.0.0", &d(), 2));
        if reply.refusal() != Some(Refusal::LinkFailed) {
            assert!(matches!(reply, Reply::Finished { .. }), "{n}: {reply:?}");
            continue;
        }
        interrupted += 1;
        let healthy = rig.daemon();
        assert_eq!(
            healthy.switch_intents(false).expect("listed"),
            [intent(Some("1.0.0"), Side::Forward)],
            "{n}"
        );
        let mut report = BootReport::default();
        before_records(&healthy, &mut report);
        let reconciled = report.reconciled.expect("it ran").expect("it went through");
        assert!(
            reconciled.conflicts.is_empty() & reconciled.unresolved.is_empty(),
            "{n}: {reconciled:?}"
        );
        assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"), "{n}");
        let record = rig.placed_record();
        assert!(placed(&record, "2.0.0"), "{n}: {record:?}");
        assert!(rig.there("/units/web-new.service"), "{n}");
        assert!(!rig.there("/units/web-old.service"), "{n}");

        let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
        assert_eq!(rig.verdict("web"), Some(ok()), "{n}");
        drop(started);
    }
    assert!(interrupted > 0, "no update stopped in its switch");
}

/// Catches a daemon killed in the switch back that leaves the new release's paths: `daemon-reload`
/// fails once `verify` has failed, so the switch back is half done, with `rollback` not run;
/// the record reads as a switch to undo, the start before the records are read puts `current`
/// and the paths on 1.0.0 and reloads, and the start then runs `rollback` to a failed verdict.
#[test]
fn a_crash_in_the_switch_back_is_undone_at_start() {
    let rig = Rig::new("intent-back");
    releases(&rig);
    let units = Arc::new(FakeUnitControl::new(rig.placed_log.clone()));
    let breaks = Arc::clone(&units);
    let ports = Ports {
        launcher: Tap::before(rig.launcher.clone(), rig.placed_log.clone(), move |step| {
            if step == Step::Verify {
                breaks.fail_reload();
            }
        }),
        units,
        ..rig.ports()
    };
    rig.launcher.exits("web", Step::Verify, &[1]);
    let reply =
        Daemon::new(config(), rig.dirs(), ports, OWN).order(&update("web", "2.0.0", &d(), 2));
    assert_eq!(reply.refusal(), Some(Refusal::LinkFailed), "{reply:?}");
    assert_eq!(rig.launcher.steps("web").last(), Some(&Step::Verify));
    assert!(rig.placed_record().reload_pending);

    let healthy = rig.daemon();
    assert_eq!(
        healthy.switch_intents(false).expect("listed"),
        [intent(Some("1.0.0"), Side::Back)]
    );
    let mut report = BootReport::default();
    before_records(&healthy, &mut report);
    let reconciled = report.reconciled.expect("it ran").expect("it went through");
    assert!(reconciled.reloaded, "{reconciled:?}");
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
    let record = rig.placed_record();
    assert!(
        placed(&record, "1.0.0") & !record.reload_pending,
        "{record:?}"
    );
    assert!(rig.there("/units/web-old.service") & !rig.there("/units/web-new.service"));

    start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let verdict = rig.verdict("web").expect("the update has its verdict");
    assert!(
        matches!(verdict.verdict, Verdict::Failed { .. }),
        "{verdict:?}"
    );
    assert_eq!(rig.launcher.steps("web").last(), Some(&Step::Rollback));
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
}
