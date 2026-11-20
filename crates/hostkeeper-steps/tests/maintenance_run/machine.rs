//! The rows of the step table (section 5.2) and of the table of how a host's turn ends (section
//! 5.7), each as the transition function answers it, and every event refused where it does not
//! fit.

use hostkeeper_steps::maintenance::{
    Action, Ending, Event, Kind, Launch, Outcome, RunStep, TransitionError, next_action, transition,
};

use crate::support::{
    apply, boot, defer, drain, fail, ok_until, on, record, resume, run_step, service,
};

// Row 1, stage.

/// Catches a turn that drains before it staged, or that does not go on to the first service's
/// drain once the stage succeeded.
#[test]
fn a_new_turn_stages_first_and_a_staged_turn_drains_the_first_service() {
    let rec = record(&[Kind::Linux], &["js-agent", "tierstore"]);
    assert_eq!(next_action(&rec), Action::Launch(Launch::Stage));
    let (launched, after, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(launched, Launch::Stage);
    assert_eq!(after.step, RunStep::Drain);
    assert!(after.drained.is_empty());
    assert_eq!(next, drain("js-agent"));
}

/// Catches a failed stage that halts the turn or drains a service: the host never left
/// service, so the turn ends deferred with the stage's reason and nothing to put back.
#[test]
fn a_failed_stage_defers_the_turn_and_drains_and_resumes_nothing() {
    let rec = record(&[Kind::Linux], &["js-agent"]);
    let (_, after, next) = run_step(&rec, fail("exit 1: no space for the download"));
    let reason = "stage: exit 1: no space for the download".to_owned();
    assert_eq!(next, Action::End(Ending::Deferred(reason.clone())));
    assert_eq!(after.deferred, Some(reason));
    assert_eq!(after.halted, None);
    assert!(after.drained.is_empty());
    assert_eq!(after.step, RunStep::Done);
}

/// Catches a stage that defers (exit 75) read as a halt.
#[test]
fn a_stage_that_defers_defers_the_turn() {
    let rec = record(&[Kind::Linux], &["js-agent"]);
    let (_, _, next) = run_step(&rec, defer("exit 75"));
    assert_eq!(
        next,
        Action::End(Ending::Deferred("stage: exit 75".to_owned()))
    );
}

// Row 2, drain.

/// Catches the drains run out of the group's package order, a service drained twice, or the
/// apply started before the last service drained.
#[test]
fn each_service_is_drained_once_in_order_and_then_the_first_kind_applies() {
    let rec = record(&[Kind::Linux], &["js-agent", "tierstore"]);
    let rec = ok_until(rec, &drain("js-agent"));
    let (launched, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(launched, Launch::Drain(service("js-agent")));
    assert_eq!(next, drain("tierstore"));
    let (launched, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(launched, Launch::Drain(service("tierstore")));
    assert_eq!(rec.drained, [service("js-agent"), service("tierstore")]);
    assert_eq!(next, apply(Kind::Linux));
}

/// Catches a host with no service to drain held at the drain step.
#[test]
fn a_host_with_no_service_to_drain_goes_from_stage_to_apply() {
    let rec = record(&[Kind::Reboot], &[]);
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, apply(Kind::Reboot));
    assert_eq!(rec.step, RunStep::Apply);
}

/// Catches a drain counted drained only when it ends, so that one cut short (a crash, a
/// deadline) is never resumed; and a drain launched again after a crash counted twice.
#[test]
fn a_service_counts_drained_from_the_launch_of_its_drain_and_once() {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &drain("js-agent"));
    let (started, action) = on(&rec, Event::Started);
    assert_eq!(action, drain("js-agent"));
    assert_eq!(started.drained, [service("js-agent")]);
    assert!(started.running);
    // The daemon died with the drain running: the record says to launch it again.
    assert_eq!(next_action(&started), drain("js-agent"));
    let (again, action) = on(&started, Event::Started);
    assert_eq!(action, drain("js-agent"));
    assert_eq!(again, started);
}

/// Catches a failed drain that resumes the services drained so far, or runs any later step:
/// the host stays cordoned until an operator acts.
#[test]
fn a_failed_drain_halts_and_resumes_nothing() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &drain("tierstore"),
    );
    let (_, after, next) = run_step(&rec, fail("exit 1"));
    let reason = "drain tierstore: exit 1".to_owned();
    assert_eq!(next, Action::End(Ending::Halted(reason.clone())));
    assert_eq!(after.halted, Some(reason));
    assert_eq!(after.step, RunStep::Drain);
    assert_eq!(after.drained, [service("js-agent"), service("tierstore")]);
}

/// Catches a drain whose deadline passed read as a halt, a deferral that leaves a drained
/// service out of service, the service whose drain deferred left out of the put back, or the
/// put back run in the drain's order.
#[test]
fn a_drain_past_its_deadline_resumes_every_drained_service_in_reverse_and_defers() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &drain("tierstore"),
    );
    let (_, rec, next) = run_step(&rec, defer("the drain deadline passed"));
    let reason = "drain tierstore: the drain deadline passed".to_owned();
    assert_eq!(rec.deferred, Some(reason.clone()));
    assert_eq!(rec.step, RunStep::PutBack);
    assert_eq!(next, resume("tierstore"));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, resume("js-agent"));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, Action::End(Ending::Deferred(reason)));
    assert!(rec.drained.is_empty());
    assert!(rec.kinds.iter().all(|kind| !kind.applied));
}

// Row 3, apply.

/// Catches the kinds applied in the order the order named them rather than macOS first, a kind
/// applied twice, or the check started before every kind is applied.
#[test]
fn each_kind_applies_once_macos_first_and_then_the_check() {
    let rec = record(&[Kind::Xcode, Kind::Macos], &[]);
    let kinds: Vec<Kind> = rec.kinds.iter().map(|progress| progress.kind).collect();
    assert_eq!(kinds, [Kind::Macos, Kind::Xcode]);
    let rec = ok_until(rec, &apply(Kind::Macos));
    let (launched, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(launched, Launch::Apply(Kind::Macos));
    assert_eq!(next, apply(Kind::Xcode));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, Action::Launch(Launch::Check));
    assert!(rec.kinds.iter().all(|kind| kind.applied));
}

/// Catches a failed apply that rolls back, resumes or goes on to the next kind.
#[test]
fn a_failed_apply_halts_and_resumes_nothing() {
    let rec = ok_until(
        record(&[Kind::Linux, Kind::Reboot], &["js-agent"]),
        &apply(Kind::Linux),
    );
    let (_, after, next) = run_step(&rec, fail("exit 100: dpkg_error"));
    let reason = "apply linux: exit 100: dpkg_error".to_owned();
    assert_eq!(next, Action::End(Ending::Halted(reason)));
    assert_eq!(after.drained, [service("js-agent")]);
    assert!(after.kinds.iter().all(|kind| !kind.applied));
}

/// Catches an apply that exits 75 read as a deferral: only the stage and a drain's deadline
/// defer; from the drain on, every failure halts.
#[test]
fn an_apply_that_defers_halts() {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &apply(Kind::Linux));
    let (_, after, next) = run_step(&rec, defer("exit 75"));
    assert_eq!(
        next,
        Action::End(Ending::Halted("apply linux: exit 75".to_owned()))
    );
    assert_eq!(after.deferred, None);
}

/// Catches a reboot whose host is still on the boot it had before treated as done, and a host
/// back on a new boot not going on with the next kind (or not recording the kind applied).
#[test]
fn a_reboot_record_waits_on_the_same_boot_and_goes_on_with_the_next_kind_on_a_new_one() {
    let rec = ok_until(
        record(&[Kind::Linux, Kind::Reboot], &[]),
        &apply(Kind::Linux),
    );
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, action) = on(&started, Event::RebootUnderWay(boot(1)));
    assert_eq!(action, Action::Wait(Kind::Linux));
    assert_eq!(rebooting.kinds[0].reboot_under_way, Some(boot(1)));
    assert_eq!(next_action(&rebooting), Action::Wait(Kind::Linux));

    let (same, action) = on(&rebooting, Event::BootIdRead(boot(1)));
    assert_eq!(action, Action::Wait(Kind::Linux));
    assert_eq!(same, rebooting);

    let (back, action) = on(&rebooting, Event::BootIdRead(boot(2)));
    assert_eq!(action, apply(Kind::Reboot));
    assert!(back.kinds[0].applied);
    assert_eq!(back.kinds[0].reboot_under_way, None);
    assert!(!back.running);
}

/// Catches the last kind's reboot not going on to the check once the host is back.
#[test]
fn the_last_kind_back_from_its_reboot_goes_on_to_the_check() {
    let rec = ok_until(record(&[Kind::Reboot], &["js-agent"]), &apply(Kind::Reboot));
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(7)));
    let (_, action) = on(&rebooting, Event::BootIdRead(boot(8)));
    assert_eq!(action, Action::Launch(Launch::Check));
}

/// Catches a `cutover` that ends well after it recorded its reboot read as the kind applied
/// before the host rebooted.
#[test]
fn a_cutover_that_ends_after_recording_its_reboot_still_waits_for_the_new_boot() {
    let rec = ok_until(record(&[Kind::Linux], &[]), &apply(Kind::Linux));
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    let (ended, action) = on(&rebooting, Event::Ended(Outcome::Ok));
    assert_eq!(action, Action::Wait(Kind::Linux));
    assert!(!ended.kinds[0].applied);
    assert!(!ended.running);
    let (_, action) = on(&ended, Event::BootIdRead(boot(2)));
    assert_eq!(action, Action::Launch(Launch::Check));
}

/// Catches a `cutover` that fails after it recorded its reboot still waited on.
#[test]
fn a_cutover_that_fails_after_recording_its_reboot_halts() {
    let rec = ok_until(record(&[Kind::Linux], &[]), &apply(Kind::Linux));
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    let (_, action) = on(&rebooting, Event::Ended(fail("exit 1: reboot refused")));
    assert_eq!(
        action,
        Action::End(Ending::Halted(
            "apply linux: exit 1: reboot refused".to_owned()
        ))
    );
}

/// Catches a kind's new version read during its reboot not applying it, and a version not yet
/// reached applying it.
#[test]
fn a_version_read_applies_the_kind_waited_for_only_once_reached() {
    let rec = ok_until(
        record(&[Kind::Macos, Kind::Xcode], &[]),
        &apply(Kind::Macos),
    );
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    let (not_yet, action) = on(&rebooting, Event::VersionRead { reached: false });
    assert_eq!(action, Action::Wait(Kind::Macos));
    assert_eq!(not_yet, rebooting);
    let (reached, action) = on(&rebooting, Event::VersionRead { reached: true });
    assert_eq!(action, apply(Kind::Xcode));
    assert!(reached.kinds[0].applied);
}

// Row 4, check.

/// Catches a passed check that does not put the services back, or puts them back in the drain's
/// order.
#[test]
fn a_passed_check_resumes_the_drained_services_in_reverse() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &Action::Launch(Launch::Check),
    );
    let (launched, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(launched, Launch::Check);
    assert_eq!(rec.step, RunStep::PutBack);
    assert_eq!(next, resume("tierstore"));
}

/// Catches a failed check that puts the host back in service.
#[test]
fn a_failed_check_halts_and_resumes_nothing() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent"]),
        &Action::Launch(Launch::Check),
    );
    let (_, after, next) = run_step(&rec, fail("exit 1: kernel 6.8.0-50 is not running"));
    assert_eq!(
        next,
        Action::End(Ending::Halted(
            "check: exit 1: kernel 6.8.0-50 is not running".to_owned()
        ))
    );
    assert_eq!(after.drained, [service("js-agent")]);
}

// Row 5, put back.

/// Catches a failed resume that goes on to the next service or ends the turn done.
#[test]
fn a_failed_resume_halts_with_the_services_not_yet_resumed_still_drained() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &resume("tierstore"),
    );
    let (_, rec, _) = run_step(&rec, Outcome::Ok);
    let (_, after, next) = run_step(&rec, fail("exit 1"));
    assert_eq!(
        next,
        Action::End(Ending::Halted("resume js-agent: exit 1".to_owned()))
    );
    assert_eq!(after.drained, [service("js-agent")]);
}

/// Catches a failed resume of a deferral's put back ending the turn deferred: the host is not
/// back in service, so the turn halts.
#[test]
fn a_failed_resume_of_a_deferral_halts() {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &drain("js-agent"));
    let (_, rec, _) = run_step(&rec, defer("the drain deadline passed"));
    let (_, after, next) = run_step(&rec, fail("exit 1"));
    assert_eq!(
        next,
        Action::End(Ending::Halted("resume js-agent: exit 1".to_owned()))
    );
    assert_eq!(
        after.deferred,
        Some("drain js-agent: the drain deadline passed".to_owned())
    );
}

// Row 6, done; and section 5.7.

/// Catches a turn whose every step ran not ending done, or ending with a service still drained
/// (cordoned) or a kind not applied.
#[test]
fn ends_done_after_every_step_with_nothing_drained_and_every_kind_applied() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &Action::End(Ending::Done),
    );
    assert_eq!(rec.step, RunStep::Done);
    assert!(rec.drained.is_empty());
    assert!(rec.kinds.iter().all(|kind| kind.applied));
    assert_eq!((rec.deferred, rec.halted), (None, None));
}

/// Catches a deferred turn (section 5.7, deferred) that leaves a service drained or reads as
/// done.
#[test]
fn ends_deferred_with_nothing_drained() {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &drain("js-agent"));
    let (_, rec, _) = run_step(&rec, defer("the drain deadline passed"));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(
        next,
        Action::End(Ending::Deferred(
            "drain js-agent: the drain deadline passed".to_owned()
        ))
    );
    assert_eq!(next_action(&rec), next);
    assert!(rec.drained.is_empty());
}

/// Catches a halted turn (section 5.7, halted) that yields a `resume`, or any launch, by any
/// road: after the halt, every event that would launch a step is refused and the record stands
/// halted with the drained services kept.
#[test]
fn ends_halted_and_a_halt_never_yields_a_resume() {
    let halts = [
        (drain("tierstore"), fail("exit 1")),
        (drain("js-agent"), fail("exit 1")),
        (apply(Kind::Linux), fail("exit 1")),
        (apply(Kind::Linux), defer("exit 75")),
        (Action::Launch(Launch::Check), fail("exit 1")),
        (resume("tierstore"), fail("exit 1")),
    ];
    for (at, outcome) in halts {
        let rec = ok_until(record(&[Kind::Linux], &["js-agent", "tierstore"]), &at);
        let (_, halted, next) = run_step(&rec, outcome);
        assert!(matches!(next, Action::End(Ending::Halted(_))), "{at:?}");
        assert_eq!(next_action(&halted), next, "{at:?}");
        assert!(!halted.drained.is_empty(), "{at:?}");
        for (event, name) in [
            (Event::Started, "started"),
            (Event::Ended(Outcome::Ok), "ended"),
            (Event::BootIdRead(boot(9)), "boot id read"),
            (Event::VersionRead { reached: true }, "version read"),
        ] {
            assert_eq!(
                transition(&halted, event),
                Err(TransitionError {
                    event: name,
                    at: "halted".to_owned(),
                }),
                "{at:?}"
            );
        }
    }
}

/// Catches a host silent in its reboot (section 5.7, not back) read as anything but still
/// rebooting, at every start on the old boot, and its return not resuming the turn.
#[test]
fn a_host_not_back_keeps_waiting_and_its_return_resumes_the_turn() {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &apply(Kind::Linux));
    let (started, _) = on(&rec, Event::Started);
    let (mut waiting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    for _ in 0..3 {
        let (same, action) = on(&waiting, Event::BootIdRead(boot(1)));
        assert_eq!(action, Action::Wait(Kind::Linux));
        waiting = same;
    }
    assert_eq!(waiting.drained, [service("js-agent")]);
    let (_, action) = on(&waiting, Event::BootIdRead(boot(2)));
    assert_eq!(action, Action::Launch(Launch::Check));
}

// Events that do not fit.

/// Catches an event taken where it does not fit, changing the record: each is refused, naming
/// the event and the action the record stands at.
#[test]
fn an_event_that_does_not_fit_is_refused_naming_it_and_where_the_turn_stands() {
    let new = record(&[Kind::Linux], &["js-agent"]);
    let draining = ok_until(new.clone(), &drain("js-agent"));
    let applying = ok_until(new.clone(), &apply(Kind::Linux));
    let (applying_started, _) = on(&applying, Event::Started);
    let (rebooting, _) = on(&applying_started, Event::RebootUnderWay(boot(1)));
    let (rebooting_ended, _) = on(&rebooting, Event::Ended(Outcome::Ok));
    let checking = ok_until(new.clone(), &Action::Launch(Launch::Check));
    let done = ok_until(new.clone(), &Action::End(Ending::Done));
    let (_, deferred, _) = run_step(&new, fail("exit 1"));
    let refused = [
        (&new, Event::Ended(Outcome::Ok), "ended", "stage"),
        (
            &draining,
            Event::RebootUnderWay(boot(1)),
            "reboot under way",
            "drain js-agent",
        ),
        (
            &applying,
            Event::RebootUnderWay(boot(1)),
            "reboot under way",
            "apply linux",
        ),
        (
            &applying,
            Event::BootIdRead(boot(1)),
            "boot id read",
            "apply linux",
        ),
        (
            &checking,
            Event::VersionRead { reached: true },
            "version read",
            "check",
        ),
        (
            &rebooting,
            Event::Started,
            "started",
            "wait for the reboot of linux",
        ),
        (
            &rebooting_ended,
            Event::Ended(Outcome::Ok),
            "ended",
            "wait for the reboot of linux",
        ),
        (&done, Event::Started, "started", "done"),
        (&deferred, Event::Started, "started", "deferred"),
    ];
    for (rec, event, name, at) in refused {
        let error = transition(rec, event).expect_err(name);
        assert_eq!(
            error.to_string(),
            format!("the event {name} does not fit a maintenance turn at {at}")
        );
    }
}
