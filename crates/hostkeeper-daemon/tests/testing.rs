//! The test doubles themselves, where the tests of the daemon do not reach every path of them.

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::locks::ServiceLocks;
use hostkeeper_daemon::testing::{
    Ending, FakeClock, Missed, ScriptedLauncher, Signal, lock_waits, spawn,
};
use hostkeeper_steps::{BootIdError, Clock, StepLauncher};

/// Catches a fake clock that moves on its own, or does not move when slept on: a sleep moves
/// it, a frozen one only by `advance`, a new boot id is reported, and a lost one cannot be read
/// until one is set again.
#[test]
fn the_fake_clock_moves_only_when_told() {
    let clock = FakeClock::new();
    let start = clock.mono_secs();
    clock.sleep_secs(5);
    assert_eq!(clock.mono_secs(), start + 5);
    let frozen = FakeClock::frozen();
    frozen.sleep_secs(5);
    assert_eq!(frozen.mono_secs(), start);
    frozen.advance(2);
    assert_eq!(frozen.mono_secs(), start + 2);
    let id = || clock.boot_id().map(|id| id.as_str().to_owned());
    assert_eq!(id().expect("a boot id"), "boot-1");
    clock.set_boot_id("boot-2");
    assert_eq!(id().expect("a boot id"), "boot-2");
    clock.lose_boot_id();
    assert!(matches!(
        clock.boot_id(),
        Err(BootIdError::Empty {
            from: "the fake clock"
        })
    ));
    clock.set_boot_id("boot-3");
    assert_eq!(id().expect("a boot id"), "boot-3");
}

/// Catches a scripted launcher whose stops always fail: a stop of a service not marked failing
/// works, and is seen.
#[test]
fn a_stop_the_script_does_not_fail_works() {
    let launcher = ScriptedLauncher::new();
    let seen = launcher.stops_seen();
    launcher.fail_stops("other");
    launcher
        .stop("updater-web-prepare-1")
        .expect("a stop that works");
    assert!(launcher.stop("updater-other-prepare-1").is_err());
    let stopped = Ending::default();
    stopped.over("the stops were asked for");
    assert_eq!(seen.wait(&stopped).expect("seen"), "updater-web-prepare-1");
    assert_eq!(
        seen.wait(&stopped).expect("seen"),
        "updater-other-prepare-1"
    );
    assert_eq!(launcher.stops().len(), 2);
}

/// Catches a wait that drops a signal its party sent before it ended: a thread sends, then ends;
/// the wait takes the signal however far the thread got, and the end is not mistaken for a miss.
#[test]
fn a_signal_sent_before_its_party_ended_is_taken() {
    let (notice, signal) = Signal::new();
    let sender = spawn("the sender", move || notice.send(7));
    assert_eq!(signal.wait(&sender), Ok(7));
    sender.join().expect("the sender");
}

/// Catches a wait that hangs, or fails without saying why, when its party ends without the
/// signal: the wait fails with what the thread returned, word for word; a thread that panics is
/// named as having panicked.
#[test]
fn a_wait_whose_party_ends_without_the_signal_fails_naming_what_ended() {
    let (_notice, signal) = Signal::<u32>::new();
    let silent = spawn("the silent thread", || 41 + 1);
    let missed = Missed {
        over: "the silent thread returned 42".to_owned(),
    };
    assert_eq!(signal.wait(&silent), Err(missed));
    assert_eq!(silent.join().expect("the silent thread"), 42);

    let panicking = spawn("the failing thread", || -> u32 { panic!("on purpose") });
    let missed = Missed {
        over: "the failing thread panicked".to_owned(),
    };
    assert_eq!(signal.wait(&panicking), Err(missed));
    assert!(panicking.join().is_err());
}

/// Catches a wait failed by the party of an earlier wait: the first wait's party is over once its
/// signal is taken, and a second wait, on a party not over, takes its own signal.
#[test]
fn the_end_of_an_earlier_waits_party_does_not_end_a_later_wait() {
    let (notice, signal) = Signal::new();
    notice.send("first");
    let first = Ending::default();
    first.over("the first party ended");
    assert_eq!(signal.wait(&first), Ok("first"));
    let second = Ending::default();
    notice.send("second");
    assert_eq!(signal.wait(&second), Ok("second"));
}

/// Catches an ending that changes what it says, or that tells only the waits made before it:
/// the first `over` stands, and a wait made after it fails at once with it.
#[test]
fn an_ending_is_over_once_and_tells_a_later_wait_at_once() {
    let (_notice, signal) = Signal::<()>::new();
    let ending = Ending::default();
    let other = ending.clone();
    ending.over("the first end");
    other.over("a second end");
    let missed = Missed {
        over: "the first end".to_owned(),
    };
    assert_eq!(signal.wait(&ending), Err(missed));
}

/// Catches a clock party over too soon or never: a party of two sleeps is not over after one
/// (a signal sent then is taken), and is over after the second, saying how many sleeps it saw.
#[test]
fn a_clock_party_is_over_after_its_sleeps_and_not_before() {
    let clock = FakeClock::frozen();
    clock.sleep_secs(1);
    let (notice, signal) = Signal::new();
    let two = clock.sleeps(2);
    let three = clock.sleeps(3);
    clock.sleep_secs(1);
    notice.send("after one sleep");
    assert_eq!(signal.wait(&two), Ok("after one sleep"));
    clock.sleep_secs(1);
    let missed = Missed {
        over: "the clock was slept on 3 times".to_owned(),
    };
    assert_eq!(signal.wait(&two), Err(missed));
    clock.sleep_secs(1);
    let missed = Missed {
        over: "the clock was slept on 4 times".to_owned(),
    };
    assert_eq!(signal.wait(&three), Err(missed));
}

/// Catches a lock wait the test cannot see: a caller of `wait_let_go` is told to the signal by
/// service before it waits, and a lock free at once still tells it. The locks name their hook.
#[test]
fn a_caller_that_waits_for_a_lock_is_signalled_by_service() {
    let locks = ServiceLocks::default();
    let waits = lock_waits(&locks);
    let waiter = {
        let locks = locks.clone();
        let web = ServiceName::new("web").expect("a service name");
        spawn("the waiter", move || locks.wait_let_go(&web))
    };
    assert_eq!(waits.wait(&waiter), Ok("web".to_owned()));
    waiter.join().expect("the waiter");
    assert!(
        format!("{locks:?}").contains("on_wait: WaitHook"),
        "{locks:?}"
    );
}
