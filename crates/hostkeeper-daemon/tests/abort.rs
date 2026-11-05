//! `abort` through `Daemon::abort`, the coordinator's order and the admin socket: a running step
//! stopped by name and its update ended `Halt`, a step that could not be stopped ended and its
//! service let go, and a service with nothing running refused and untouched. All on fakes: a
//! launcher that holds a step until it is stopped or released, a clock that moves when told, an
//! alert sink, and an in-process admin connection with an explicit peer.

mod support;

use std::sync::Arc;
use std::thread;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::admin::Peer;
use hostkeeper_daemon::boot::{ALERT_UNSTOPPABLE, alert_line};
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::testing::{FakeClock, manifest};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::verbs::abort::ABORT_STOP_ATTEMPTS;
use hostkeeper_daemon::{Daemon, Refusal, Refused, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{Abort, Order, order};
use hostkeeper_proto::hostkeeper::local::v1::AbortRequest;
use hostkeeper_steps::record::{PARAMS_FILE, Record, State};
use hostkeeper_steps::runner::{REASON_ABORTED, REASON_UNSTOPPABLE};
use support::{Rig, admin_from, alerting, d, lines, name, snapshot, update};

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
    let first = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("alpha", "2.0.0", &d(), 1)))
    };
    gate.entered();
    clock.advance(601);
    assert_eq!(
        stops.recv().expect("the dead-man's stop"),
        "updater-alpha-prepare-1"
    );
    gate.release();
    let reply = first.join().expect("the order");
    assert_eq!(reply.refusal(), Some(Refusal::Unstopped), "{reply:?}");
    daemon
}

/// Catches an abort that waits for a bound, stops a wrong unit, runs a rollback, leaves the
/// lock held or the record `running`: an unbounded `prepare` held until its unit is stopped is
/// aborted through the admin socket from another thread; the one stop names the record's unit,
/// the order's own reply is `Halt` `aborted_by_operator`, the record says so, no step ran after
/// `prepare`, the params file is gone, a later order of the service is `halted` (not `busy`),
/// and another service's order that waited for the step lock meanwhile ran to its end.
#[tokio::test(flavor = "multi_thread")]
async fn abort_stops_an_unbounded_prepare_and_halts_the_order() {
    let rig = Rig::new("abort-unbounded");
    unbounded(&rig, "web", "2.0.0");
    rig.add("web", "2.1.0");
    rig.add("beta", "2.0.0");
    let gate = rig.launcher.hold_until_stopped("web", Step::Prepare);
    let daemon = Arc::new(rig.daemon());
    let order = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    gate.entered();
    let (asked, waited) = std::sync::mpsc::channel();
    rig.store.notify(asked);
    let other = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("beta", "2.0.0", &d(), 1)))
    };
    assert_eq!(waited.recv().expect("beta has its package"), "beta 2.0.0");
    let unit = record(&rig, "web").unit(Step::Prepare);

    let (mut client, _) = admin_from(Arc::clone(&daemon), Peer { uid: Some(0) });
    let request = AbortRequest {
        service: "web".to_owned(),
    };
    let aborted = client.abort(request).await.expect("the abort").into_inner();

    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    let wire = aborted.record.expect("the record");
    assert_eq!(wire.reason, REASON_ABORTED);
    assert_eq!(wire.step, "prepare");
    assert_eq!(rig.launcher.stops(), [unit]);
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
    let stops = rig.launcher.stops_seen();
    let daemon = Arc::new(rig.daemon());
    let order = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    gate.entered();
    let abort = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.abort(&name("web")))
    };
    // The abort has asked the runner to end the update and stopped the unit; the step returns.
    assert_eq!(stops.recv().expect("the stop"), "updater-web-prepare-1");
    gate.release();
    let aborted = abort.join().expect("the abort").expect("aborted");
    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    assert_eq!(rig.launcher.steps("web"), [Step::Prepare]);
    assert_eq!(aborted.unit.as_deref(), Some("updater-web-prepare-1"));
    assert_eq!(aborted.record.state, halt_state(REASON_ABORTED));
}

/// Catches an abort that waits for a step it cannot see, or stops a unit that is not running:
/// while the runner waits between two tries of `verify` (it deferred), nothing is stopped, and
/// the update ends `Halt` before `verify` is launched again.
#[test]
fn abort_while_the_runner_waits_between_two_steps_stops_nothing_and_launches_nothing_more() {
    let rig = Rig::new("abort-waiting");
    rig.add("web", "2.0.0");
    rig.launcher.exits("web", Step::Verify, &[75]);
    let sleep = rig.clock.park_next_sleep();
    let daemon = Arc::new(rig.daemon());
    let order = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    sleep.entered();
    let abort = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.abort(&name("web")))
    };
    // The abort has asked the runner to end the update and waits for the order; then the
    // runner's wait ends. The record says the deferred `verify` is done, no step is running.
    while daemon.ctx().locks.waiting() == 0 {
        thread::yield_now();
    }
    assert!(matches!(record(&rig, "web").state, State::Done { .. }));
    sleep.release();
    let aborted = abort.join().expect("the abort").expect("aborted");
    assert_eq!(order.join().expect("the order"), halted(REASON_ABORTED));
    assert_eq!(aborted.unit, None);
    assert!(rig.launcher.stops().is_empty());
    let steps = [Step::Prepare, Step::Cutover, Step::Verify];
    assert_eq!(rig.launcher.steps("web"), steps);
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
/// coordinator's order and once by the admin socket, ends in the same record (state, step,
/// attempt, generation), with the same stop.
#[tokio::test(flavor = "multi_thread")]
async fn abort_by_coordinator_order_and_by_admin_socket_end_in_the_same_state() {
    let mut ended = Vec::new();
    for by_order in [true, false] {
        let rig = Rig::new(if by_order {
            "abort-by-order"
        } else {
            "abort-by-admin"
        });
        unbounded(&rig, "web", "2.0.0");
        let gate = rig.launcher.hold_until_stopped("web", Step::Prepare);
        let daemon = Arc::new(rig.daemon());
        let order = {
            let daemon = Arc::clone(&daemon);
            thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
        };
        gate.entered();
        match by_order {
            true => {
                let daemon = Arc::clone(&daemon);
                let reply =
                    tokio::task::spawn_blocking(move || daemon.order(&abort_order("web", 1)));
                assert_eq!(reply.await.expect("the abort"), halted(REASON_ABORTED));
            }
            false => {
                let (mut client, _) = admin_from(Arc::clone(&daemon), Peer { uid: Some(0) });
                let request = AbortRequest {
                    service: "web".to_owned(),
                };
                client.abort(request).await.expect("the abort");
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
    let run = |service: &'static str| {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update(service, "2.0.0", &d(), 1)))
    };
    let alpha = run("alpha");
    alpha_gate.entered();
    let (asked, waited) = std::sync::mpsc::channel();
    rig.store.notify(asked);
    let beta = run("beta");
    assert_eq!(waited.recv().expect("beta has its package"), "beta 2.0.0");
    let refused = daemon.abort(&name("beta")).expect_err("beta runs no step");
    assert_eq!(refused.refusal, Refusal::NothingRunning);

    daemon.abort(&name("alpha")).expect("alpha aborted");
    assert_eq!(alpha.join().expect("alpha"), halted(REASON_ABORTED));
    beta_gate.entered();
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
    let order = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    gate.entered();
    assert_eq!(daemon.order(&abort_order("web", 4)), halted(REASON_ABORTED));
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
    let order = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 1)))
    };
    gate.entered();
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
