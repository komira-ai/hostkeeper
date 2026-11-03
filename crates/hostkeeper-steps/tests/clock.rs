//! The clock: a stamp from another boot measures nothing, and the host's clock answers.

mod support;

use hostkeeper_steps::{Clock, Stamp, SystemClock};
use support::FakeClock;

/// Catches elapsed time carried across a reboot: the monotonic clock restarts, so a stamp of
/// another boot reads as no time having passed rather than a difference of unrelated counters.
#[test]
fn a_stamp_of_another_boot_has_no_elapsed_time() {
    let clock = FakeClock::new();
    let stamp = Stamp::now(&*clock);
    clock.advance(40);
    assert_eq!(stamp.elapsed_secs(&*clock), 40);
    clock.reboot("boot-b");
    clock.advance(1000);
    assert_eq!(stamp.elapsed_secs(&*clock), 0);
}

/// Catches a clock that answers nothing or has no boot id (the record's boot comparison would
/// then treat every boot alike): the host's clock is read and slept on without comparing real
/// time, and its boot id is not empty and is the same on two reads.
#[test]
fn the_system_clock_answers_and_names_the_boot() {
    let clock = SystemClock;
    let _ = clock.mono_secs();
    clock.sleep_secs(0);
    assert!(!clock.boot_id().is_empty());
    assert_eq!(clock.boot_id(), clock.boot_id());
}

/// Catches stamps that do not order or measure: on an injected clock, the elapsed time is the
/// advance, a later stamp is not below an earlier one, and a sleep advances the clock by exactly
/// its argument and is recorded.
#[test]
fn the_injected_clock_orders_stamps_and_measures_elapsed_time() {
    let clock = FakeClock::new();
    let first = Stamp::now(&*clock);
    clock.advance(25);
    let second = Stamp::now(&*clock);
    assert_eq!(first.elapsed_secs(&*clock), 25);
    assert_eq!(second.elapsed_secs(&*clock), 0);
    assert_eq!(second.boot_id, first.boot_id);
    assert!(second.mono_secs >= first.mono_secs);
    assert_eq!(second.mono_secs - first.mono_secs, 25);

    assert!(clock.slept().is_empty());
    clock.sleep_secs(7);
    assert_eq!(clock.slept(), [7]);
    assert_eq!(first.elapsed_secs(&*clock), 32);
    assert_eq!(second.elapsed_secs(&*clock), 7);
}
