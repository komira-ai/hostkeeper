//! The hook as the daemon reads it: every step and outcome through the hook contract crate (the
//! exit code through `Outcome`, the last line through both readers, what the updater runs next
//! through `next`), over a fake runner and a fake probe, in a scratch record directory.

mod support;

use std::fs;

use hello::hook::Units;
use hostkeeper_hook::{Class, Context, Next, Outcome, Phase, Step, Verdict};
use support::{
    FakeProbe, FakeRunner, KICK_A, KICK_B, KILL_A, KILL_B, PORT, Ran, START_A, START_B, STOP_A,
    STOP_B, TARGET, Want, World, contract, ctx, files,
};

fn failed(phase: Phase, class: Class) -> Verdict {
    Verdict::Failed { phase, class }
}

fn rollback_then(phase: Phase, class: Class) -> Next {
    Next::Rollback {
        then: failed(phase, class),
    }
}

/// Runs a step that must succeed.
fn go(world: &World, step: Step, runner: &FakeRunner, probe: &FakeProbe) -> Ran {
    let ran = world.step(step, runner, probe);
    assert_eq!(ran.code, 0, "{step} gave {ran:?}");
    ran
}

/// A full update: the four steps, each succeeding.
fn update(world: &World, runner: &FakeRunner, probe: &FakeProbe) {
    for step in [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup] {
        go(world, step, runner, probe);
    }
}

/// What a step that failed told, as the contract reads it: exit 1 with `detail` in the line.
fn failure<'a>(class: Class, reason: Option<&'a str>, detail: &'a str) -> Want<'a> {
    Want {
        class,
        reason,
        detail: Some(detail),
        ..Want::new(Outcome::Failure)
    }
}

/// The world after a first install (active `a`), its runner and probe, with the calls cleared.
fn installed(name: &str, units: Units) -> (World, FakeRunner, FakeProbe) {
    let world = World::new(name, units);
    let (runner, probe) = (FakeRunner::default(), FakeProbe::ready(TARGET));
    update(&world, &runner, &probe);
    runner.take_calls();
    probe.take_calls();
    (world, runner, probe)
}

/// Catches a first install that does not run prepare, cutover, verify and cleanup in the order
/// the updater runs them, with the argv, the markers, the record and the lines the design gives.
#[test]
fn a_first_install_runs_the_four_steps() {
    let world = World::systemd("first-install");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);

    let ran = world.step(Step::Prepare, &runner, &probe);
    let want = Want {
        detail: Some("prepared slot a"),
        ..Want::new(Outcome::Success)
    };
    assert_eq!(
        contract(Step::Prepare, &ran, &want, &ctx()),
        Next::Run(Step::Cutover)
    );
    assert_eq!(
        ran.stdout,
        "{\"class\":\"release\",\"detail\":\"prepared slot a\"}\n"
    );
    assert_eq!(
        world.snapshot(),
        files(&[("next", Some("a")), ("prepared", Some(""))])
    );
    assert!(
        runner.take_calls().is_empty(),
        "prepare changes nothing a client sees"
    );
    assert!(probe.take_calls().is_empty());

    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = Want {
        detail: Some("started slot a"),
        ..Want::new(Outcome::Success)
    };
    assert_eq!(
        contract(Step::Cutover, &ran, &want, &ctx()),
        Next::Run(Step::Verify)
    );
    assert_eq!(runner.take_calls(), [START_A]);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("next", Some("a")),
            ("prepared", Some("")),
            ("cutover", Some("")),
            ("slots", None),
            ("slots/a", Some("")),
        ])
    );

    let ran = world.step(Step::Verify, &runner, &probe);
    let next = contract(Step::Verify, &ran, &Want::new(Outcome::Success), &ctx());
    assert_eq!(next, Next::Run(Step::Cleanup));
    assert_eq!(
        ran.stdout,
        "{\"class\":\"release\",\"promotion_pending\":false}\n"
    );
    assert_eq!(
        probe.take_calls(),
        [(PORT, "/readyz".to_owned()), (PORT, "/version".to_owned())]
    );
    assert!(
        runner.take_calls().is_empty(),
        "verify never changes the units"
    );
    assert!(world.snapshot().contains_key("verified"));

    let ran = world.step(Step::Cleanup, &runner, &probe);
    let next = contract(Step::Cleanup, &ran, &Want::new(Outcome::Success), &ctx());
    assert_eq!(
        next,
        Next::Report(Verdict::Succeeded {
            promotion_pending: false
        })
    );
    assert_eq!(ran.stdout, "{\"class\":\"release\"}\n");
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("verified", Some("")),
            ("slots", None),
            ("slots/a", Some("")),
        ])
    );
    assert!(
        runner.take_calls().is_empty(),
        "cleanup has nothing to stop: cutover did"
    );
}

/// Catches an update that starts the slot that is already serving, stops the wrong one, or does
/// not alternate between the two.
#[test]
fn an_update_alternates_between_the_slots() {
    let (world, runner, probe) = installed("alternate", Units::Systemd);

    let ran = go(&world, Step::Prepare, &runner, &probe);
    assert!(ran.stdout.contains("prepared slot b"), "{ran:?}");
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("next", Some("b")),
            ("prepared", Some("")),
            ("slots", None),
            ("slots/a", Some("")),
        ]),
        "prepare forgets the last update's verified and starts nothing"
    );
    assert!(runner.take_calls().is_empty());

    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [STOP_A, START_B]);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("next", Some("b")),
            ("prepared", Some("")),
            ("cutover", Some("")),
            ("slots", None),
            ("slots/b", Some("")),
        ])
    );
    go(&world, Step::Verify, &runner, &probe);
    go(&world, Step::Cleanup, &runner, &probe);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("b")),
            ("verified", Some("")),
            ("slots", None),
            ("slots/b", Some("")),
        ])
    );

    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [STOP_B, START_A]);
}

/// Catches a verify that reads a server that is up but not ready as ready, or as a fault of the
/// host; and a rollback that does not stop the new slot and start the old one again.
#[test]
fn a_verify_that_finds_the_server_not_ready_is_rolled_back_to_the_old_slot() {
    let (world, runner, probe) = installed("not-ready", Units::Systemd);
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    runner.take_calls();
    probe.answer("/readyz", Ok((503, "not ready\n".to_owned())));

    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failure(Class::Release, Some("not_ready"), "503");
    let next = contract(Step::Verify, &ran, &want, &ctx());
    let then = failed(Phase::GateRelease, Class::Release);
    assert_eq!(next, Next::Rollback { then: then.clone() });
    assert_eq!(
        probe.take_calls(),
        [(PORT, "/readyz".to_owned())],
        "503 ends the verify"
    );
    assert!(!world.snapshot().contains_key("verified"));
    assert!(runner.take_calls().is_empty());

    let ran = world.step(Step::Rollback, &runner, &probe);
    let want = Want {
        reversible: Some(true),
        ..Want::new(Outcome::Success)
    };
    let after = Context {
        then: Some(then.clone()),
        ..ctx()
    };
    assert_eq!(
        contract(Step::Rollback, &ran, &want, &after),
        Next::Report(then)
    );
    assert_eq!(ran.stdout, "{\"class\":\"release\",\"reversible\":true}\n");
    assert_eq!(runner.take_calls(), [STOP_B, START_A]);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("slots", None),
            ("slots/a", Some(""))
        ]),
        "the old slot serves again and the update is forgotten"
    );
}

/// Catches a rollback of a first install (no old slot) that starts a slot, or leaves the new one.
#[test]
fn rolling_back_a_first_install_stops_the_new_slot_and_starts_none() {
    let world = World::systemd("first-rollback");
    let runner = FakeRunner::default();
    let probe = FakeProbe::default();
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    runner.take_calls();

    let ran = go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(ran.stdout, "{\"class\":\"release\",\"reversible\":true}\n");
    assert_eq!(runner.take_calls(), [STOP_A]);
    assert_eq!(
        world.snapshot(),
        files(&[("slots", None)]),
        "no marker is left"
    );
}

/// Catches the rollback of a failed first install failing on a real host: the daemon has stopped
/// and unlinked the units before the rollback runs, so `systemctl stop` answers 5 (not loaded),
/// which is nothing to stop and must not fail the rollback. Any other non-zero stop still fails.
#[test]
fn a_stop_of_a_unit_that_is_not_loaded_is_nothing_to_stop() {
    let world = World::systemd("first-rollback-unlinked");
    let runner = FakeRunner::default();
    let probe = FakeProbe::default();
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    runner.take_calls();

    runner.answer(STOP_A, Ok(5));
    let ran = go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(ran.stdout, "{\"class\":\"release\",\"reversible\":true}\n");
    assert!(
        ran.stderr
            .contains("`/usr/bin/systemctl stop hello@a.service` exited with 5: ignored"),
        "{}",
        ran.stderr
    );
    assert_eq!(runner.take_calls(), [STOP_A]);
    assert_eq!(
        world.snapshot(),
        files(&[("slots", None)]),
        "no marker is left"
    );

    let (world, runner, probe) = installed("start-not-loaded", Units::Systemd);
    go(&world, Step::Prepare, &runner, &probe);
    runner.answer(START_B, Ok(5));
    let ran = world.step(Step::Cutover, &runner, &probe);
    assert_eq!(
        ran.code, 1,
        "a start that answers 5 started nothing: {ran:?}"
    );
}

/// Catches a server that does not answer being a failure (which would roll back at the first
/// refused connection) and not a deferral with the backoff, or one that never ends in a rollback
/// at the deadline.
#[test]
fn a_server_that_does_not_answer_defers_verify_until_the_deadline() {
    let world = World::systemd("not-listening");
    let runner = FakeRunner::default();
    let probe = FakeProbe::default();
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    probe.answer("/readyz", Err("connection refused".to_owned()));

    let ran = world.step(Step::Verify, &runner, &probe);
    assert_eq!(ran.code, 75);
    let want = Want {
        reason: Some("not_listening"),
        retry_after_secs: Some(2),
        detail: Some("connection refused"),
        ..Want::new(Outcome::Deferred)
    };
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(
        next,
        Next::Again {
            step: Step::Verify,
            after_secs: Some(2)
        }
    );
    assert!(ran.stdout.starts_with(
        "{\"class\":\"release\",\"reason\":\"not_listening\",\"retry_after_secs\":2,"
    ));
    assert!(!world.snapshot().contains_key("verified"));

    let late = Context {
        verify_deadline_passed: true,
        ..ctx()
    };
    let next = contract(Step::Verify, &ran, &want, &late);
    assert_eq!(next, rollback_then(Phase::GateRelease, Class::Release));

    // The server answered /readyz and then did not answer /version.
    probe.answer("/readyz", Ok((200, "ready\n".to_owned())));
    probe.answer("/version", Err("connection reset".to_owned()));
    let ran = world.step(Step::Verify, &runner, &probe);
    let want = Want {
        detail: Some("connection reset"),
        ..want
    };
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(
        next,
        Next::Again {
            step: Step::Verify,
            after_secs: Some(2)
        }
    );
}

/// Catches a server that runs another version than the update being verified.
#[test]
fn a_server_on_another_version_fails_verify_as_wrong_version() {
    let world = World::systemd("wrong-version");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready("1.0.0");
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);

    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failure(Class::Release, Some("wrong_version"), "\"1.0.0\"");
    assert!(
        ran.stdout.contains("\\\"1.2.3\\\""),
        "both versions are in the detail: {ran:?}"
    );
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::GateRelease, Class::Release));
    assert!(!world.snapshot().contains_key("verified"));
}

/// Catches a status other than 200 or 503 (from either request) read as ready.
#[test]
fn another_status_fails_verify_as_bad_status() {
    let world = World::systemd("bad-status");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);

    probe.answer("/readyz", Ok((500, "oops\n".to_owned())));
    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failure(
        Class::Release,
        Some("bad_status"),
        "GET /readyz answered 500",
    );
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::GateRelease, Class::Release));

    probe.answer("/readyz", Ok((200, "ready\n".to_owned())));
    probe.answer("/version", Ok((404, "not found\n".to_owned())));
    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failure(
        Class::Release,
        Some("bad_status"),
        "GET /version answered 404",
    );
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::GateRelease, Class::Release));
    assert!(!world.snapshot().contains_key("verified"));
}

/// Catches a cleanup that commits an update verify has not passed, and one that refuses without
/// telling the updater it may still roll back.
#[test]
fn cleanup_before_verify_is_refused_and_leaves_the_rollback_open() {
    let world = World::systemd("cleanup-early");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    let before = world.snapshot();
    runner.take_calls();

    let ran = world.step(Step::Cleanup, &runner, &probe);
    let want = Want {
        reversible: Some(true),
        ..failure(Class::Release, None, "verify has not succeeded")
    };
    let next = contract(Step::Cleanup, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));
    assert_eq!(world.snapshot(), before, "nothing is committed");
    assert!(runner.take_calls().is_empty());
}

/// Catches cutover running before prepare, verify before cutover, and a prepare of a release with
/// no program: each is refused as a fault of the release and changes nothing.
#[test]
fn a_step_before_its_prerequisite_is_refused_and_changes_nothing() {
    let world = World::systemd("prerequisites");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);

    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(Class::Release, None, "prepare has not run");
    let next = contract(Step::Cutover, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));

    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failure(Class::Release, None, "cutover has not run");
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::GateRelease, Class::Release));

    let program = world.artifact().join("bin").join("hello");
    fs::remove_file(&program).expect("remove the program");
    let ran = world.step(Step::Prepare, &runner, &probe);
    let want = failure(Class::Release, Some("bad_artifact"), "no program");
    let next = contract(Step::Prepare, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));

    fs::create_dir(&program).expect("a directory where the program should be");
    let ran = world.step(Step::Prepare, &runner, &probe);
    let next = contract(Step::Prepare, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));

    assert_eq!(world.snapshot(), files(&[]));
    assert!(runner.take_calls().is_empty());
    assert!(probe.take_calls().is_empty());
}

/// Catches a step that is not safe to run again after it ended (the updater runs a step twice
/// after a crash): the second run must succeed, change no record, and ask for the same commands.
#[test]
fn every_step_run_twice_changes_nothing_the_second_time() {
    let world = World::systemd("twice");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);

    let run_twice = |step: Step| {
        go(&world, step, &runner, &probe);
        let (first, first_calls) = (world.snapshot(), runner.take_calls());
        let again = go(&world, step, &runner, &probe);
        assert_eq!(
            world.snapshot(),
            first,
            "{step} changed the record the second time: {again:?}"
        );
        assert_eq!(
            runner.take_calls(),
            first_calls,
            "{step} ran other commands the second time"
        );
        first_calls
    };
    run_twice(Step::Prepare);
    assert_eq!(run_twice(Step::Cutover), [START_A]);
    assert!(run_twice(Step::Verify).is_empty());
    assert!(run_twice(Step::Cleanup).is_empty());
    assert_eq!(world.snapshot().get("active"), Some(&Some("a".to_owned())));

    // An update from a to b, each step twice, too.
    run_twice(Step::Prepare);
    assert_eq!(run_twice(Step::Cutover), [STOP_A, START_B]);
    run_twice(Step::Verify);
    run_twice(Step::Cleanup);
    assert_eq!(world.snapshot().get("active"), Some(&Some("b".to_owned())));

    // Then a rollback and an uninstall, each twice.
    assert!(
        run_twice(Step::Rollback).is_empty(),
        "nothing is cut over: nothing to undo"
    );
    assert_eq!(run_twice(Step::Uninstall), [STOP_A, STOP_B]);
    assert_eq!(world.snapshot(), files(&[]));
}

/// Catches a rollback twice that restarts the old slot again or fails the second time, and one
/// with nothing recorded that is not a success.
#[test]
fn a_rollback_is_a_success_when_there_is_nothing_to_undo() {
    let world = World::systemd("nothing-to-undo");
    let runner = FakeRunner::default();
    let probe = FakeProbe::default();
    let then = failed(Phase::BeforeCommit, Class::Release);
    let after = Context {
        then: Some(then.clone()),
        ..ctx()
    };
    let want = Want {
        reversible: Some(true),
        ..Want::new(Outcome::Success)
    };
    let ran = world.step(Step::Rollback, &runner, &probe);
    assert_eq!(
        contract(Step::Rollback, &ran, &want, &after),
        Next::Report(then)
    );
    assert!(runner.take_calls().is_empty());

    // A prepared update that never cut over: rolling back forgets it and runs nothing.
    go(&world, Step::Prepare, &runner, &probe);
    let ran = world.step(Step::Rollback, &runner, &probe);
    contract(Step::Rollback, &ran, &want, &after);
    assert!(runner.take_calls().is_empty());
    assert_eq!(world.snapshot(), files(&[]));
}

/// Catches a rollback that trusts a recorded cutover with no next slot (a record cut off half
/// way), or that restarts the slot it has just stopped.
#[test]
fn a_rollback_over_an_odd_record_stops_only_what_it_can_name() {
    let world = World::systemd("odd-record");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    fs::write(world.record().join("cutover"), "").expect("a cutover with no next");
    go(&world, Step::Rollback, &runner, &probe);
    assert!(runner.take_calls().is_empty());
    assert_eq!(world.snapshot(), files(&[]));

    for (name, content) in [("active", "a"), ("next", "a"), ("cutover", "")] {
        fs::write(world.record().join(name), content).expect("the record");
    }
    go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(
        runner.take_calls(),
        [STOP_A],
        "the serving slot is the one stopped: not started again"
    );
    assert_eq!(
        world.snapshot(),
        files(&[("active", Some("a"))]),
        "the update is forgotten, not the serving slot"
    );
}

/// Catches a cutover that stops the slot it starts, when the record names the same slot twice.
#[test]
fn a_cutover_to_the_serving_slot_stops_nothing() {
    let world = World::systemd("same-slot");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    for (name, content) in [("active", "a"), ("next", "a"), ("prepared", "")] {
        fs::write(world.record().join(name), content).expect("the record");
    }
    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [START_A]);
}

/// Catches the launchd variant using systemctl, or a wrong label, or an argv that differs from
/// the one the design gives, in any step.
#[test]
fn launchd_slots_are_run_with_launchctl() {
    let (world, runner, probe) = installed("launchd", Units::Launchd);
    assert_eq!(world.snapshot().get("active"), Some(&Some("a".to_owned())));

    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [KILL_A, KICK_B]);
    probe.answer("/readyz", Ok((503, "not ready\n".to_owned())));
    let ran = world.step(Step::Verify, &runner, &probe);
    assert_eq!(ran.code, 1);
    go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(runner.take_calls(), [KILL_B, KICK_A]);
    go(&world, Step::Uninstall, &runner, &probe);
    assert_eq!(runner.take_calls(), [KILL_A, KILL_B]);
    assert_eq!(world.snapshot(), files(&[]));
}

/// Catches a launchd job with no process (launchctl answers non-zero to the kill) failing the
/// step, or being ignored silently, and a failed kickstart being ignored.
#[test]
fn a_launchd_kill_that_finds_no_process_is_logged_and_a_failed_kickstart_fails() {
    let (world, runner, probe) = installed("launchd-lenient", Units::Launchd);
    go(&world, Step::Prepare, &runner, &probe);
    runner.answer(KILL_A, Ok(113));
    let ran = go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [KILL_A, KICK_B]);
    assert!(
        ran.stderr.contains("exited with 113: ignored"),
        "the ignored answer is logged: {:?}",
        ran.stderr
    );

    runner.answer(KICK_B, Ok(3));
    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(Class::Release, None, "exited with 3");
    let next = contract(Step::Cutover, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));

    runner.answer(KILL_A, Err("no such file".to_owned()));
    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(Class::Environment, None, "cannot run");
    let next = contract(Step::Cutover, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Environment));
}

/// Catches a start that fails (non-zero, or not runnable) being taken for a success, a cutover
/// that does not record itself first (so that a rollback could not restart the stopped slot), and
/// a start that goes on after a stop that failed.
#[test]
fn a_failed_start_or_stop_fails_the_cutover_and_leaves_it_recorded() {
    let (world, runner, probe) = installed("cutover-fails", Units::Systemd);
    go(&world, Step::Prepare, &runner, &probe);

    runner.answer(STOP_A, Ok(1));
    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(
        Class::Release,
        None,
        "systemctl stop hello@a.service` exited with 1",
    );
    let next = contract(Step::Cutover, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Release));
    assert_eq!(
        runner.take_calls(),
        [STOP_A],
        "no start after a stop that failed"
    );
    assert!(
        world.snapshot().contains_key("cutover"),
        "a rollback must know it may have to undo"
    );

    runner.answer(STOP_A, Ok(0));
    runner.answer(START_B, Ok(1));
    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(
        Class::Release,
        None,
        "systemctl start hello@b.service` exited with 1",
    );
    contract(Step::Cutover, &ran, &want, &ctx());

    runner.answer(START_B, Err("Permission denied".to_owned()));
    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = failure(Class::Environment, None, "Permission denied");
    let next = contract(Step::Cutover, &ran, &want, &ctx());
    assert_eq!(next, rollback_then(Phase::BeforeCommit, Class::Environment));
    runner.take_calls();

    // The rollback that follows undoes it: the started slot stops, the old one starts.
    runner.answer(START_B, Ok(0));
    go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(runner.take_calls(), [STOP_B, START_A]);
}

/// Catches a rollback that fails (the old slot does not start, or the new one does not stop)
/// being reported as a success, or forgetting the update, which would make a second rollback
/// do nothing.
#[test]
fn a_rollback_that_cannot_stop_or_start_fails_and_keeps_the_record() {
    let (world, runner, probe) = installed("rollback-fails", Units::Systemd);
    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    runner.take_calls();

    runner.answer(STOP_B, Ok(1));
    let ran = world.step(Step::Rollback, &runner, &probe);
    let want = failure(Class::Release, None, "exited with 1");
    // The updater cannot undo what it cannot stop: nothing repairs it, a halt.
    let next = contract(Step::Rollback, &ran, &want, &ctx());
    assert_eq!(next, Next::Report(Verdict::Halt));
    assert_eq!(runner.take_calls(), [STOP_B]);
    assert!(
        world.snapshot().contains_key("cutover"),
        "the record still says what to undo"
    );

    runner.answer(STOP_B, Ok(0));
    runner.answer(START_A, Ok(1));
    let ran = world.step(Step::Rollback, &runner, &probe);
    contract(Step::Rollback, &ran, &want, &ctx());
    assert_eq!(runner.take_calls(), [STOP_B, START_A]);
    assert!(world.snapshot().contains_key("next"));

    runner.answer(START_A, Ok(0));
    go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(world.snapshot().get("active"), Some(&Some("a".to_owned())));
    assert!(!world.snapshot().contains_key("next"));
}

/// Catches an uninstall that leaves a slot running, a marker, or any key of the record behind,
/// and one that fails without telling the updater whose fault it is.
#[test]
fn an_uninstall_stops_both_slots_and_removes_the_record() {
    let (world, runner, probe) = installed("uninstall", Units::Systemd);
    let ran = world.step(Step::Uninstall, &runner, &probe);
    let next = contract(Step::Uninstall, &ran, &Want::new(Outcome::Success), &ctx());
    assert_eq!(
        next,
        Next::Report(Verdict::Succeeded {
            promotion_pending: false
        })
    );
    assert_eq!(runner.take_calls(), [STOP_A, STOP_B]);
    assert_eq!(world.snapshot(), files(&[]));

    // A stop that fails fails the uninstall, before the record is touched.
    let (world, runner, probe) = installed("uninstall-fails", Units::Systemd);
    runner.answer(STOP_B, Ok(1));
    let ran = world.step(Step::Uninstall, &runner, &probe);
    let want = failure(Class::Release, None, "exited with 1");
    let next = contract(Step::Uninstall, &ran, &want, &ctx());
    assert_eq!(
        next,
        Next::Report(failed(Phase::BeforeCommit, Class::Release))
    );
    assert!(world.snapshot().contains_key("active"));
}

/// Catches an uninstall that removes a directory of markers somebody put files in, or hides the
/// failure to remove it.
#[test]
fn an_uninstall_that_cannot_remove_the_markers_directory_fails() {
    let (world, runner, probe) = installed("uninstall-stray", Units::Systemd);
    fs::write(world.record().join("slots").join("stray"), "x").expect("a stray file");
    let ran = world.step(Step::Uninstall, &runner, &probe);
    let want = failure(Class::Environment, None, "cannot remove");
    let next = contract(Step::Uninstall, &ran, &want, &ctx());
    assert_eq!(
        next,
        Next::Report(failed(Phase::BeforeCommit, Class::Environment))
    );
}

/// Catches the detail of a failure not being logged on standard error, where an operator reads
/// it, or being printed after the report line.
#[test]
fn the_detail_is_logged_on_standard_error_and_the_line_is_last() {
    let world = World::systemd("logs");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let ran = world.step(Step::Cutover, &runner, &probe);
    assert_eq!(ran.stderr, "hello update-hook: prepare has not run\n");
    assert_eq!(
        ran.stdout,
        "{\"class\":\"release\",\"detail\":\"prepare has not run\"}\n"
    );
    let ran = go(&world, Step::Rollback, &runner, &probe);
    assert_eq!(ran.stderr, "", "a report with no detail logs nothing");
}

/// Catches a record that is edited by hand or cut off (a key that holds no slot, a prepare with
/// no next slot) being trusted, and a key the hook cannot read, write or remove being ignored.
#[test]
fn a_record_the_hook_cannot_read_or_write_is_an_environment_failure() {
    let world = World::systemd("odd-keys");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let record = world.record();
    let environment = |step: Step, detail: &str| {
        let ran = world.step(step, &runner, &probe);
        let want = failure(Class::Environment, None, detail);
        contract(step, &ran, &want, &ctx());
    };

    fs::write(record.join("active"), "c\n").expect("active");
    environment(Step::Prepare, "holds \"c\", not a slot");
    fs::remove_file(record.join("active")).expect("remove");

    fs::write(record.join("prepared"), "").expect("prepared");
    environment(Step::Cutover, "prepared but no next slot");

    // A key that is a directory cannot be read, replaced or removed as a file.
    fs::create_dir(record.join("active")).expect("a directory");
    environment(Step::Prepare, "cannot read");
    fs::remove_dir(record.join("active")).expect("remove");

    fs::create_dir_all(record.join("next")).expect("a directory");
    fs::write(record.join("next").join("held"), "").expect("a file in it");
    environment(Step::Prepare, "cannot write");
    fs::remove_dir_all(record.join("next")).expect("remove");

    fs::create_dir(record.join("verified")).expect("a directory");
    environment(Step::Prepare, "cannot remove");
    fs::remove_dir(record.join("verified")).expect("remove");

    // A marker the hook cannot create: `slots` is a file.
    fs::write(record.join("next"), "a").expect("next");
    fs::write(record.join("slots"), "").expect("a file where the markers go");
    environment(Step::Cutover, "cannot create");
    assert!(
        runner.take_calls().is_empty(),
        "no unit is started without its marker"
    );
}
