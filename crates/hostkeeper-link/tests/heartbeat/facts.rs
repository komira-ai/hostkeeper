//! The facts a heartbeat carries, and the loop's period.

use std::io;
use std::sync::Arc;

use hostkeeper_link::DEFAULT_HEARTBEAT_SECS;
use hostkeeper_link::testing::{FakeClock, ScriptedSleeper};
use hostkeeper_link::{
    ANNOUNCED, Acknowledged, BeatOutcome, ClockSleeper, Health, Heartbeat, LinkError, Liveness,
    Observation, Observed, OrderResult, Outcome, Platform, RecordedService, Reply, ServiceFacts,
    Sleeper, StopFlag, Tick,
};
use hostkeeper_steps::Clock;

use crate::verdict;
use crate::{Rig, recorded, service, update, version};

/// Catches a heartbeat whose list of services comes from anywhere but the step records (a
/// configuration, a list of file names), that omits an installed service, or that takes a fact
/// from the wrong source: the version and the try the record holds from the record (none for a
/// record that holds none), the health and its detail and the observer's detail from the
/// observer.
#[test]
fn heartbeat_lists_installed_services_from_records_and_observer() {
    let rig = Rig::new("facts-list");
    let kept = verdict(&update("o-1", 1, "y", "2.0"), Outcome::Succeeded);
    rig.install(vec![
        recorded("x", "1.0"),
        RecordedService {
            updating: Some(version("2.1")),
            last_result: Some(kept.clone()),
            record_try: Some(1),
            installer: Some("tarball".to_owned()),
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
            drift: None,
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
                record_try: None,
                installer: None,
                drift: None,
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
                record_try: Some(1),
                installer: Some("tarball".to_owned()),
                drift: None,
            },
        ],
        sent_unix: hostkeeper_link::testing::START_UNIX,
        renewal: hostkeeper_link::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
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
/// not stop when its sleeper says so; and a heartbeat whose outcome is not handed on, or handed
/// on for another heartbeat: three heartbeats, one refused by the coordinator between two that
/// were answered, hand on exactly those three outcomes in that order.
#[test]
fn run_beats_at_start_and_after_every_period_until_stopped() {
    let rig = Rig::new("facts-run");
    let refused = LinkError::Transport(io::ErrorKind::ConnectionRefused);
    rig.world.link.answer(Ok(Reply::default()));
    rig.world.link.answer(Err(refused.clone()));
    let sleeper = ScriptedSleeper::new(2);
    let mut seen = Vec::new();
    rig.heartbeater()
        .run(&sleeper, &mut |outcome| seen.push(outcome.clone()));
    assert_eq!(rig.world.link.calls().len(), 3);
    assert_eq!(sleeper.waits(), [15, 15, 15]);
    assert_eq!(
        seen,
        [
            BeatOutcome::Sent,
            BeatOutcome::NotSent(refused),
            BeatOutcome::Sent
        ]
    );
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
    assert_eq!(clock.boot_id().expect("a boot id").as_str(), "fake-boot");
    assert!(!stop.is_stopped());
    stop.stop();
    assert!(stop.is_stopped());
    assert_eq!(sleeper.sleep(15), Tick::Stop);
    assert_eq!(clock.waits(), [15, 15]);
}

/// Catches a service listed again after it left keeping what the loop knew before: its record's
/// new verdict is sent, and a verdict already acknowledged is not sent again.
#[test]
fn a_service_listed_again_takes_its_record_and_keeps_what_was_counted() {
    let rig = Rig::new("facts-return");
    let old = verdict(&update("o-1", 1, "x", "2.0"), Outcome::Succeeded);
    let new = verdict(&update("o-2", 1, "x", "3.0"), Outcome::Failed);
    let with = |result: &OrderResult| RecordedService {
        last_result: Some(result.clone()),
        ..recorded("x", "2.0")
    };
    rig.install(vec![with(&old)]);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(Reply {
        acknowledged: vec![Acknowledged {
            service: service("x"),
            order: old.order.clone(),
        }],
        ..Reply::default()
    }));
    heartbeater.beat_once();
    rig.world.records.set(Vec::new());
    heartbeater.beat_once();
    rig.world.records.set(vec![with(&old)]);
    heartbeater.beat_once();
    rig.world.records.set(Vec::new());
    heartbeater.beat_once();
    rig.world.records.set(vec![with(&new)]);
    heartbeater.beat_once();
    let shown: Vec<Option<OrderResult>> = rig
        .world
        .link
        .beats()
        .iter()
        .map(|beat| {
            beat.services
                .first()
                .and_then(|facts| facts.last_update.clone())
        })
        .collect();
    assert_eq!(shown, [Some(old), None, None, None, Some(new)]);
}

/// Catches step records that name one service twice being sent (the coordinator refuses such a
/// heartbeat, and the two entries would share one verdict): nothing is sent, and the line says
/// which name.
#[test]
fn step_records_that_name_a_service_twice_send_nothing() {
    let rig = Rig::new("facts-twice");
    rig.install(vec![recorded("x", "1.0"), recorded("x", "2.0")]);
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NoFacts);
    assert!(rig.world.link.calls().is_empty());
    assert_eq!(
        rig.world.log.lines(),
        [
            "error: no heartbeat: the installed services cannot be read: the step records name x \
          twice"
        ]
    );
}

/// Catches a heartbeat that says another time than the host's unix clock at the beat, or another
/// certificate end or error than the enrollment loop's, or that sends an error past the wire's
/// bound or an empty one: each beat says the clock of its moment and what the loop's state
/// holds then, the error cut to the bound at a character boundary, and an empty error as none.
#[test]
fn a_heartbeat_says_the_host_clock_and_the_enrollment_loops_word_on_the_certificate() {
    use hostkeeper_link::Renewal;
    use hostkeeper_wire::wire::MAX_RENEW_ERROR;

    let rig = Rig::new("facts-renewal");
    rig.install(Vec::new());
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);

    rig.world.clock.set_unix(1_234_567_890);
    let long = format!("a{}", "é".repeat(200));
    rig.world.renewal.set(Renewal {
        cert_not_after_unix: Some(1_235_000_000),
        last_renew_error: Some(long),
    });
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);

    rig.world.renewal.set(Renewal {
        cert_not_after_unix: Some(1_235_000_000),
        last_renew_error: Some(String::new()),
    });
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);

    let said: Vec<(u64, Renewal)> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| (beat.sent_unix, beat.renewal))
        .collect();
    let clipped = format!("a{}", "é".repeat(127));
    assert_eq!(clipped.len(), MAX_RENEW_ERROR - 1);
    assert_eq!(
        said,
        [
            (hostkeeper_link::testing::START_UNIX, Renewal::default()),
            (
                1_234_567_890,
                Renewal {
                    cert_not_after_unix: Some(1_235_000_000),
                    last_renew_error: Some(clipped),
                }
            ),
            (
                1_234_567_890,
                Renewal {
                    cert_not_after_unix: Some(1_235_000_000),
                    last_renew_error: None,
                }
            ),
        ]
    );
}

/// Catches a heartbeat whose trusted root is read once and kept, read from anywhere but its
/// source, or left out: each beat carries what the source says at that beat, none while it says
/// none.
#[test]
fn a_heartbeat_carries_the_trusted_root_its_source_says_at_each_beat() {
    use hostkeeper_link::{RootReason, TrustedRootFacts};

    let rig = Rig::new("facts-trusted-root");
    rig.install(Vec::new());
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let stale = TrustedRootFacts {
        age_secs: Some(91 * 86_400),
        reason: Some(RootReason::Stale),
    };
    rig.world.trusted_root.set(Some(stale));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let fresh = TrustedRootFacts {
        age_secs: Some(60),
        reason: None,
    };
    rig.world.trusted_root.set(Some(fresh));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);

    let said: Vec<Option<TrustedRootFacts>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.trusted_root)
        .collect();
    assert_eq!(said, [None, Some(stale), Some(fresh)]);
}

/// Proves the installers the loop was given at start go out in every heartbeat it makes, and a
/// loop given none sends none.
///
/// Catches: installers read once and sent in the first heartbeat only, or never copied from
/// the loop's configuration into the heartbeat.
#[test]
fn every_heartbeat_lists_the_installers_the_loop_was_given() {
    use hostkeeper_link::{Heartbeater, LoopConfig};

    let rig = Rig::new("facts-installers");
    rig.install(Vec::new());
    let mut heartbeater = Heartbeater::new(
        LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: vec!["hostkeeper-archive".to_owned()],
            private_repository: false,
        },
        rig.world.ports(),
    );
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let said: Vec<Vec<String>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.installers)
        .collect();
    assert_eq!(said, [["hostkeeper-archive"], ["hostkeeper-archive"]]);

    let plain = Rig::new("facts-no-installers");
    plain.install(Vec::new());
    assert_eq!(plain.heartbeater().beat_once(), BeatOutcome::Sent);
    assert_eq!(plain.world.link.beats()[0].installers, Vec::<String>::new());
}

/// Proves the drift the observer answered for a service is in that service's facts, and a
/// service whose observation has none has none.
///
/// Catches: a drift lost between the observation and the heartbeat, or carried to another
/// service.
#[test]
fn the_drift_the_observer_found_is_in_the_services_facts() {
    use hostkeeper_link::Drift;

    let rig = Rig::new("facts-drift");
    rig.install(vec![recorded("x", "1.0"), recorded("y", "1.0")]);
    let found = Drift {
        found: Some("2.0-1".to_owned()),
        recorded: None,
    };
    rig.world.observer.set(
        &service("x"),
        Observation {
            liveness: Liveness::Running,
            version: Some(version("1.0")),
            health: Health::Healthy,
            health_detail: None,
            drift: Some(found.clone()),
        },
    );
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let beat = rig.world.link.beats().remove(0);
    let drifts: Vec<(&str, Option<Drift>)> = beat
        .services
        .iter()
        .map(|facts| (facts.name.as_str(), facts.drift.clone()))
        .collect();
    assert_eq!(drifts, [("x", Some(found)), ("y", None)]);
}
