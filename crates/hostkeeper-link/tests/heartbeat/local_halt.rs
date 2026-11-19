//! The halt the host holds of a service, reported in every heartbeat's service facts.

use hostkeeper_link::testing::START_UNIX;
use hostkeeper_link::{Feature, HeldHalt, LocalHalt, Reason, Reply};

use crate::{Rig, recorded, service};

fn reason(text: &str) -> Reason {
    Reason::new(text).expect("a reason")
}

fn held(generation: u64, written_unix: Option<u64>) -> HeldHalt {
    HeldHalt {
        reason: Some(reason("disk_tainted")),
        generation,
        written_unix,
    }
}

/// The hold the beat `at` reports for `name`.
fn reported(rig: &Rig, at: usize, name: &str) -> Option<LocalHalt> {
    rig.world.link.beats()[at]
        .services
        .iter()
        .find(|facts| facts.name == service(name))
        .expect("the service is listed")
        .local_halt
        .clone()
}

fn raising(generation: u64, name: &str) -> Reply {
    Reply {
        features: vec![Feature::HaltV1],
        halt_generations: [(service(name), generation)].into_iter().collect(),
        ..Reply::default()
    }
}

/// Proves a service whose host holds a halt is reported with its reason, generation and the
/// seconds since it was written, and that the store is asked once per installed service per beat.
///
/// Catches: a hold left out of the facts, an age taken from the wrong clock, and a store asked
/// for services that are not installed or asked more than once a beat.
#[test]
fn a_held_halt_is_reported_with_its_reason_generation_and_age() {
    let rig = Rig::new("local-halt-held");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "2.0")]);
    rig.world
        .halts
        .halt(&service("x"), held(15, Some(START_UNIX - 600)));
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    assert_eq!(
        reported(&rig, 0, "x"),
        Some(LocalHalt {
            reason: Some(reason("disk_tainted")),
            generation: 15,
            age_secs: Some(600),
        })
    );
    assert_eq!(rig.world.halts.asked(), 2);
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.asked(), 4);
}

/// Proves a service that holds no halt reports none, beside one that does.
///
/// Catches: a hold reported for every service once any holds one.
#[test]
fn a_service_with_no_hold_reports_no_local_halt() {
    let rig = Rig::new("local-halt-none");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "2.0")]);
    rig.world
        .halts
        .halt(&service("x"), held(15, Some(START_UNIX - 600)));
    rig.heartbeater().beat_once();
    assert_eq!(reported(&rig, 0, "y"), None);
    assert!(reported(&rig, 0, "x").is_some());
}

/// Proves a reply's halt generation at or below the held one lifts nothing and the next beat
/// still reports the hold.
///
/// Catches: a stale generation from the coordinator lifting the halt the host holds, or hiding it
/// from the next heartbeat.
#[test]
fn a_reply_generation_below_the_held_one_leaves_the_hold_reported() {
    let rig = Rig::new("local-halt-stale");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world
        .halts
        .halt(&service("x"), held(15, Some(START_UNIX - 600)));
    rig.world.link.answer(Ok(raising(3, "x")));
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.heard(), [("x".to_owned(), None, 3)]);
    assert_eq!(reported(&rig, 1, "x").expect("still held").generation, 15);
}

/// Proves a reply's halt generation above the held one lifts the hold and the next beat reports
/// none.
///
/// Catches: a lifted halt still reported as held.
#[test]
fn a_reply_generation_above_the_held_one_lifts_the_reported_hold() {
    let rig = Rig::new("local-halt-lifted");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world
        .halts
        .halt(&service("x"), held(15, Some(START_UNIX - 600)));
    rig.world.link.answer(Ok(raising(16, "x")));
    let mut heartbeater = rig.heartbeater();
    heartbeater.beat_once();
    assert!(reported(&rig, 0, "x").is_some());
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.heard(), [("x".to_owned(), None, 16)]);
    assert_eq!(reported(&rig, 1, "x"), None);
}

/// Proves a hold written after the clock's now is reported as 0 seconds old.
///
/// Catches: an age that wraps or panics when the write is stamped in the future.
#[test]
fn a_hold_written_in_the_future_is_zero_seconds_old() {
    let rig = Rig::new("local-halt-future");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world
        .halts
        .halt(&service("x"), held(2, Some(START_UNIX + 90)));
    rig.heartbeater().beat_once();
    assert_eq!(reported(&rig, 0, "x").expect("held").age_secs, Some(0));
}

/// Proves a hold whose write time is unknown is reported with no age.
///
/// Catches: an unknown write time reported as age 0 or as the whole clock.
#[test]
fn a_hold_with_no_write_time_reports_no_age() {
    let rig = Rig::new("local-halt-unwritten");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world.halts.halt(&service("x"), held(2, None));
    rig.heartbeater().beat_once();
    let halt = reported(&rig, 0, "x").expect("held");
    assert_eq!(halt.age_secs, None);
    assert_eq!(halt.generation, 2);
}
