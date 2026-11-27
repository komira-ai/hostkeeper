//! A daemon that reads its host's hardware health ([`hostkeeper_link::HostHealthSource`]):
//! `host-health-v1` announced, and the health sent, only once the coordinator advertised it, and
//! the records a reply names written handed back to the source.

use hostkeeper_link::testing::World;
use hostkeeper_link::{BeatOutcome, Feature, Reply};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::{Rig, reply};

/// The health the fake source gives: a boot, its marker and one record.
fn health() -> pb::HostHealth {
    pb::HostHealth {
        events: vec![pb::HardwareEvent {
            event_id: "sel:01a4@1790000000".to_owned(),
            severity: pb::HardwareSeverity::Critical.into(),
            ..pb::HardwareEvent::default()
        }],
        boot_id: "b7c1".to_owned(),
        boot_unix: 1_795_662_000,
        clean_previous_shutdown: true,
        ..pb::HostHealth::default()
    }
}

/// Proves the first heartbeat, before any reply, announces no `host-health-v1` and sends no
/// health without asking the source, and the heartbeat after a reply that advertises it
/// announces it and sends exactly what the source gives; a reply that stops advertising it stops
/// both.
///
/// Catches: health sent to a coordinator that never advertised it, or the source's health
/// changed on its way into the heartbeat.
#[test]
fn health_is_sent_only_while_the_coordinator_advertises_it() {
    let rig = Rig::with_world("health-source-sent", World::default().reading_health());
    rig.world.health.set(health());
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::HostHealthV1])));
    rig.world.link.answer(Ok(reply(None, &[])));
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].health, None);
    assert!(!beats[0].features.contains(&Feature::HostHealthV1));
    assert_eq!(beats[1].health, Some(health()));
    assert!(beats[1].features.contains(&Feature::HostHealthV1));
    assert_eq!(beats[2].health, None);
    assert!(!beats[2].features.contains(&Feature::HostHealthV1));
    assert_eq!(rig.world.health.asked(), 1);
}

/// Proves the records a reply names written go to the source, exactly and once per reply, and a
/// reply that names none tells it nothing; they go whether or not that reply advertises the
/// feature, since only a coordinator that wrote them names them.
///
/// Catches: written records left held and sent again for ever, or the source told of an empty
/// list on every heartbeat.
#[test]
fn records_written_go_back_to_the_source() {
    let rig = Rig::with_world("health-source-written", World::default().reading_health());
    let mut heartbeater = rig.heartbeater();
    let ids = vec!["sel:01a4@1790000000".to_owned(), "panic:2b6f".to_owned()];
    rig.world.link.answer(Ok(Reply {
        health_written: ids.clone(),
        ..reply(None, &[Feature::HostHealthV1])
    }));
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::HostHealthV1])));
    rig.world.link.answer(Ok(Reply {
        health_written: vec!["smart:sda".to_owned()],
        ..reply(None, &[])
    }));
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    assert_eq!(
        rig.world.health.written_ids(),
        vec![ids, vec!["smart:sda".to_owned()]]
    );
}

/// Proves a host with no health source neither asks for nor passes on anything when a reply
/// names records written, and keeps beating.
///
/// Catches: a loop that fails on a reply naming records when it has no source.
#[test]
fn a_host_with_no_source_passes_records_written_over() {
    let rig = Rig::new("health-source-none");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(Reply {
        health_written: vec!["sel:1".to_owned()],
        ..reply(None, &[Feature::HostHealthV1])
    }));
    for _ in 0..2 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    assert_eq!(rig.world.health.asked(), 0);
    assert!(rig.world.health.written_ids().is_empty());
}
