//! The hookless `force` on a host (design section 5.6, the host's half): what it refuses and when,
//! the checks it skips, a running step it stops, the order of its parts, the units it starts and
//! waits for, and how it fails.

mod support;

use std::fs;
use std::sync::Arc;
use std::thread;

use hostkeeper_daemon::checks::{Check, Checks};
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::packages::PackageError;
use hostkeeper_daemon::testing::{Log, NoPrune, manifest};
use hostkeeper_daemon::verb::{DIGEST_RULE, HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Placement, Ports, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs, Log as PlacedLog};
use hostkeeper_steps::record::{PARAMS_FILE, RECORD_FILE};
use hostkeeper_steps::{Clock, Finished, ForceFailure, ForceVerdict};
use support::force::{
    UNIT, WEB, asked_active, calls, daemon_on, failed, forced, link_of, set, started, succeeded,
    token_of, web, writes_of_step_12,
};
use support::{OWN, Rig, TARGET, config, d, digest, force, lines, name, roots, snapshot, update};

/// A regular file at `path` below the rig's root, which no package placed.
fn occupy(rig: &Rig, path: &str) {
    let file = rig.scratch.path().join(path.trim_start_matches('/'));
    fs::create_dir_all(file.parent().expect("a parent")).expect("its directory");
    fs::write(file, "mine\n").expect("the file");
}

/// Catches a force of `hostkeeper` itself, one without a digest, or one whose service's halt
/// generation or step record cannot be read reaching the package port, a lock or the disk: each
/// is refused with its token, and nothing is asked, placed or written.
#[test]
fn force_is_refused_for_hostkeeper_and_without_a_registered_digest() {
    let rig = Rig::new("force-refused");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&force("hostkeeper", "2.0.0", &d(), 1, 1)),
        Reply::Refused {
            refusal: Refusal::SelfService,
            detail: "hostkeeper updates itself by its own path, never by a force".to_owned(),
        }
    );
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", "", 1, 1)),
        Reply::Refused {
            refusal: Refusal::InvalidOrder,
            detail: format!("the order's force.package_digest {DIGEST_RULE}"),
        }
    );
    assert!(!rig.state.exists());
    for (service, file, text, refusal) in [
        ("api", "held-halt", "two\n", Refusal::HaltUnreadable),
        ("db", RECORD_FILE, "{", Refusal::RecordUnreadable),
    ] {
        let dir = rig.state.join(service);
        fs::create_dir_all(&dir).expect("the service's directory");
        fs::write(dir.join(file), text).expect("the file");
        let reply = daemon.order(&force(service, "2.0.0", &d(), 1, 1));
        assert_eq!(reply.refusal(), Some(refusal), "{reply:?}");
    }
    assert!(rig.store.asked().is_empty());
    assert!(rig.placed_log.events().is_empty());
    for service in ["hostkeeper", WEB, "api", "db"] {
        assert_eq!(daemon.ctx().locks.holder(&name(service)), None, "{service}");
    }
}

/// Catches a refactor that makes a force skip a check it must make, or make one it must skip:
/// the list skips exactly `busy` and the halt; each other check refuses a force with its token
/// and places nothing; and a force runs past an unfinished hooked update of another package and
/// past a standing halt, both of which refuse an `update`.
#[test]
fn force_skips_only_busy_and_the_standing_halt() {
    for check in Check::ALL {
        let skipped = matches!(check, Check::Busy | Check::Halt);
        assert_eq!(Checks::for_force().skips(check), skipped, "{check:?}");
        assert!(!Checks::hooked().skips(check), "{check:?}");
    }
    assert_eq!(Checks::for_force().skipped(), [Check::Busy, Check::Halt]);
    assert!(Checks::hooked().skipped().is_empty());

    let rig = Rig::new("force-checks");
    let daemon = rig.daemon();
    let too_new =
        manifest(WEB, "2.4.0").replace("min_hostkeeper = \"0.1.0\"", "min_hostkeeper = \"0.2.0\"");
    rig.add_with(WEB, "2.1.0", &manifest("api", "2.1.0"));
    rig.add_with(WEB, "2.2.0", &manifest(WEB, "2.9.0"));
    rig.add_with(WEB, "2.4.0", &too_new);
    rig.store.fail(
        WEB,
        "2.5.0",
        PackageError::Other("an entry of the archive is a device".to_owned()),
    );
    rig.add_units(
        WEB,
        "2.6.0",
        &[UNIT],
        &[UNIT],
        &[("web.conf", "/etc/web/web.conf")],
    );
    occupy(&rig, "/etc/web/web.conf");
    let rows = [
        (force("we b", "2.0.0", &d(), 1, 0), "invalid_order"),
        (force(WEB, "2.0.0", &digest("AB"), 1, 0), "invalid_order"),
        (force(WEB, "2.1.0", &d(), 1, 0), "manifest_mismatch"),
        (force(WEB, "2.2.0", &d(), 1, 0), "manifest_mismatch"),
        (force(WEB, "2.4.0", &d(), 1, 0), "daemon_too_old"),
        (force(WEB, "2.5.0", &d(), 1, 0), "package_failed"),
        (force(WEB, "2.6.0", &d(), 1, 0), "path_conflict"),
    ];
    for (order, token) in rows {
        let reply = daemon.order(&order);
        assert_eq!(
            token_of(&reply).as_deref(),
            Some(token),
            "{order:?}: {reply:?}"
        );
    }
    assert!(rig.placed_log.events().is_empty());
    assert!(!rig.state.join(WEB).join(RECORD_FILE).exists());

    // Busy: the unfinished update holds every other update off, and the force stops it.
    let rig = Rig::new("force-busy");
    rig.add(WEB, "1.0.0");
    web(&rig, "3.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose(WEB, Step::Prepare);
    let lost = daemon.order(&update(WEB, "1.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    let busy = daemon.order(&update(WEB, "3.0.0", &d(), 2));
    assert_eq!(busy.refusal(), Some(Refusal::Busy), "{busy:?}");
    assert_eq!(daemon.order(&force(WEB, "3.0.0", &d(), 3, 0)), succeeded());
    assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);

    // The halt: a halted service refuses every update, and the force of the held generation runs.
    let rig = Rig::new("force-halted");
    rig.add(WEB, "1.0.0");
    web(&rig, "3.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.exits(WEB, Step::Prepare, &[76]);
    let halted = daemon.order(&update(WEB, "1.0.0", &d(), 1));
    assert!(
        matches!(
            halted,
            Reply::Finished {
                verdict: Verdict::Halt,
                ..
            }
        ),
        "{halted:?}"
    );
    let refused = daemon.order(&update(WEB, "3.0.0", &d(), 2));
    assert_eq!(refused.refusal(), Some(Refusal::Halted), "{refused:?}");
    assert_eq!(daemon.order(&force(WEB, "3.0.0", &d(), 3, 0)), succeeded());
}

/// Catches a floor of the host's own: with 3.0.0 running, a force to 2.0.0 runs to `Succeeded`,
/// `previous` names 3.0.0, and the downgrade adds no path to the state directory or beside
/// `placed.json`.
#[test]
fn force_to_an_older_version_is_accepted_with_no_local_floor() {
    let rig = Rig::new("force-older");
    let daemon = rig.daemon();
    web(&rig, "3.0.0");
    web(&rig, "2.0.0");
    forced(&rig, &daemon, "3.0.0", 1);
    let paths = |rig: &Rig| -> Vec<std::path::PathBuf> {
        let mut paths: Vec<_> = snapshot(&rig.state)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        paths.extend(
            snapshot(&rig.scratch.path().join("placed"))
                .into_iter()
                .map(|(path, _)| path),
        );
        paths
    };
    let before = paths(&rig);
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    assert_eq!(link_of(&rig, WEB, "previous").as_deref(), Some("3.0.0"));
    assert_eq!(paths(&rig), before);
}

/// Catches a change to the host before the target is had and checked (design section 8.10,
/// "Force of a version not on the host"): a package the port refuses (`digest_mismatch`,
/// `no_space`) and one whose file is occupied (`path_conflict`) each fail before the commit with
/// nothing stopped, unlinked, switched or linked, `current` and the record as they were.
#[test]
fn force_fetches_and_verifies_before_it_stops_anything() {
    let rig = Rig::new("force-fetch");
    let daemon = rig.daemon();
    web(&rig, "1.0.0");
    forced(&rig, &daemon, "1.0.0", 1);
    let record = fs::read(rig.state.join(WEB).join(RECORD_FILE)).expect("the record");
    rig.store.fail(
        WEB,
        "2.0.0",
        PackageError::DigestMismatch("the bytes are another digest".to_owned()),
    );
    rig.store.fail(
        WEB,
        "2.1.0",
        PackageError::NoSpace("the disk is full".to_owned()),
    );
    rig.add_units(
        WEB,
        "2.2.0",
        &[UNIT],
        &[UNIT],
        &[("web.conf", "/etc/web/web.conf")],
    );
    occupy(&rig, "/etc/web/web.conf");
    let release = |reason| failed(Phase::BeforeCommit, Class::Release, reason);
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        release("digest_mismatch")
    );
    assert_eq!(
        daemon.order(&force(WEB, "2.1.0", &d(), 3, 0)),
        failed(Phase::BeforeCommit, Class::Environment, "no_space")
    );
    assert_eq!(
        daemon.order(&force(WEB, "2.2.0", &d(), 4, 0)),
        release("path_conflict")
    );
    assert!(rig.placed_log.events().is_empty());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
    assert_eq!(rig.units.active(), set(&[UNIT]));
    assert_eq!(
        fs::read(rig.state.join(WEB).join(RECORD_FILE)).expect("the record"),
        record
    );
    let asked = [("web", "2.0.0"), ("web", "2.1.0")].map(|(s, v)| (s.to_owned(), v.to_owned()));
    assert_eq!(rig.store.asked(), asked);
}

/// Catches a halt lifted before the pre-checks, or ignored (design section 5.4): with `web` and
/// `api` halted and held at generation 2, a force with generation 3 that fails its fetch leaves
/// the hold at 2 and the halt in place; one that passes raises the hold to 3, clears the halt
/// and runs; and a force of `api` with generation 2 runs and leaves its hold at 2.
#[test]
fn force_over_a_standing_halt_lifts_the_halt() {
    let rig = Rig::new("force-lift");
    for service in [WEB, "api"] {
        rig.add(service, "1.0.0");
        rig.add_units(service, "3.0.0", &[], &[], &[]);
        rig.launcher.exits(service, Step::Prepare, &[76]);
    }
    let (daemon, _) = rig.booted();
    let halts = &daemon.ctx().halts;
    for service in [WEB, "api"] {
        let halted = daemon.order(&update(service, "1.0.0", &d(), 1));
        assert!(
            matches!(
                halted,
                Reply::Finished {
                    verdict: Verdict::Halt,
                    ..
                }
            ),
            "{halted:?}"
        );
        halts
            .raise(&name(service), 2)
            .expect("hold the service at 2");
    }
    rig.store.fail(
        WEB,
        "2.0.0",
        PackageError::DigestMismatch("other bytes".to_owned()),
    );
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 3)),
        failed(Phase::BeforeCommit, Class::Release, "digest_mismatch")
    );
    assert_eq!(halts.get(&name(WEB)).ok(), Some(2));
    assert!(matches!(
        rig.verdict(WEB),
        Some(Finished {
            verdict: Verdict::Halt,
            ..
        })
    ));
    assert_eq!(daemon.order(&force(WEB, "3.0.0", &d(), 3, 3)), succeeded());
    assert_eq!(halts.get(&name(WEB)).ok(), Some(3));
    assert_eq!(
        daemon.order(&force("api", "3.0.0", &d(), 2, 2)),
        succeeded()
    );
    assert_eq!(halts.get(&name("api")).ok(), Some(2));
}

/// Catches a lift whose write fails being passed over: the force is refused `store_failed` and
/// nothing is placed or switched.
#[test]
fn a_halt_lift_that_cannot_be_written_stops_the_force() {
    let rig = Rig::new("force-lift-fails");
    let daemon = rig.daemon();
    web(&rig, "2.0.0");
    fs::create_dir_all(rig.state.join(WEB).join("held-halt.tmp")).expect("a directory in the way");
    let reply = daemon.order(&force(WEB, "2.0.0", &d(), 1, 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(rig.placed_log.events().is_empty());
    assert_eq!(link_of(&rig, WEB, "current"), None);
}

/// Catches a force that stops the wrong unit, waits for the step to end by itself, or closes the
/// update another way (design section 8.10, "Force during a hooked update"): the `cutover` held
/// in the launcher is stopped by the name its record gives, the update ends `Failed`,
/// `forced_over`, with no `rollback` launched and its params file gone, and the force then runs.
#[test]
fn force_stops_a_running_step_and_closes_its_record_forced_over() {
    let rig = Rig::new("force-over");
    rig.add(WEB, "1.0.0");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    let daemon = Arc::new(daemon);
    let gate = rig.launcher.hold(WEB, Step::Cutover);
    let stops = rig.launcher.stops_seen();
    let updating = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update(WEB, "1.0.0", &d(), 1)))
    };
    gate.entered();
    // The force's first wait for the lock is held until the update has ended, so that it stops
    // the step once.
    let waiting = rig.clock.park_next_sleep();
    let forcing = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)))
    };
    assert_eq!(stops.recv().expect("a stop"), "updater-web-cutover-1");
    waiting.entered();
    gate.release();
    assert_eq!(
        updating.join().expect("the update"),
        failed(Phase::BeforeCommit, Class::Environment, "forced_over")
    );
    waiting.release();
    assert_eq!(forcing.join().expect("the force"), succeeded());
    assert_eq!(rig.launcher.steps(WEB), [Step::Prepare, Step::Cutover]);
    assert_eq!(rig.launcher.stops(), ["updater-web-cutover-1"]);
    assert!(!rig.state.join(WEB).join(PARAMS_FILE).exists());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
}

/// Catches a runner that launches `rollback` or `cleanup` for an update a force closed: a
/// `prepare` held in the launcher, with no dead-man, is stopped by the force, and the launcher's
/// log holds that one run and that one stop.
#[test]
fn force_interrupts_a_running_prepare_and_the_runner_launches_no_rollback() {
    let rig = Rig::new("force-prepare");
    rig.add(WEB, "1.0.0");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    let daemon = Arc::new(daemon);
    let log = Log::default();
    rig.launcher.log_to(log.clone());
    let gate = rig.launcher.hold(WEB, Step::Prepare);
    let stops = rig.launcher.stops_seen();
    let updating = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update(WEB, "1.0.0", &d(), 1)))
    };
    gate.entered();
    let waiting = rig.clock.park_next_sleep();
    let forcing = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)))
    };
    assert_eq!(stops.recv().expect("a stop"), "updater-web-prepare-1");
    waiting.entered();
    gate.release();
    assert_eq!(
        updating.join().expect("the update"),
        failed(Phase::BeforeCommit, Class::Environment, "forced_over")
    );
    waiting.release();
    assert_eq!(forcing.join().expect("the force"), succeeded());
    assert_eq!(
        lines(&log),
        ["run updater-web-prepare-1", "stop updater-web-prepare-1"]
    );
}

/// Catches a force that touches another service's step, or waits on the step lock: with `api`'s
/// `prepare` holding the step lock, a force of `web` runs to `Succeeded`, `api`'s record keeps
/// its bytes and no unit of `api` is stopped; `api`'s update then ends as it would have.
#[test]
fn force_does_not_wait_for_another_services_step() {
    let rig = Rig::new("force-other");
    rig.add("api", "1.0.0");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    let daemon = Arc::new(daemon);
    let gate = rig.launcher.hold("api", Step::Prepare);
    let updating = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("api", "1.0.0", &d(), 1)))
    };
    gate.entered();
    let record = rig.state.join("api").join(RECORD_FILE);
    let before = fs::read(&record).expect("api's record");
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 1, 0)), succeeded());
    assert_eq!(fs::read(&record).expect("api's record"), before);
    assert!(rig.launcher.stops().is_empty());
    gate.release();
    assert_eq!(updating.join().expect("the update"), succeeded());
}

/// Catches a force that lets go of its service's lock before its verdict, or queues what comes
/// meanwhile: while the force waits for its unit (step 15), an update of `web` is `busy` at once,
/// the same force again is `running`, and an update of `api` runs to its end.
#[test]
fn force_takes_the_service_lock_only_and_a_second_order_for_the_service_is_busy_not_queued() {
    let rig = Rig::new("force-busy-not-queued");
    web(&rig, "2.0.0");
    rig.add("api", "1.0.0");
    let (daemon, _) = rig.booted();
    let daemon = Arc::new(daemon);
    rig.units.active_after(UNIT, 1);
    let parked = rig.clock.park_next_sleep();
    let forcing = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&force(WEB, "2.0.0", &d(), 1, 0)))
    };
    parked.entered();
    assert_eq!(
        daemon.order(&update(WEB, "2.0.0", &d(), 2)),
        Reply::Refused {
            refusal: Refusal::Busy,
            detail: "web is locked by an order to force it to 2.0.0".to_owned(),
        }
    );
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 1, 0)),
        Reply::Running
    );
    assert_eq!(daemon.order(&update("api", "1.0.0", &d(), 1)), succeeded());
    parked.release();
    assert_eq!(forcing.join().expect("the force"), succeeded());
}

/// Catches a part of a force run out of its order (a start before `daemon-reload`, a link before
/// the switch, an unlink after it): for a target that drops a unit and a file and adds one of
/// each, the calls are the stop of the old `run` units, the stop, disable and unlink of the
/// dropped unit and the unlink of the dropped file with their reload, the switch, the links of
/// the new file, `WantedBy=` link and unit, `daemon-reload`, and the starts.
#[test]
fn force_runs_the_steps_in_the_documented_order() {
    let rig = Rig::new("force-order");
    let daemon = rig.daemon();
    let old = ["web.service", "web-gone.service"];
    let new = ["web.service", "web-new.service"];
    rig.add_units(
        WEB,
        "1.0.0",
        &old,
        &old,
        &[("gone.conf", "/etc/web/gone.conf")],
    );
    rig.add_units(
        WEB,
        "2.0.0",
        &new,
        &new,
        &[("new.conf", "/etc/web/new.conf")],
    );
    forced(&rig, &daemon, "1.0.0", 1);
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    let wants = format!("/units/{TARGET}.wants");
    assert_eq!(
        calls(&rig),
        [
            "stop web-gone.service".to_owned(),
            "stop web.service".to_owned(),
            "stop web-gone.service".to_owned(),
            format!("unlink {wants}/web-gone.service"),
            "unlink /etc/web/gone.conf".to_owned(),
            "unlink /units/web-gone.service".to_owned(),
            "reload".to_owned(),
            "switch".to_owned(),
            "link /etc/web/new.conf".to_owned(),
            format!("link {wants}/web-new.service"),
            "link /units/web-new.service".to_owned(),
            "reload".to_owned(),
            "start web.service".to_owned(),
            "start web-new.service".to_owned(),
        ]
    );
}

/// Catches the old manifest's `run` list used for the starts, or every unit of the target
/// started: only the target's `run` unit is started and active; its other unit is linked and not
/// started.
#[test]
fn force_starts_exactly_the_units_the_target_lists() {
    let rig = Rig::new("force-starts");
    let daemon = rig.daemon();
    rig.add_units(
        WEB,
        "1.0.0",
        &["web-old.service"],
        &["web-old.service"],
        &[],
    );
    rig.add_units(WEB, "2.0.0", &[UNIT, "web-extra.service"], &[UNIT], &[]);
    forced(&rig, &daemon, "1.0.0", 1);
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    assert_eq!(started(&rig), [UNIT]);
    assert_eq!(rig.units.active(), set(&[UNIT]));
    let extra = rig.scratch.path().join("units").join("web-extra.service");
    let meta = fs::symlink_metadata(extra).expect("the unit is linked");
    assert!(meta.file_type().is_symlink());
}

/// Catches a wait that ends on the first active unit, ends early, or reads another clock: with
/// `force_start_secs` 60, one unit active at once is asked after once, and another that turns
/// active on its 61st ask passes, on its 62nd fails `start_failed`; either way the clock moved 60
/// seconds, by the force's polls alone.
#[test]
fn force_waits_for_every_unit_to_be_active_within_force_start_secs() {
    let two = "web-two.service";
    let failed_start = failed(Phase::AfterCommit, Class::Release, "start_failed");
    for (asks, expected) in [(60, succeeded()), (61, failed_start)] {
        let rig = Rig::new("force-wait");
        let daemon = rig.daemon();
        rig.add_units(WEB, "2.0.0", &[UNIT, two], &[UNIT, two], &[]);
        rig.units.active_after(two, asks);
        let began = rig.clock.mono_secs();
        assert_eq!(
            daemon.order(&force(WEB, "2.0.0", &d(), 1, 0)),
            expected,
            "{asks}"
        );
        assert_eq!(rig.clock.mono_secs() - began, 60, "{asks}");
        assert_eq!(asked_active(&rig, UNIT), 1, "{asks}");
        assert_eq!(asked_active(&rig, two), 61, "{asks}");
    }
}

/// Catches a force that rolls back after a failed start, or fetches the version it left again
/// (design section 8.7): the first force ends `Failed`, `start_failed`, on the target with
/// `previous` the version left and nothing started; a second force to that version asks the
/// package port for nothing and runs to `Succeeded`.
#[test]
fn force_with_unit_that_fails_to_start_reports_failed_and_a_second_force_recovers() {
    let rig = Rig::new("force-recovers");
    let daemon = rig.daemon();
    let two = "web-two.service";
    web(&rig, "1.0.0");
    rig.add_units(WEB, "2.0.0", &[two], &[two], &[]);
    rig.units.fail_start(two);
    forced(&rig, &daemon, "1.0.0", 1);
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        failed(Phase::AfterCommit, Class::Release, "start_failed")
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    assert_eq!(link_of(&rig, WEB, "previous").as_deref(), Some("1.0.0"));
    assert!(rig.units.active().is_empty());
    let record = rig.record(WEB).expect("the record");
    let verdict = record.force.and_then(|force| force.verdict);
    assert_eq!(
        verdict,
        Some(ForceVerdict::Failed {
            reason: ForceFailure::StartFailed
        })
    );
    assert_eq!(started(&rig), [two]);
    assert_eq!(daemon.order(&force(WEB, "1.0.0", &d(), 3, 0)), succeeded());
    assert!(rig.store.asked().is_empty());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
    assert_eq!(link_of(&rig, WEB, "previous").as_deref(), Some("2.0.0"));
    assert_eq!(rig.units.active(), set(&[UNIT]));
}

/// `web` at 1.0.0, forced there, with 2.0.0 beside it, which places a file besides its unit.
fn with_a_file(name: &str) -> Rig {
    let rig = Rig::new(name);
    web(&rig, "1.0.0");
    rig.add_units(
        WEB,
        "2.0.0",
        &[UNIT],
        &[UNIT],
        &[("web.conf", "/etc/web/web.conf")],
    );
    forced(&rig, &rig.daemon(), "1.0.0", 1);
    rig
}

/// Catches a failure to link or to reload reported as another reason, rolled back, or followed
/// by a start: an injected write error of step 12 ends `Failed`, `link_failed`, and a failing
/// `daemon-reload` `Failed`, `daemon_reload_failed`, each after the commit with `current` on the
/// target and no unit started.
#[test]
fn force_records_link_failed_and_daemon_reload_failed_with_the_host_on_the_target() {
    let (first, _) = writes_of_step_12(with_a_file, "web.conf");
    let rig = with_a_file("force-link-fails");
    let fs = Arc::new(FaultFs::failing_at(
        rig.scratch.path(),
        first,
        PlacedLog::new(),
    ));
    let daemon = daemon_on(&rig, fs, Arc::new(NoPrune));
    let (daemon, alerts) = support::alerting(daemon);
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        failed(Phase::AfterCommit, Class::Environment, "link_failed")
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    assert!(started(&rig).is_empty());
    let alerted = lines(&alerts);
    assert!(
        alerted
            .iter()
            .any(|line| line.contains("alert=force_failed service=web: link_failed")),
        "{alerted:?}"
    );

    let rig = Rig::new("force-reload-fails");
    let daemon = rig.daemon();
    web(&rig, "1.0.0");
    web(&rig, "2.0.0");
    forced(&rig, &daemon, "1.0.0", 1);
    rig.units.fail_reload();
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        failed(
            Phase::AfterCommit,
            Class::Environment,
            "daemon_reload_failed"
        )
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    assert!(started(&rig).is_empty());
    assert_eq!(
        rig.record(WEB)
            .and_then(|record| record.force)
            .and_then(|force| force.verdict),
        Some(ForceVerdict::Failed {
            reason: ForceFailure::DaemonReloadFailed
        })
    );
}

/// Catches a stop of the version left that fails being passed over, or reported after the
/// commit: the service manager refuses every call, and the force ends `Failed`, `link_failed`,
/// before the commit, with `current` where it was.
#[test]
fn a_force_whose_units_cannot_be_stopped_fails_before_the_switch() {
    let rig = Rig::new("force-stop-fails");
    web(&rig, "1.0.0");
    web(&rig, "2.0.0");
    forced(&rig, &rig.daemon(), "1.0.0", 1);
    let ports = Ports {
        units: Arc::new(FakeUnitControl::failing(PlacedLog::new())),
        ..rig.ports()
    };
    let daemon = Daemon::new(config(), rig.dirs(), ports, OWN);
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        failed(Phase::BeforeCommit, Class::Environment, "link_failed")
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
}

/// Catches a version left whose manifest no longer reads keeping its units running across the
/// switch: the unit `placed.json` holds enabled for it is stopped before the switch all the
/// same, and started again from the target.
#[test]
fn a_force_stops_the_enabled_units_of_a_version_whose_manifest_no_longer_reads() {
    let rig = Rig::new("force-broken-left");
    let daemon = rig.daemon();
    let left = rig.add_units(WEB, "1.0.0", &[UNIT], &[UNIT], &[]);
    web(&rig, "2.0.0");
    forced(&rig, &daemon, "1.0.0", 1);
    fs::write(left.join("manifest.toml"), "not a manifest").expect("break the manifest");
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    let calls = calls(&rig);
    let stop = calls.iter().position(|call| *call == "stop web.service");
    let switch = calls.iter().position(|call| call == "switch");
    assert!(stop.is_some() & (stop < switch), "{calls:?}");
    assert_eq!(rig.units.active(), set(&[UNIT]));
}

/// Catches a force that cannot stop the running step and goes on, or gives up after one try:
/// the stop is tried twice by name, the force ends `Failed`, `unstopped`, with an alert, and
/// nothing is switched; the step stays flagged.
#[test]
fn a_force_that_cannot_stop_the_running_step_changes_nothing_else() {
    let rig = Rig::new("force-unstopped");
    rig.add(WEB, "1.0.0");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    let (daemon, alerts) = support::alerting(daemon);
    rig.launcher.lose(WEB, Step::Prepare);
    let lost = daemon.order(&update(WEB, "1.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    rig.launcher.fail_stops(WEB);
    assert_eq!(
        daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)),
        failed(Phase::BeforeCommit, Class::Environment, "unstopped")
    );
    assert_eq!(
        rig.launcher.stops(),
        ["updater-web-prepare-1", "updater-web-prepare-1"]
    );
    assert!(
        lines(&alerts)
            .iter()
            .any(|line| line.contains("alert=unstoppable_unit"))
    );
    // Nothing was switched: `current` still names the release `rig.add` installed.
    assert_eq!(
        link_of(&rig, WEB, "current").as_deref(),
        Some(support::BASE)
    );
    assert!(rig.placed_log.events().is_empty());
    assert!(rig.record(WEB).expect("the record").forced);
}

/// Catches a force held off for good by a lock a step that could not be stopped keeps: once it
/// has stopped that step's unit, the force takes the lock over and runs.
#[test]
fn a_force_takes_over_the_lock_of_a_step_that_could_not_be_stopped() {
    let rig = Rig::new("force-take-over");
    rig.add(WEB, "1.0.0");
    web(&rig, "2.0.0");
    let (daemon, _) = rig.booted();
    rig.launcher.lose(WEB, Step::Prepare);
    let lost = daemon.order(&update(WEB, "1.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    let held = Package {
        verb: HookedVerb::Update,
        version: hostkeeper_config::names::Version::new("1.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    };
    daemon
        .ctx()
        .locks
        .take(&name(WEB), held)
        .expect("the lock")
        .keep();
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);
    assert_eq!(daemon.ctx().locks.holder(&name(WEB)), None);
}

/// Catches steps 4 to 6 writing a record no hooked update holds, so that a stale flag could close
/// the next update, or touching another service: with `web`'s last update ended, its force
/// writes only its own eight journal entries, and the next update of `web` runs its steps; `api`'s
/// unfinished record keeps its bytes and its unit is not stopped.
#[test]
fn a_force_while_no_hooked_step_runs_sets_no_forced_flag() {
    let rig = Rig::new("force-no-flag");
    rig.add(WEB, "1.0.0");
    rig.add("api", "1.0.0");
    web(&rig, "2.0.0");
    rig.add_with(WEB, "3.0.0", &manifest(WEB, "3.0.0"));
    let (daemon, _) = rig.booted();
    assert_eq!(daemon.order(&update(WEB, "1.0.0", &d(), 1)), succeeded());
    rig.launcher.lose("api", Step::Prepare);
    let lost = daemon.order(&update("api", "1.0.0", &d(), 1));
    assert_eq!(lost.refusal(), Some(Refusal::LauncherFailed), "{lost:?}");
    let api = rig.state.join("api").join(RECORD_FILE);
    let before = fs::read(&api).expect("api's record");
    let writes = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = rig
        .daemon()
        .with_stores(support::crash_at(None, Arc::clone(&writes)));
    assert_eq!(counted.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 8);
    assert!(!rig.record(WEB).expect("web's record").forced);
    assert_eq!(fs::read(&api).expect("api's record"), before);
    assert_eq!(rig.launcher.stops(), Vec::<String>::new());
    let steps = rig.launcher.steps(WEB).len();
    assert_eq!(daemon.order(&update(WEB, "3.0.0", &d(), 3)), succeeded());
    assert!(rig.launcher.steps(WEB).len() > steps);
}

/// Catches the placement of the test rig drifting from the daemon's directories: the packages
/// directory the linker reaches is the daemon's own.
#[test]
fn the_rig_links_into_the_daemons_packages_directory() {
    let rig = Rig::new("force-rig");
    let placement = Placement {
        roots: roots(),
        fs: rig.placed_fs.clone(),
    };
    assert_eq!(
        placement.fs.host_path(placement.roots.packages().dir()),
        rig.packages
    );
}
