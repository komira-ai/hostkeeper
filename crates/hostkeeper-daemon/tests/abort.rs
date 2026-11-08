//! `abort` through `Daemon::abort` and the coordinator's order: a running step
//! stopped by name and its update ended `Halt`, a step that could not be stopped ended and its
//! service let go, and a service with nothing running refused and untouched. All on fakes: a
//! launcher that holds a step until it is stopped or released, a clock that moves when told, and an
//! alert sink.
//!
//! Every wait here ends: each names the party that would send what it waits for (the thread of an
//! order or an abort, the dead-man's ticks on the clock), and fails the test, naming what ended,
//! when that party is over first. An abort is run on a thread of its own and checked once it waits
//! for the order, so an abort that stops nothing, or a wrong unit, fails rather than waits for an
//! order whose step is held until it is stopped.

mod support;

use std::sync::Arc;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::boot::{ALERT_UNSTOPPABLE, alert_line};
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::testing::{FakeClock, Spawned, lock_waits, manifest, spawn};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::verbs::abort::{ABORT_STOP_ATTEMPTS, Aborted};
use hostkeeper_daemon::{Daemon, Refusal, Refused, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{Abort, Order, order};
use hostkeeper_steps::record::{PARAMS_FILE, Record, State};
use hostkeeper_steps::runner::{REASON_ABORTED, REASON_UNSTOPPABLE};
use support::{Rig, alerting, d, lines, name, snapshot, update};

/// The reply of an order that ended `Halt` for `reason`.
fn halted(reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Halt,
        reason: Some(reason.to_owned()),
        resent: false,
    }
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

/// The state of a record that ended `Halt` for `reason`.
fn halt_state(reason: &str) -> State {
    State::Finished {
        verdict: Verdict::Halt,
        reason: Some(reason.to_owned()),
    }
}

/// A coordinator's `abort` of `service`, try `order_try`.
fn abort_order(service: &str, order_try: u64) -> Order {
    abort_order_id(&format!("abort-{service}"), service, order_try)
}

/// A coordinator's `abort` of `service` with the order id `order_id`, try `order_try`.
fn abort_order_id(order_id: &str, service: &str, order_try: u64) -> Order {
    Order {
        order_id: order_id.to_owned(),
        r#try: order_try,
        verb: Some(order::Verb::Abort(Abort {
            service: service.to_owned(),
        })),
    }
}

/// `order` run by `daemon` on a thread of its own, named `name` in what a missed signal says.
fn run(daemon: &Arc<Daemon>, name: &str, order: Order) -> Spawned<Reply> {
    let daemon = Arc::clone(daemon);
    spawn(name, move || daemon.order(&order))
}

/// The abort of `service` by `daemon`, on a thread of its own.
fn abort_of(daemon: &Arc<Daemon>, service: &str) -> Spawned<Result<Aborted, Refused>> {
    let (daemon, service) = (Arc::clone(daemon), name(service));
    spawn("the abort", move || daemon.abort(&service))
}

/// `service` at `version` with an unbounded `prepare` (`prepare = 0`), installed over the base.
fn unbounded(rig: &Rig, service: &str, version: &str) {
    rig.add(service, "0.9.0");
    let text = manifest(service, version).replace("prepare = 600", "prepare = 0");
    rig.store.add(service, version, &text);
}

/// The record of `service`, which must be there.
fn record(rig: &Rig, service: &str) -> Record {
    rig.record(service).expect("a step record")
}

/// A daemon whose dead-man watches the steps on a clock that moves only when told: an update of
/// `alpha` 2.0.0 whose `prepare` ran past its bound and could not be stopped, so its runner
/// stopped `Unstopped` and its service is locked for good. Every stop of `alpha` fails until the
/// test allows it.
fn unstopped(rig: &Rig) -> Arc<Daemon> {
    rig.add("alpha", "2.0.0");
    rig.add("alpha", "3.0.0");
    rig.add("beta", "2.0.0");
    rig.launcher.fail_stops("alpha");
    let stops = rig.launcher.stops_seen();
    let gate = rig.launcher.hold("alpha", Step::Prepare);
    let clock = FakeClock::frozen();
    let mut ports = rig.ports();
    ports.clock = clock.clone();
    ports.deadman_poll_secs = Some(1);
    let daemon = Arc::new(Daemon::new(
        support::config(),
        rig.dirs(),
        ports,
        support::OWN,
    ));
    let first = run(&daemon, "alpha's order", update("alpha", "2.0.0", &d(), 1));
    gate.entered(&first).expect("alpha's prepare runs");
    clock.advance(601);
    // The dead-man ticks, then sleeps on the clock: by its second sleep from here, one whole tick
    // has looked past the bound.
    let ticked = clock.sleeps(2);
    assert_eq!(
        stops.wait(&ticked).expect("the dead-man's stop"),
        "updater-alpha-prepare-1"
    );
    gate.release();
    let reply = first.join().expect("the order");
    assert_eq!(reply.refusal(), Some(Refusal::Unstopped), "{reply:?}");
    daemon
}

/// Catches an abort that waits for a bound, stops a wrong unit, runs a rollback, leaves the
/// lock held or the record `running`: an unbounded `prepare` held until its unit is stopped is
/// aborted by `Daemon::abort` from another thread; the one stop names the record's unit,
/// the order's own reply is `Halt` `aborted_by_operator`, the record says so, no step ran after
/// `prepare`, the params file is gone, a later order of the service is `halted` (not `busy`),
/// and another service's order that waited for the step lock meanwhile ran to its end.
#[test]
fn abort_stops_an_unbounded_prepare_and_halts_the_order() {
    let rig = Rig::new("abort-unbounded");
    unbounded(&rig, "web", "2.0.0");
    rig.add("web", "2.1.0");
    rig.add("beta", "2.0.0");
    let gate = rig.launcher.hold_until_stopped("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = run(&daemon, "web's order", update("web", "2.0.0", &d(), 1));
    gate.entered(&order).expect("web's prepare runs");
    let asked = rig.store.notified();
    let other = run(&daemon, "beta's order", update("beta", "2.0.0", &d(), 1));
    assert_eq!(
        asked.wait(&other).expect("beta has its package"),
        "beta 2.0.0"
    );
    let unit = record(&rig, "web").unit(Step::Prepare);

    let waits = lock_waits(&daemon.ctx().locks);
    let abort = abort_of(&daemon, "web");
    // The abort stops the step's unit, then waits for the order to let go of the lock.
    assert_eq!(
        waits.wait(&abort).expect("the abort waits for the order"),
        "web"
    );
    assert_eq!(rig.launcher.stops(), std::slice::from_ref(&unit));
    let aborted = abort.join().expect("the abort").expect("aborted");

    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    assert_eq!(aborted.record.state, halt_state(REASON_ABORTED));
    assert_eq!(aborted.record.step, Step::Prepare);
    assert_eq!(aborted.unit.as_deref(), Some(unit.as_str()));
    let rec = record(&rig, "web");
    assert_eq!(rec.state, halt_state(REASON_ABORTED));
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
    assert!(!rig.state.join("web").join(PARAMS_FILE).exists());
    assert_eq!(daemon.ctx().locks.holder(&name("web")), None);
    let later = daemon.order(&update("web", "2.1.0", &d(), 2));
    assert_eq!(later.refusal(), Some(Refusal::Halted), "{later:?}");
    assert_eq!(other.join().expect("beta's order"), succeeded());
}

/// Catches an abort read only after the next step is launched: the abort comes while `prepare`
/// is running and its unit is stopped, the step then returns, and `cutover` is never launched;
/// the update ends `Halt` `aborted_by_operator`.
#[test]
fn abort_between_two_steps_ends_the_order_before_the_next_step_launches() {
    let rig = Rig::new("abort-between");
    rig.add("web", "2.0.0");
    let gate = rig.launcher.hold("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = run(&daemon, "the order", update("web", "2.0.0", &d(), 1));
    gate.entered(&order).expect("prepare runs");
    let waits = lock_waits(&daemon.ctx().locks);
    let abort = abort_of(&daemon, "web");
    // The abort has asked the runner to end the update, stopped the unit, and waits for the
    // order; the step then returns.
    assert_eq!(
        waits.wait(&abort).expect("the abort waits for the order"),
        "web"
    );
    assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);
    gate.release();
    let aborted = abort.join().expect("the abort").expect("aborted");
    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
    assert_eq!(aborted.unit.as_deref(), Some("updater-web-prepare-1"));
    assert_eq!(aborted.record.state, halt_state(REASON_ABORTED));
}

/// What a variant of [`abort_while_waiting`] leaves out, so that a signal its waits name never
/// comes: each variant fails, naming what ended, where the scenario would otherwise wait for good.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Missing {
    /// The scenario as it is: every signal comes.
    Nothing,
    /// The order is refused before it takes the service's lock (its digest is not one), so its
    /// runner never waits between two tries.
    OrderReachesTheLock,
    /// The abort names a service that runs nothing, so it never waits for the order.
    AbortWaitsForTheOrder,
}

/// The scenario of [`abort_while_the_runner_waits_between_two_steps_stops_nothing_and_launches_nothing_more`],
/// with `missing` left out.
fn abort_while_waiting(rig_name: &str, missing: Missing) {
    let rig = Rig::new(rig_name);
    rig.add("web", "2.0.0");
    rig.launcher.exits("web", Step::Verify, &[75]);
    let sleep = rig.clock.park_next_sleep();
    let daemon = Arc::new(rig.daemon());
    let digest = match missing {
        Missing::OrderReachesTheLock => "not-a-digest".to_owned(),
        Missing::Nothing | Missing::AbortWaitsForTheOrder => d(),
    };
    let order = run(&daemon, "the order", update("web", "2.0.0", &digest, 1));
    sleep
        .entered(&order)
        .expect("the runner waits between two tries");
    let waits = lock_waits(&daemon.ctx().locks);
    let aborted = match missing {
        Missing::AbortWaitsForTheOrder => "beta",
        Missing::Nothing | Missing::OrderReachesTheLock => "web",
    };
    let abort = abort_of(&daemon, aborted);
    // The abort has asked the runner to end the update and waits for the order; then the
    // runner's wait ends. The record says the deferred `verify` is done, no step is running.
    assert_eq!(
        waits.wait(&abort).expect("the abort waits for the order"),
        "web"
    );
    assert!(matches!(record(&rig, "web").state, State::Done { .. }));
    sleep.release();
    let aborted = abort.join().expect("the abort").expect("aborted");
    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    assert_eq!(aborted.unit, None);
    assert!(rig.launcher.stops().is_empty());
    let steps = [Step::Prepare, Step::Cutover, Step::Verify];
    assert_eq!(rig.launcher.steps("web"), steps);
}

/// Catches an abort that waits for a step it cannot see, or stops a unit that is not running:
/// while the runner waits between two tries of `verify` (it deferred), nothing is stopped, and
/// the update ends `Halt` before `verify` is launched again.
#[test]
fn abort_while_the_runner_waits_between_two_steps_stops_nothing_and_launches_nothing_more() {
    abort_while_waiting("abort-waiting", Missing::Nothing);
}

/// Catches a wait for the runner that does not end when the order never reaches the lock: the
/// order is refused before it takes the service's lock, and the wait for its runner's sleep fails,
/// naming the order's refusal, where it waited for good before.
#[test]
#[should_panic(
    expected = r#"the runner waits between two tries: Missed { over: "the order returned Refused { refusal: InvalidOrder"#
)]
fn a_wait_for_a_runner_whose_order_never_reaches_the_lock_fails_naming_the_order() {
    abort_while_waiting("abort-no-lock", Missing::OrderReachesTheLock);
}

/// Catches a wait for the abort that does not end when the abort never waits for the order (the
/// spin on a count of waiters, before): an abort of a service that runs nothing is refused, and
/// the wait fails, naming the abort's refusal.
#[test]
#[should_panic(
    expected = r#"the abort waits for the order: Missed { over: "the abort returned Err(Refused { refusal: NothingRunning"#
)]
fn a_wait_for_an_abort_that_never_waits_for_the_order_fails_naming_the_abort() {
    abort_while_waiting("abort-no-wait", Missing::AbortWaitsForTheOrder);
}

/// Catches a retry count that is not the constant, an alert missing or doubled, a record that
/// says `aborted_by_operator` for a unit still running, or a lock kept: before the abort the
/// service is locked for good (another order is `busy`, `Status` shows it stuck, another service
/// still runs); the abort tries the stop `ABORT_STOP_ATTEMPTS` times, closes the record
/// `unstoppable`, raises one alert naming the service, the unit and the launcher's error, and
/// lets go of the lock, so the next order is `halted`.
#[test]
fn abort_that_cannot_stop_the_unit_marks_unstoppable() {
    let rig = Rig::new("abort-unstoppable");
    let daemon = unstopped(&rig);
    let alpha = name("alpha");
    let other = daemon.order(&update("alpha", "3.0.0", &d(), 2));
    assert_eq!(other.refusal(), Some(Refusal::Busy), "{other:?}");
    let status = daemon.status().expect("status");
    assert!(status.services[0].stuck);
    assert_eq!(daemon.order(&update("beta", "2.0.0", &d(), 1)), succeeded());
    let daemon = Arc::into_inner(daemon).expect("the only daemon");
    let (daemon, log) = alerting(daemon);
    let before = rig.launcher.stops().len();

    let aborted = daemon.abort(&alpha).expect("aborted");

    let unit = "updater-alpha-prepare-1";
    let tried = &rig.launcher.stops()[before..];
    assert_eq!(
        tried.len(),
        usize::try_from(ABORT_STOP_ATTEMPTS).expect("a count")
    );
    assert!(tried.iter().all(|stopped| stopped == unit), "{tried:?}");
    assert_eq!(aborted.unit.as_deref(), Some(unit));
    assert_eq!(aborted.record.state, halt_state(REASON_UNSTOPPABLE));
    assert_eq!(record(&rig, "alpha").state, halt_state(REASON_UNSTOPPABLE));
    let detail = format!(
        "service=alpha unit={unit}: an abort could not stop it: \
         the launcher failed: {unit} could not be stopped"
    );
    assert_eq!(lines(&log), [alert_line(ALERT_UNSTOPPABLE, &detail)]);
    assert_eq!(daemon.ctx().locks.holder(&alpha), None);
    assert!(!rig.state.join("alpha").join(PARAMS_FILE).exists());
    let next = daemon.order(&update("alpha", "3.0.0", &d(), 3));
    assert_eq!(next.refusal(), Some(Refusal::Halted), "{next:?}");
}

/// Catches a first failed stop taken as final: two stops fail and the third works; the record is
/// `aborted_by_operator` after three calls, and nothing is alerted.
#[test]
fn abort_retries_a_stop_that_fails_then_works() {
    let rig = Rig::new("abort-retry");
    let daemon = unstopped(&rig);
    rig.launcher.allow_stops("alpha");
    rig.launcher.fail_stops_times("alpha", 2);
    let daemon = Arc::into_inner(daemon).expect("the only daemon");
    let (daemon, log) = alerting(daemon);
    let before = rig.launcher.stops().len();
    let aborted = daemon.abort(&name("alpha")).expect("aborted");
    assert_eq!(rig.launcher.stops().len() - before, 3);
    assert_eq!(aborted.record.state, halt_state(REASON_ABORTED));
    assert!(lines(&log).is_empty(), "{:?}", lines(&log));
}

/// Catches the unstopped case handled only when the stop fails: the unit of a step that could
/// not be stopped at its bound has since exited, the abort's one stop works, the record is
/// `aborted_by_operator`, and the service's lock is let go.
#[test]
fn abort_of_an_unstopped_service_whose_unit_has_since_exited_halts_aborted_by_operator() {
    let rig = Rig::new("abort-exited");
    let daemon = unstopped(&rig);
    rig.launcher.allow_stops("alpha");
    let before = rig.launcher.stops().len();
    let aborted = daemon.abort(&name("alpha")).expect("aborted");
    assert_eq!(rig.launcher.stops()[before..], ["updater-alpha-prepare-1"]);
    assert_eq!(record(&rig, "alpha").state, halt_state(REASON_ABORTED));
    assert_eq!(aborted.record.state, halt_state(REASON_ABORTED));
    assert_eq!(daemon.ctx().locks.holder(&name("alpha")), None);
}

/// Catches an abort that invents a halt for an idle service: a service with no record, one whose
/// update ended, and one whose update was deferred are each refused `nothing_running`, as an
/// order and as a call, and every file of the state directory is as it was.
#[test]
fn abort_with_no_step_running_is_refused_and_writes_nothing() {
    let rig = Rig::new("abort-idle");
    rig.add("fresh", "2.0.0");
    rig.add("done", "2.0.0");
    rig.add("later", "2.0.0");
    rig.launcher.exits("later", Step::Prepare, &[75]);
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("done", "2.0.0", &d(), 1)), succeeded());
    let deferred = daemon.order(&update("later", "2.0.0", &d(), 1));
    assert!(
        matches!(
            deferred,
            Reply::Finished {
                verdict: Verdict::Deferred { .. },
                ..
            }
        ),
        "{deferred:?}"
    );
    std::fs::create_dir_all(&rig.state).expect("the state dir");
    let before = snapshot(&rig.state);
    for service in ["fresh", "done", "later"] {
        let refused = daemon.abort(&name(service)).expect_err(service);
        let detail = format!("no step of {service} is running");
        assert_eq!(
            refused,
            Refused::new(Refusal::NothingRunning, detail),
            "{service}"
        );
        let reply = daemon.order(&abort_order(service, 1));
        assert_eq!(reply.refusal(), Some(Refusal::NothingRunning), "{service}");
    }
    assert_eq!(snapshot(&rig.state), before);
    assert!(rig.launcher.stops().is_empty());
}

/// Catches the two entry points diverging: the scenario of the first test, aborted once by the
/// coordinator's order and once by `Daemon::abort` in the process, ends in the same record
/// (state, step, attempt, generation), with the same stop.
#[test]
fn abort_by_coordinator_order_and_by_daemon_abort_end_in_the_same_state() {
    let mut ended = Vec::new();
    for by_order in [true, false] {
        let rig = Rig::new(if by_order {
            "abort-by-order"
        } else {
            "abort-by-call"
        });
        unbounded(&rig, "web", "2.0.0");
        let gate = rig.launcher.hold_until_stopped("web", Step::Prepare);
        let daemon = Arc::new(rig.daemon());
        let order = run(&daemon, "the order", update("web", "2.0.0", &d(), 1));
        gate.entered(&order).expect("prepare runs");
        let waits = lock_waits(&daemon.ctx().locks);
        // Either way the abort stops the unit, then waits for the order to let go of the lock.
        match by_order {
            true => {
                let abort = run(&daemon, "the abort", abort_order("web", 1));
                let waited = waits.wait(&abort).expect("the abort waits for the order");
                assert_eq!(waited, "web");
                assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);
                assert_eq!(abort.join().expect("the abort"), halted(REASON_ABORTED));
            }
            false => {
                let abort = abort_of(&daemon, "web");
                let waited = waits.wait(&abort).expect("the abort waits for the order");
                assert_eq!(waited, "web");
                assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);
                abort.join().expect("the abort").expect("aborted");
            }
        }
        assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
        let rec = record(&rig, "web");
        ended.push((
            rec.state,
            rec.step,
            rec.attempt,
            rec.generation,
            rig.launcher.stops(),
        ));
    }
    assert_eq!(ended[0], ended[1]);
}

/// Catches an abort that is global or waits on the step lock: while `alpha` runs a step (holding
/// the step lock) and `beta`'s order waits for it, an abort of `beta` is refused at once (no step
/// of it runs); the abort of `alpha` stops only `alpha`'s unit, and `beta` then runs its step,
/// its record and unit untouched by the abort.
#[test]
fn abort_takes_no_step_lock_and_touches_only_its_service() {
    let rig = Rig::new("abort-only");
    unbounded(&rig, "alpha", "2.0.0");
    rig.add("beta", "2.0.0");
    let alpha_gate = rig.launcher.hold_until_stopped("alpha", Step::Prepare);
    let beta_gate = rig.launcher.hold("beta", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let alpha = run(&daemon, "alpha's order", update("alpha", "2.0.0", &d(), 1));
    alpha_gate.entered(&alpha).expect("alpha's prepare runs");
    let asked = rig.store.notified();
    let beta = run(&daemon, "beta's order", update("beta", "2.0.0", &d(), 1));
    assert_eq!(
        asked.wait(&beta).expect("beta has its package"),
        "beta 2.0.0"
    );
    let refused = daemon.abort(&name("beta")).expect_err("beta runs no step");
    assert_eq!(refused.refusal, Refusal::NothingRunning);

    let waits = lock_waits(&daemon.ctx().locks);
    let abort = abort_of(&daemon, "alpha");
    assert_eq!(
        waits
            .wait(&abort)
            .expect("the abort waits for alpha's order"),
        "alpha"
    );
    assert_eq!(rig.launcher.stops(), ["updater-alpha-prepare-1"]);
    abort.join().expect("the abort").expect("alpha aborted");
    assert_eq!(alpha.join().expect("alpha"), halted(REASON_ABORTED));
    beta_gate.entered(&beta).expect("beta's prepare runs");
    assert!(matches!(record(&rig, "beta").state, State::Running { .. }));
    assert_eq!(rig.launcher.stops(), ["updater-alpha-prepare-1"]);
    beta_gate.release();
    assert_eq!(beta.join().expect("beta"), succeeded());
}

/// Catches try rules that do not cover `abort`, or that take another order for a resend: the
/// same try of an abort order sent twice is answered from the first answer (marked resent) and
/// stops nothing more; another try, and another abort order with the same try, are new aborts,
/// which find nothing running.
#[test]
fn a_resent_abort_order_is_answered_from_the_first_answer() {
    let rig = Rig::new("abort-resend");
    rig.add("web", "2.0.0");
    let gate = rig.launcher.hold_until_stopped("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = run(&daemon, "the order", update("web", "2.0.0", &d(), 1));
    gate.entered(&order).expect("prepare runs");
    let waits = lock_waits(&daemon.ctx().locks);
    let abort = run(&daemon, "the abort", abort_order("web", 4));
    assert_eq!(
        waits.wait(&abort).expect("the abort waits for the order"),
        "web"
    );
    assert_eq!(rig.launcher.stops(), ["updater-web-prepare-1"]);
    assert_eq!(abort.join().expect("the abort"), halted(REASON_ABORTED));
    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    let resent = Reply::Finished {
        verdict: Verdict::Halt,
        reason: Some(REASON_ABORTED.to_owned()),
        resent: true,
    };
    assert_eq!(daemon.order(&abort_order("web", 4)), resent);
    assert_eq!(rig.launcher.stops().len(), 1);
    let again = daemon.order(&abort_order("web", 5));
    assert_eq!(again.refusal(), Some(Refusal::NothingRunning), "{again:?}");
    assert_eq!(daemon.order(&abort_order("web", 5)), again);
    let other = daemon.order(&abort_order_id("abort-web-later", "web", 4));
    assert_eq!(other.refusal(), Some(Refusal::NothingRunning), "{other:?}");
    assert_eq!(rig.launcher.stops().len(), 1);
}

/// Catches an abort that writes `aborted_by_operator` for a unit still running, waits for it, or
/// lets go of the order's lock: every stop of a running step fails, so the abort closes the
/// record `unstoppable` at once with one alert, and the order keeps the service's lock until its
/// step ends; the order's own reply is then `Halt` `unstoppable`, and the lock is let go.
#[test]
fn abort_of_a_running_step_that_cannot_be_stopped_closes_it_unstoppable_and_the_order_keeps_the_lock()
 {
    let rig = Rig::new("abort-running-unstoppable");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    rig.launcher.fail_stops("web");
    let gate = rig.launcher.hold("web", Step::Prepare);
    let (daemon, log) = alerting(rig.daemon());
    let daemon = Arc::new(daemon);
    let order = run(&daemon, "the order", update("web", "2.0.0", &d(), 1));
    gate.entered(&order).expect("prepare runs");
    // Every stop fails: the abort closes the record and returns without waiting for the order.
    let aborted = daemon.abort(&name("web")).expect("aborted");
    assert_eq!(aborted.record.state, halt_state(REASON_UNSTOPPABLE));
    assert_eq!(lines(&log).len(), 1);
    let held = daemon.ctx().locks.holder(&name("web")).expect("held");
    assert!(!held.stuck);
    gate.release();
    assert_eq!(order.join().expect("the order"), halted(REASON_UNSTOPPABLE));
    assert_eq!(record(&rig, "web").state, halt_state(REASON_UNSTOPPABLE));
    assert_eq!(daemon.ctx().locks.holder(&name("web")), None);
    let next = daemon.order(&update("web", "2.1.0", &d(), 2));
    assert_eq!(next.refusal(), Some(Refusal::Halted), "{next:?}");
    assert_eq!(lines(&log).len(), 1);
}

/// Catches an abort that lets go of a lock its record does not explain, or reports success with
/// no record: a lock held for good while the record says the update ended (a step of the boot
/// work holds it) is `nothing_running` and stays held; an unstopped service whose record is gone
/// is `store_failed`, and its lock stays held too.
#[test]
fn abort_refuses_a_held_lock_its_record_does_not_name() {
    let rig = Rig::new("abort-not-named");
    rig.add("web", "2.0.0");
    let daemon = rig.daemon();
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded());
    let package = Package {
        verb: HookedVerb::Update,
        version: Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    };
    let web: ServiceName = name("web");
    daemon.ctx().locks.take(&web, package).expect("free").keep();
    let refused = daemon.abort(&web).expect_err("the record names no step");
    assert_eq!(refused.refusal, Refusal::NothingRunning);
    assert!(daemon.ctx().locks.holder(&web).expect("held").stuck);

    let rig = Rig::new("abort-record-gone");
    let daemon = unstopped(&rig);
    std::fs::remove_file(rig.state.join("alpha").join("step-record.json")).expect("removed");
    let refused = daemon.abort(&name("alpha")).expect_err("no record");
    let gone = "the step record of the aborted update is gone";
    assert_eq!(refused, Refused::new(Refusal::StoreFailed, gone));
    assert!(
        daemon
            .ctx()
            .locks
            .holder(&name("alpha"))
            .expect("held")
            .stuck
    );
}

/// Catches a lock kept for good when the params file of an unstopped update cannot be removed:
/// the abort closes the record and lets the lock go, then answers `store_failed` for the file.
#[test]
fn an_unstopped_abort_whose_params_file_cannot_be_removed_still_lets_the_lock_go() {
    let rig = Rig::new("abort-params-stuck");
    let daemon = unstopped(&rig);
    rig.launcher.allow_stops("alpha");
    let params = rig.state.join("alpha").join(PARAMS_FILE);
    std::fs::remove_file(&params).expect("the params file");
    std::fs::create_dir_all(params.join("in-the-way")).expect("a directory in its place");
    let refused = daemon
        .abort(&name("alpha"))
        .expect_err("the params file stays");
    assert_eq!(refused.refusal, Refusal::StoreFailed);
    assert_eq!(record(&rig, "alpha").state, halt_state(REASON_ABORTED));
    assert_eq!(daemon.ctx().locks.holder(&name("alpha")), None);
}
