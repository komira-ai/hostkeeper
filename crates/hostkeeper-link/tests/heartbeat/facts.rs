//! The facts a heartbeat carries, and the loop's period.

use std::io;
use std::sync::Arc;

use hostkeeper_link::testing::{FakeClock, ScriptedSleeper, verdict};
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, ClockSleeper, Health, Heartbeat, LinkError, Liveness, Observation,
    Observed, Outcome, Platform, RecordedService, ServiceFacts, Sleeper, StopFlag, Tick,
};
use hostkeeper_steps::Clock;

use crate::{Rig, recorded, service, update, version};

/// Catches a heartbeat whose list of services comes from anywhere but the step records (a
/// configuration, a list of file names), that omits an installed service, or that takes a fact
/// from the wrong source: the version from the record, the health and its detail and the
/// observer's detail from the observer.
#[test]
fn heartbeat_lists_installed_services_from_records_and_observer() {
    let rig = Rig::new("facts-list");
    let kept = verdict(&update("o-1", 1, "y", "2.0"), Outcome::Succeeded);
    rig.install(vec![
        recorded("x", "1.0"),
        RecordedService {
            updating: Some(version("2.1")),
            last_result: Some(kept.clone()),
            ..recorded("y", "2.0")
        },
    ]);
    rig.world.observer.set(
        &service("x"),
        Observation {
            liveness: Liveness::Stopped,
            version: Some(version("1.0")),
            health: Health::Unhealthy,
            health_detail: Some("probe refused".to_owned()),
        },
    );
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let expected = Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services: vec![
            ServiceFacts {
                name: service("x"),
                version: version("1.0"),
                health: Health::Unhealthy,
                updating: None,
                last_update: None,
                deferred: None,
                health_detail: Some("probe refused".to_owned()),
                observed: Some(Observed {
                    liveness: Liveness::Stopped,
                    version: Some(version("1.0")),
                }),
            },
            ServiceFacts {
                name: service("y"),
                version: version("2.0"),
                health: Health::Healthy,
                updating: Some(version("2.1")),
                last_update: Some(kept),
                deferred: None,
                health_detail: None,
                observed: Some(Observed {
                    liveness: Liveness::Running,
                    version: None,
                }),
            },
        ],
    };
    assert_eq!(rig.world.link.beats(), vec![expected]);

    // A service whose record is gone is no longer listed; the next one is.
    rig.world.records.set(vec![recorded("y", "2.0")]);
    heartbeater.beat_once();
    let names: Vec<String> = rig.world.link.beats()[1]
        .services
        .iter()
        .map(|facts| facts.name.as_str().to_owned())
        .collect();
    assert_eq!(names, ["y"]);
}

/// Catches a heartbeat sent with no services when the step records cannot be read (the
/// coordinator would count every service as gone), and a failure that is not said.
#[test]
fn records_that_cannot_be_read_send_nothing_and_say_why() {
    let rig = Rig::new("facts-unreadable");
    rig.install(vec![recorded("x", "1.0")]);
    rig.world
        .records
        .fail(Some(io::ErrorKind::PermissionDenied));
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NoFacts);
    assert!(rig.world.link.calls().is_empty());
    assert_eq!(
        rig.world.log.lines(),
        ["error: no heartbeat: the installed services cannot be read: permission denied"]
    );
    rig.world.records.fail(None);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
}

/// Catches a link failure that stops the loop or goes unsaid: it is logged, and the next
/// period beats again.
#[test]
fn a_link_failure_is_logged_and_the_next_period_beats_again() {
    let rig = Rig::new("facts-link-failure");
    rig.world
        .link
        .answer(Err(LinkError::Transport(io::ErrorKind::ConnectionRefused)));
    let mut heartbeater = rig.heartbeater();
    assert_eq!(
        heartbeater.beat_once(),
        BeatOutcome::NotSent(LinkError::Transport(io::ErrorKind::ConnectionRefused))
    );
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(rig.world.link.calls().len(), 2);
    assert_eq!(
        rig.world.log.lines(),
        ["error: the coordinator could not be reached: connection refused"]
    );
}

/// Catches a loop that waits before its first heartbeat, waits other than its period, or does
/// not stop when its sleeper says so.
#[test]
fn run_beats_at_start_and_after_every_period_until_stopped() {
    let rig = Rig::new("facts-run");
    let sleeper = ScriptedSleeper::new(2);
    rig.heartbeater().run(&sleeper);
    assert_eq!(rig.world.link.calls().len(), 3);
    assert_eq!(sleeper.waits(), [15, 15, 15]);
}

/// Catches the production sleeper waiting other than the period on its clock, or not stopping
/// the loop once its flag is set.
#[test]
fn a_clock_sleeper_waits_the_period_on_its_clock_and_stops_when_asked() {
    let clock = Arc::new(FakeClock::new(100, 7));
    let stop = StopFlag::default();
    let sleeper = ClockSleeper::new(clock.clone(), stop.clone());
    assert_eq!(sleeper.sleep(15), Tick::Beat);
    assert_eq!(clock.mono_secs(), 22);
    assert_eq!(clock.boot_id(), "fake-boot");
    assert!(!stop.is_stopped());
    stop.stop();
    assert!(stop.is_stopped());
    assert_eq!(sleeper.sleep(15), Tick::Stop);
    assert_eq!(clock.waits(), [15, 15]);
}
