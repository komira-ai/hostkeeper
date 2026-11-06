//! Waits: one period after a heartbeat, cut short by the end of a running order, so its verdict
//! goes out at once.

use std::collections::VecDeque;
use std::sync::Mutex;

use hostkeeper_link::{BeatOutcome, Outcome, Sleeper, Tick};

use crate::{Rig, abort, install, recorded, reply, update, verdict, version};

/// What the sleeper does in one wait, after noting how long it was asked to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    /// The wait passes and nothing ends in it.
    Passes,
    /// The order on the worker ends in the wait with its verdict, and the executor's step record
    /// lists the service at 2.0, as an install leaves it.
    OrderEnds,
    /// The worker of the order dies in the wait, with no verdict.
    WorkerDies,
    /// The loop is stopped.
    Stops,
}

/// A sleeper that plays a script, one entry per wait, and stops the loop once it is out.
struct Script<'a> {
    rig: &'a Rig,
    script: Mutex<VecDeque<Then>>,
    waits: Mutex<Vec<u64>>,
}

impl<'a> Script<'a> {
    fn new(rig: &'a Rig, script: &[Then]) -> Script<'a> {
        Script {
            rig,
            script: Mutex::new(script.iter().copied().collect()),
            waits: Mutex::new(Vec::new()),
        }
    }

    /// The waits asked of it, in seconds.
    fn waits(&self) -> Vec<u64> {
        self.waits.lock().expect("not poisoned").clone()
    }
}

impl Sleeper for Script<'_> {
    fn sleep(&self, secs: u64) -> Tick {
        self.waits.lock().expect("not poisoned").push(secs);
        let then = self
            .script
            .lock()
            .expect("not poisoned")
            .pop_front()
            .unwrap_or(Then::Stops);
        match then {
            Then::Passes => Tick::Beat,
            Then::OrderEnds => {
                assert!(self.rig.world.dispatch.run_next(), "an order runs");
                self.rig.world.records.set(vec![recorded("x", "2.0")]);
                Tick::Beat
            }
            Then::WorkerDies => {
                assert!(self.rig.world.dispatch.drop_next(), "an order runs");
                Tick::Beat
            }
            Then::Stops => Tick::Stop,
        }
    }
}

/// The loop over `rig` run with `sleeper` to its stop: what each heartbeat came to.
fn run(rig: &Rig, sleeper: &Script<'_>) -> Vec<BeatOutcome> {
    let mut seen = Vec::new();
    rig.heartbeater()
        .run(sleeper, &mut |outcome| seen.push(outcome.clone()));
    seen
}

/// Catches an install whose verdict, and the service it lists, wait for the end of the period
/// they ended in: the host is ordered to install `x`, which it does not run; the install ends in
/// the first second of the wait; the next heartbeat goes out after that one second, lists `x` at
/// 2.0 with the install's verdict, and the wait after it is a whole period again.
#[test]
fn an_order_that_ends_in_the_wait_is_reported_at_once_and_the_period_resumes() {
    let rig = Rig::new("waits-install");
    let order = install("i-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    let sleeper = Script::new(&rig, &[Then::OrderEnds, Then::Stops]);

    assert_eq!(run(&rig, &sleeper), [BeatOutcome::Sent, BeatOutcome::Sent]);
    assert_eq!(sleeper.waits(), [1, 15]);
    let beats = rig.world.link.beats();
    assert!(beats[0].services.is_empty(), "{:?}", beats[0]);
    assert_eq!(beats[1].services.len(), 1);
    let listed = &beats[1].services[0];
    assert_eq!(listed.name.as_str(), "x");
    assert_eq!(listed.version, version("2.0"));
    assert_eq!(listed.updating, None);
    assert_eq!(
        listed.last_update,
        Some(verdict(&order, Outcome::Succeeded))
    );
}

/// Catches a loop that, waiting a second at a time for an order, never beats until the order
/// ends, or waits longer than its period: an update outlasts a whole period; the loop beats at
/// the period, reporting the update running, then goes on waiting a second at a time and beats
/// once more as soon as the update ends in the third second.
#[test]
fn an_order_that_outlasts_the_period_is_beaten_at_the_period_then_at_its_end() {
    let rig = Rig::new("waits-long");
    rig.install(vec![recorded("x", "1.0")]);
    let order = update("u-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order.clone()), &[])));
    let mut script = vec![Then::Passes; 15];
    script.extend([Then::Passes, Then::Passes, Then::OrderEnds, Then::Stops]);
    let sleeper = Script::new(&rig, &script);

    assert_eq!(
        run(&rig, &sleeper),
        [BeatOutcome::Sent, BeatOutcome::Sent, BeatOutcome::Sent]
    );
    let mut waits = vec![1; 18];
    waits.push(15);
    assert_eq!(sleeper.waits(), waits);
    let beats = rig.world.link.beats();
    assert_eq!(beats[1].services[0].updating, Some(version("2.0")));
    assert_eq!(beats[1].services[0].last_update, None);
    assert_eq!(beats[2].services[0].updating, None);
    assert_eq!(
        beats[2].services[0].last_update,
        Some(verdict(&order, Outcome::Succeeded))
    );
}

/// Catches a stop asked for while an order runs that the loop does not take, or takes only at
/// the end of the period: the sleeper says stop in the second second of the wait, and the loop
/// ends with no other heartbeat.
#[test]
fn a_stop_in_the_wait_for_an_order_ends_the_loop_with_no_other_beat() {
    let rig = Rig::new("waits-stop");
    rig.install(vec![recorded("x", "1.0")]);
    let order = update("u-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order), &[])));
    let sleeper = Script::new(&rig, &[Then::Passes, Then::Stops]);

    assert_eq!(run(&rig, &sleeper), [BeatOutcome::Sent]);
    assert_eq!(sleeper.waits(), [1, 1]);
    assert_eq!(rig.world.link.calls().len(), 1);
    assert_eq!(rig.world.dispatch.pending(), 1, "the order was never run");
}

/// Catches a worker that died with no verdict keeping the loop waiting a second at a time for
/// good, or its end not said: the loop beats at once, with no verdict to send, says the order
/// ended with no verdict, and waits a whole period again.
#[test]
fn a_worker_that_dies_in_the_wait_ends_the_wait_with_no_verdict() {
    let rig = Rig::new("waits-dies");
    rig.install(vec![recorded("x", "1.0")]);
    let order = update("u-1", 1, "x", "2.0");
    rig.world.link.answer(Ok(reply(Some(order), &[])));
    let sleeper = Script::new(&rig, &[Then::WorkerDies, Then::Stops]);

    assert_eq!(run(&rig, &sleeper), [BeatOutcome::Sent, BeatOutcome::Sent]);
    assert_eq!(sleeper.waits(), [1, 15]);
    let beats = rig.world.link.beats();
    assert_eq!(beats[1].services[0].updating, None);
    assert_eq!(beats[1].services[0].last_update, None);
    assert_eq!(
        rig.world.log.lines(),
        ["error: order u-1 try 1 of x ended with no verdict"]
    );
}

/// Catches an abort, whose answer is logged and never sent, cutting the wait short: with no
/// order running, an abort of a listed service is handed to its own worker and the loop waits
/// its whole period.
#[test]
fn an_abort_alone_does_not_cut_the_wait() {
    let rig = Rig::new("waits-abort");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world
        .link
        .answer(Ok(reply(Some(abort("a-1", "x")), &[])));
    let sleeper = Script::new(&rig, &[Then::Stops]);

    assert_eq!(run(&rig, &sleeper), [BeatOutcome::Sent]);
    assert_eq!(sleeper.waits(), [15]);
    assert_eq!(
        rig.world.dispatch.pending(),
        1,
        "the abort is on its worker"
    );
}
