//! An operator's Retry and Put back of a halted turn (docs/design/maintenance-updates.md, section
//! 4.2), each as the transition function answers it: Retry launches the step that failed again
//! under the new try and the turn goes on; Put back resumes the drained services in reverse and
//! ends the turn with the halt kept as its note; neither fits a turn that is not halted.

use hostkeeper_steps::maintenance::{
    Action, Ending, Event, Kind, Launch, MaintenanceRecord, Outcome, RunStep, TransitionError,
    next_action, transition,
};

use crate::support::{
    apply, boot, defer, drain, fail, ok_until, on, record, resume, run_step, service,
};

const CHECK: Action = Action::Launch(Launch::Check);

/// A turn of `linux` over `js-agent` and `tierstore` whose step at `at` ended with `outcome`,
/// which halts it.
fn halted_at(at: &Action, outcome: Outcome) -> MaintenanceRecord {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent", "tierstore"]), at);
    let (_, halted, next) = run_step(&rec, outcome);
    assert!(matches!(next, Action::End(Ending::Halted(_))), "{at:?}");
    halted
}

fn retried(order_try: u64) -> Event {
    Event::Retried {
        order_try,
        boot: Some(boot(9)),
    }
}

/// A turn of `linux` whose `cutover` asked for its reboot in the boot `boot(1)` and then failed,
/// which halts it with the reboot under way.
fn halted_with_reboot_asked() -> MaintenanceRecord {
    let rec = ok_until(record(&[Kind::Linux], &["js-agent"]), &apply(Kind::Linux));
    let (started, _) = on(&rec, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    let (halted, next) = on(&rebooting, Event::Ended(fail("exit 1")));
    assert_eq!(
        next,
        Action::End(Ending::Halted("apply linux: exit 1".to_owned()))
    );
    assert_eq!(halted.kinds[0].reboot_under_way, Some(boot(1)));
    halted
}

// Retry.

/// Catches a Retry that launches anything but the step that failed (the next service's drain,
/// the next kind, the put back), keeps the old try, or leaves the halt: from each step that can
/// halt, the retried record carries the new try and no halt, keeps the drained services, and
/// stands at that same step.
#[test]
fn a_retry_launches_the_step_that_failed_again_under_the_new_try() {
    let halts = [
        drain("js-agent"),
        drain("tierstore"),
        apply(Kind::Linux),
        CHECK,
        resume("tierstore"),
        resume("js-agent"),
    ];
    for at in halts {
        let halted = halted_at(&at, fail("exit 1"));
        let (rec, next) = on(&halted, retried(2));
        assert_eq!(next, at, "{at:?}");
        assert_eq!(next_action(&rec), at, "{at:?}");
        assert_eq!(rec.halted, None, "{at:?}");
        assert_eq!(rec.order_try, 2, "{at:?}");
        assert_eq!(rec.drained, halted.drained, "{at:?}");
        assert_eq!(rec.step, halted.step, "{at:?}");
        assert_eq!(rec.put_back, None, "{at:?}");
    }
}

/// Catches a retried drain that counts its service drained twice, or drains the next service in
/// its place: `tierstore`'s drain failed after it counted `tierstore` drained, so the retry
/// launches `tierstore`'s drain again with the list unchanged, and once it succeeds the apply
/// comes next.
#[test]
fn a_retried_drain_drains_the_same_service_once_and_goes_on_to_the_apply() {
    let halted = halted_at(&drain("tierstore"), fail("exit 1"));
    assert_eq!(halted.drained, [service("js-agent"), service("tierstore")]);
    let (rec, _) = on(&halted, retried(2));
    let (started, action) = on(&rec, Event::Started);
    assert_eq!(action, drain("tierstore"));
    assert_eq!(started.drained, [service("js-agent"), service("tierstore")]);
    let (after, next) = on(&started, Event::Ended(Outcome::Ok));
    assert_eq!(next, apply(Kind::Linux));
    assert_eq!(after.drained, [service("js-agent"), service("tierstore")]);
}

/// Catches a retry that re-runs the steps before the one that failed, or stops after it: a failed
/// apply is retried, and the turn goes on from the apply through the check and both resumes, in
/// reverse, to done, with the kind applied and no note.
#[test]
fn a_retried_apply_goes_on_to_the_check_and_the_put_back_and_ends_done() {
    let halted = halted_at(&apply(Kind::Linux), fail("exit 1"));
    let (mut rec, mut next) = on(&halted, retried(2));
    let mut launched = Vec::new();
    while let Action::Launch(_) = next {
        launched.push(next.clone());
        let (_, after, then) = run_step(&rec, Outcome::Ok);
        (rec, next) = (after, then);
    }
    assert_eq!(
        launched,
        [
            apply(Kind::Linux),
            CHECK,
            resume("tierstore"),
            resume("js-agent")
        ]
    );
    assert_eq!(next, Action::End(Ending::Done));
    assert_eq!(rec.step, RunStep::Done);
    assert!(rec.kinds[0].applied);
    assert!(rec.drained.is_empty());
    assert_eq!(rec.order_try, 2);
    assert_eq!(rec.put_back, None);
}

/// Catches a retry of a run of two kinds that applies the first kind again: `linux` was applied
/// and the `reboot` kind's cutover failed, so the retry launches the `reboot` cutover alone.
#[test]
fn a_retry_applies_only_the_kinds_not_yet_applied() {
    let rec = ok_until(
        record(&[Kind::Linux, Kind::Reboot], &["js-agent"]),
        &apply(Kind::Reboot),
    );
    assert!(rec.kinds[0].applied);
    let (_, halted, _) = run_step(&rec, fail("exit 1"));
    let (retried_rec, next) = on(&halted, retried(2));
    assert_eq!(next, apply(Kind::Reboot));
    assert!(retried_rec.kinds[0].applied);
    assert!(!retried_rec.kinds[1].applied);
}

/// Catches a retry that fails again carrying on, or keeping the first reason: the retried check
/// fails with another exit, so the turn halts again with the new reason, the new try kept.
#[test]
fn a_retry_that_fails_again_halts_again_with_the_new_reason() {
    let halted = halted_at(&CHECK, fail("exit 1"));
    let (rec, _) = on(&halted, retried(2));
    let (_, again, next) = run_step(&rec, fail("exit 2"));
    let reason = "check: exit 2".to_owned();
    assert_eq!(next, Action::End(Ending::Halted(reason.clone())));
    assert_eq!(again.halted, Some(reason));
    assert_eq!(again.order_try, 2);
    assert_eq!(again.step, RunStep::Check);
}

/// Catches a retry of a `cutover` that failed after asking for its reboot that waits for a reboot
/// that never came: on the boot the reboot was asked in, the reboot is forgotten and the
/// `cutover` launched again.
#[test]
fn a_retry_on_the_boot_the_reboot_was_asked_in_launches_the_cutover_again() {
    let halted = halted_with_reboot_asked();
    let (rec, next) = on(
        &halted,
        Event::Retried {
            order_try: 2,
            boot: Some(boot(1)),
        },
    );
    assert_eq!(next, apply(Kind::Linux));
    assert_eq!(rec.kinds[0].reboot_under_way, None);
    assert!(!rec.kinds[0].applied);
}

/// Catches a retry after the host did reboot that installs the kind a second time, and one whose
/// boot id cannot be read that guesses: on another boot, and with no boot id, the retry keeps
/// the reboot under way and waits, and a new boot id then applies the kind and the check comes
/// next.
#[test]
fn a_retry_on_another_boot_or_none_waits_and_a_new_boot_applies_the_kind() {
    let halted = halted_with_reboot_asked();
    for boot_now in [Some(boot(2)), None] {
        let (rec, next) = on(
            &halted,
            Event::Retried {
                order_try: 2,
                boot: boot_now.clone(),
            },
        );
        assert_eq!(next, Action::Wait(Kind::Linux), "{boot_now:?}");
        assert_eq!(rec.kinds[0].reboot_under_way, Some(boot(1)), "{boot_now:?}");
        let (applied, then) = on(&rec, Event::BootIdRead(boot(2)));
        assert_eq!(then, CHECK, "{boot_now:?}");
        assert!(applied.kinds[0].applied, "{boot_now:?}");
    }
}

// Put back.

/// Catches a Put back that resumes in the drain's order, misses a drained service, runs another
/// step, or forgets the halt: from each step that can halt, the record stands at the last drained
/// service's `resume` with the new try, no halt and the halt's reason as its note; the `resume`s
/// then run in reverse to done, and each kind stays applied or not as it was.
#[test]
fn a_put_back_resumes_every_drained_service_in_reverse_and_ends_done() {
    let halts = [
        (drain("js-agent"), vec![resume("js-agent")]),
        (
            drain("tierstore"),
            vec![resume("tierstore"), resume("js-agent")],
        ),
        (
            apply(Kind::Linux),
            vec![resume("tierstore"), resume("js-agent")],
        ),
        (CHECK, vec![resume("tierstore"), resume("js-agent")]),
        (
            resume("tierstore"),
            vec![resume("tierstore"), resume("js-agent")],
        ),
        (resume("js-agent"), vec![resume("js-agent")]),
    ];
    for (at, resumes) in halts {
        let halted = halted_at(&at, fail("exit 1"));
        let reason = halted.halted.clone();
        let (mut rec, mut next) = on(&halted, Event::PutBack { order_try: 3 });
        assert_eq!(rec.halted, None, "{at:?}");
        assert_eq!(rec.put_back, reason, "{at:?}");
        assert_eq!(rec.order_try, 3, "{at:?}");
        assert_eq!(rec.step, RunStep::PutBack, "{at:?}");
        let mut launched = Vec::new();
        while let Action::Launch(_) = next {
            launched.push(next.clone());
            let (_, after, then) = run_step(&rec, Outcome::Ok);
            (rec, next) = (after, then);
        }
        assert_eq!(launched, resumes, "{at:?}");
        assert_eq!(next, Action::End(Ending::Done), "{at:?}");
        assert!(rec.drained.is_empty(), "{at:?}");
        assert_eq!(rec.kinds, halted.kinds, "{at:?}");
        assert_eq!(rec.put_back, reason, "{at:?}");
    }
}

/// Catches a Put back of a host that was never drained that launches anything: the turn has no
/// service to drain and its check failed, so the Put back ends the turn done at once, with the
/// kind applied and the note kept.
#[test]
fn a_put_back_of_a_host_never_drained_ends_done_at_once() {
    let rec = ok_until(record(&[Kind::Linux], &[]), &CHECK);
    let (_, halted, _) = run_step(&rec, fail("exit 1"));
    assert!(halted.drained.is_empty());
    let (after, next) = on(&halted, Event::PutBack { order_try: 2 });
    assert_eq!(next, Action::End(Ending::Done));
    assert_eq!(after.step, RunStep::Done);
    assert!(after.kinds[0].applied);
    assert_eq!(after.put_back, Some("check: exit 1".to_owned()));
}

/// Catches a Put back that leaves a reboot under way to be waited for, or counts its kind
/// applied: the `cutover` asked for its reboot and failed, so the Put back forgets the reboot,
/// leaves the kind not applied and resumes the drained service.
#[test]
fn a_put_back_forgets_a_reboot_under_way_and_leaves_its_kind_not_applied() {
    let halted = halted_with_reboot_asked();
    let (rec, next) = on(&halted, Event::PutBack { order_try: 2 });
    assert_eq!(next, resume("js-agent"));
    assert_eq!(rec.kinds[0].reboot_under_way, None);
    assert!(!rec.kinds[0].applied);
    let (_, done, end) = run_step(&rec, Outcome::Ok);
    assert_eq!(end, Action::End(Ending::Done));
    assert!(!done.kinds[0].applied);
}

/// Catches a Put back of a turn that halted while a deferral put its services back ending as done:
/// the drain's deadline passed, `js-agent`'s resume failed, and the Put back resumes `js-agent`
/// and ends the turn deferred with the deferral's reason, nothing applied.
#[test]
fn a_put_back_of_a_halt_in_a_deferrals_put_back_ends_deferred() {
    let rec = ok_until(
        record(&[Kind::Linux], &["js-agent", "tierstore"]),
        &drain("tierstore"),
    );
    let (_, deferring, next) = run_step(&rec, defer("exit 75"));
    assert_eq!(next, resume("tierstore"));
    let (_, resumed, next) = run_step(&deferring, Outcome::Ok);
    assert_eq!(next, resume("js-agent"));
    let (_, halted, _) = run_step(&resumed, fail("exit 1"));
    let (after, next) = on(&halted, Event::PutBack { order_try: 2 });
    assert_eq!(next, resume("js-agent"));
    let (_, ended, end) = run_step(&after, Outcome::Ok);
    let deferral = "drain tierstore: exit 75".to_owned();
    assert_eq!(end, Action::End(Ending::Deferred(deferral.clone())));
    assert_eq!(ended.deferred, Some(deferral));
    assert_eq!(ended.put_back, Some("resume js-agent: exit 1".to_owned()));
    assert!(!ended.kinds[0].applied);
}

/// Catches a Put back whose own `resume` fails being carried past, and a Retry of it that skips
/// that `resume`: the failed `resume` halts the turn again with its reason, and a Retry launches
/// that same `resume` and then the next, to done with the first note kept.
#[test]
fn a_put_back_whose_resume_fails_halts_and_a_retry_resumes_from_there() {
    let halted = halted_at(&CHECK, fail("exit 1"));
    let (rec, _) = on(&halted, Event::PutBack { order_try: 2 });
    let (_, again, next) = run_step(&rec, fail("exit 1"));
    let reason = "resume tierstore: exit 1".to_owned();
    assert_eq!(next, Action::End(Ending::Halted(reason)));
    let (rec, next) = on(&again, retried(3));
    assert_eq!(next, resume("tierstore"));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, resume("js-agent"));
    let (_, rec, next) = run_step(&rec, Outcome::Ok);
    assert_eq!(next, Action::End(Ending::Done));
    assert_eq!(rec.put_back, Some("check: exit 1".to_owned()));
    assert_eq!(rec.order_try, 3);
}

// Not halted.

/// Catches a Retry or a Put back taken by a turn that is not halted, changing it: before the
/// stage, while it drains, while it waits for a reboot, once done, once deferred, and once put
/// back, each is refused naming the event and where the turn stands.
#[test]
fn a_retry_or_a_put_back_of_a_turn_that_is_not_halted_is_refused() {
    let new = record(&[Kind::Linux], &["js-agent"]);
    let draining = ok_until(new.clone(), &drain("js-agent"));
    let applying = ok_until(new.clone(), &apply(Kind::Linux));
    let (started, _) = on(&applying, Event::Started);
    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(1)));
    let done = ok_until(new.clone(), &Action::End(Ending::Done));
    let (_, deferred, _) = run_step(&new, fail("exit 1"));
    let halted = halted_at(&CHECK, fail("exit 1"));
    let (put_back, _) = on(&halted, Event::PutBack { order_try: 2 });
    let put_back = ok_until(put_back, &Action::End(Ending::Done));
    let turns = [
        (&new, "stage"),
        (&draining, "drain js-agent"),
        (&rebooting, "wait for the reboot of linux"),
        (&done, "done"),
        (&deferred, "deferred"),
        (&put_back, "done"),
    ];
    for (rec, at) in turns {
        for (event, name) in [
            (retried(5), "retried"),
            (Event::PutBack { order_try: 5 }, "put back"),
        ] {
            assert_eq!(
                transition(rec, event),
                Err(TransitionError {
                    event: name,
                    at: at.to_owned(),
                }),
                "{at}"
            );
        }
    }
}
